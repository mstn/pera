use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::anyhow;
use tracing::{debug, trace};
use wasmtime::component::{Component, ComponentExportIndex, Instance, Linker, ResourceTable};
use wasmtime::{Cache, Config, Engine, Store};
use wasmtime_wasi::p2::{IoView, WasiCtx, WasiCtxBuilder, WasiView};

use pera_canonical::{CatalogSkill, SkillCatalog, SkillMetadata, load_canonical_world_from_wit};
use pera_core::{ActionSkillRef, StoreError};

use crate::capabilities::SqliteCapabilityProvider;

#[derive(Clone)]
pub struct SkillRuntime {
    root: PathBuf,
    engine: Engine,
    catalog: SkillCatalog,
    component_cache: Arc<tokio::sync::Mutex<BTreeMap<String, Arc<Component>>>>,
    sqlite_path_cache: Arc<tokio::sync::Mutex<BTreeMap<String, PathBuf>>>,
    warm_instance_cache: Arc<tokio::sync::Mutex<BTreeMap<String, Arc<Mutex<WarmInstance>>>>>,
}

pub(crate) struct WarmInstance {
    pub(crate) store: Store<WasmHostState>,
    pub(crate) instance: Instance,
    pub(crate) function_exports: BTreeMap<String, ComponentExportIndex>,
}

pub(crate) struct WasmHostState {
    table: ResourceTable,
    wasi: WasiCtx,
}

impl WasmHostState {
    fn new() -> Self {
        let wasi = WasiCtxBuilder::new()
            .inherit_stdout()
            .inherit_stderr()
            .build();
        Self {
            table: ResourceTable::new(),
            wasi,
        }
    }
}

impl IoView for WasmHostState {
    fn table(&mut self) -> &mut ResourceTable {
        &mut self.table
    }
}

impl WasiView for WasmHostState {
    fn ctx(&mut self) -> &mut WasiCtx {
        &mut self.wasi
    }
}

impl SkillRuntime {
    pub fn new(root: impl Into<PathBuf>, catalog: SkillCatalog) -> Result<Self, StoreError> {
        let root = root.into();
        let engine = skill_runtime_engine(&root)?;

        Ok(Self {
            root,
            engine,
            catalog,
            component_cache: Arc::new(tokio::sync::Mutex::new(BTreeMap::new())),
            sqlite_path_cache: Arc::new(tokio::sync::Mutex::new(BTreeMap::new())),
            warm_instance_cache: Arc::new(tokio::sync::Mutex::new(BTreeMap::new())),
        })
    }

    pub fn engine(&self) -> &Engine {
        &self.engine
    }

    pub fn catalog(&self) -> &SkillCatalog {
        &self.catalog
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn resolve_skill(&self, skill_ref: &ActionSkillRef) -> Option<&CatalogSkill> {
        self.catalog.resolve_skill(
            &skill_ref.skill_name,
            skill_ref.skill_version.as_ref().map(|version| version.as_str()),
            skill_ref.profile_name.as_deref(),
        )
    }

    pub async fn warm_components(&self) -> Result<(), StoreError> {
        let started_at = Instant::now();
        let skill_refs = self
            .catalog
            .skills()
            .map(|skill| ActionSkillRef {
                skill_name: skill.metadata.skill_name.clone(),
                skill_version: skill
                    .metadata
                    .skill_version
                    .as_ref()
                    .map(|version| pera_core::SkillVersion::new(version.clone())),
                profile_name: skill.metadata.profile_name.clone(),
            })
            .collect::<Vec<_>>();

        for skill_ref in &skill_refs {
            self.load_component(skill_ref).await?;
        }

        debug!(
            skill_count = skill_refs.len(),
            elapsed_ms = started_at.elapsed().as_millis(),
            "runtime component warm-up complete",
        );
        Ok(())
    }

    async fn load_component(
        &self,
        skill_ref: &ActionSkillRef,
    ) -> Result<Arc<Component>, StoreError> {
        let started_at = Instant::now();
        let skill = self.resolve_skill(skill_ref).ok_or_else(|| {
            StoreError::new(format!(
                "skill '{}'{}/{} is not available in the runtime",
                skill_ref.skill_name,
                skill_ref
                    .skill_version
                    .as_ref()
                    .map(|version| format!(" version '{}'", version.as_str()))
                    .unwrap_or_default(),
                skill_ref.profile_name.as_deref().unwrap_or("")
            ))
        })?;
        let artifact_ref = skill.metadata.artifact_ref.as_deref().ok_or_else(|| {
            StoreError::new(format!(
                "skill '{}' has no compiled artifact reference",
                skill_ref.skill_name
            ))
        })?;
        if let Some(component) = self
            .component_cache
            .lock()
            .await
            .get(artifact_ref)
            .cloned()
        {
            trace!(
                skill = %skill_ref.skill_name,
                artifact_ref = %artifact_ref,
                elapsed_ms = started_at.elapsed().as_millis(),
                "component cache hit",
            );
            return Ok(component);
        }

        debug!(
            skill = %skill_ref.skill_name,
            artifact_ref = %artifact_ref,
            "component cache miss",
        );
        let read_started_at = Instant::now();
        let component_bytes = tokio::fs::read(artifact_ref).await.map_err(io_error)?;
        debug!(
            skill = %skill_ref.skill_name,
            artifact_ref = %artifact_ref,
            elapsed_ms = read_started_at.elapsed().as_millis(),
            byte_len = component_bytes.len(),
            "component bytes loaded",
        );
        let engine = self.engine.clone();
        let compile_started_at = Instant::now();
        let component = tokio::task::spawn_blocking(move || {
            Component::new(&engine, component_bytes)
                .map(Arc::new)
                .map_err(|error| StoreError::new(error.to_string()))
        })
        .await
        .map_err(join_error)??;
        debug!(
            skill = %skill_ref.skill_name,
            artifact_ref = %artifact_ref,
            elapsed_ms = compile_started_at.elapsed().as_millis(),
            "component compiled",
        );

        let mut cache = self.component_cache.lock().await;
        let component = cache
            .entry(artifact_ref.to_owned())
            .or_insert_with(|| Arc::clone(&component))
            .clone();
        trace!(
            skill = %skill_ref.skill_name,
            artifact_ref = %artifact_ref,
            elapsed_ms = started_at.elapsed().as_millis(),
            "component ready",
        );
        Ok(component)
    }

    pub async fn sqlite_provider(
        &self,
        skill_ref: &ActionSkillRef,
    ) -> Result<SqliteCapabilityProvider, StoreError> {
        let started_at = Instant::now();
        let database_path = self.sqlite_database_path(skill_ref).await?;
        let provider = tokio::task::spawn_blocking(move || {
            SqliteCapabilityProvider::new(database_path)
                .map_err(|error| StoreError::new(error.to_string()))
        })
        .await
        .map_err(join_error)??;
        trace!(
            skill = %skill_ref.skill_name,
            db = %provider.database_path().display(),
            elapsed_ms = started_at.elapsed().as_millis(),
            "sqlite provider ready",
        );
        Ok(provider)
    }

    pub(crate) async fn warm_instance(
        &self,
        skill_ref: &ActionSkillRef,
    ) -> Result<Arc<Mutex<WarmInstance>>, StoreError> {
        let started_at = Instant::now();
        let cache_key = skill_runtime_key(skill_ref);
        if let Some(instance) = self
            .warm_instance_cache
            .lock()
            .await
            .get(&cache_key)
            .cloned()
        {
            trace!(
                skill = %skill_ref.skill_name,
                elapsed_ms = started_at.elapsed().as_millis(),
                "warm instance cache hit",
            );
            return Ok(instance);
        }

        debug!(
            skill = %skill_ref.skill_name,
            "warm instance cache miss",
        );
        let component = self.load_component(skill_ref).await?;
        let skill = self.resolve_skill(skill_ref).cloned().ok_or_else(|| {
            StoreError::new(format!(
                "skill '{}'{}/{} is not available in the catalog",
                skill_ref.skill_name,
                skill_ref
                    .skill_version
                    .as_ref()
                    .map(|version| format!(" version '{}'", version.as_str()))
                    .unwrap_or_default(),
                skill_ref.profile_name.as_deref().unwrap_or("")
            ))
        })?;
        let sqlite_provider = self.sqlite_provider(skill_ref).await?;
        let engine = self.engine.clone();
        let skill_for_build = skill.clone();
        let component_for_build = Arc::clone(&component);
        let build_started_at = Instant::now();
        let warm_instance = tokio::task::spawn_blocking(move || {
            build_warm_instance(&skill_for_build, component_for_build, sqlite_provider, &engine)
        })
        .await
        .map_err(join_error)??;
        debug!(
            skill = %skill_ref.skill_name,
            elapsed_ms = build_started_at.elapsed().as_millis(),
            "warm instance built",
        );

        let warm_instance = Arc::new(Mutex::new(warm_instance));
        let mut cache = self.warm_instance_cache.lock().await;
        let warm_instance = cache
            .entry(cache_key)
            .or_insert_with(|| Arc::clone(&warm_instance))
            .clone();
        trace!(
            skill = %skill_ref.skill_name,
            elapsed_ms = started_at.elapsed().as_millis(),
            "warm instance ready",
        );
        Ok(warm_instance)
    }

    pub async fn evict_warm_instance(&self, skill_ref: &ActionSkillRef) {
        self.warm_instance_cache
            .lock()
            .await
            .remove(&skill_runtime_key(skill_ref));
    }

    fn profile_dir(&self, skill_ref: &ActionSkillRef) -> Result<PathBuf, StoreError> {
        let skill_version = skill_ref
            .skill_version
            .as_ref()
            .map(|version| version.as_str())
            .ok_or_else(|| {
                StoreError::new(format!(
                    "skill '{}' is missing a version",
                    skill_ref.skill_name
                ))
            })?;
        let profile_name = skill_ref.profile_name.as_deref().ok_or_else(|| {
            StoreError::new(format!(
                "skill '{}' is missing a profile name",
                skill_ref.skill_name
            ))
        })?;
        Ok(self
            .root
            .join("catalog")
            .join("skills")
            .join(&skill_ref.skill_name)
            .join(skill_version)
            .join(profile_name))
    }

    async fn sqlite_database_path(&self, skill_ref: &ActionSkillRef) -> Result<PathBuf, StoreError> {
        let started_at = Instant::now();
        let cache_key = skill_runtime_key(skill_ref);
        if let Some(path) = self.sqlite_path_cache.lock().await.get(&cache_key).cloned() {
            trace!(
                skill = %skill_ref.skill_name,
                path = %path.display(),
                elapsed_ms = started_at.elapsed().as_millis(),
                "sqlite path cache hit",
            );
            return Ok(path);
        }

        let profile_dir = self.profile_dir(skill_ref)?;
        let manifest_path = resolve_manifest_path(&profile_dir)?;
        let manifest_bytes = tokio::fs::read(&manifest_path).await.map_err(io_error)?;
        let manifest: pera_core::SkillManifest =
            serde_yaml::from_slice(&manifest_bytes).map_err(yaml_error)?;
        let sqlite_databases = manifest
            .defaults
            .databases
            .iter()
            .filter(|database| database.engine == "sqlite")
            .collect::<Vec<_>>();
        let database = match sqlite_databases.as_slice() {
            [database] => *database,
            [] => {
                return Err(StoreError::new(format!(
                    "skill '{}' does not define a sqlite database",
                    skill_ref.skill_name
                )))
            }
            _ => {
                return Err(StoreError::new(format!(
                    "skill '{}' defines multiple sqlite databases; capability mapping is ambiguous",
                    skill_ref.skill_name
                )))
            }
        };
        let skill_version = skill_ref
            .skill_version
            .as_ref()
            .map(|version| version.as_str())
            .ok_or_else(|| {
                StoreError::new(format!(
                    "skill '{}' is missing a version",
                    skill_ref.skill_name
                ))
            })?;
        let profile_name = skill_ref.profile_name.as_deref().ok_or_else(|| {
            StoreError::new(format!(
                "skill '{}' is missing a profile name",
                skill_ref.skill_name
            ))
        })?;
        let path = self
            .root
            .join("state")
            .join("skills")
            .join(&skill_ref.skill_name)
            .join(skill_version)
            .join(profile_name)
            .join("databases")
            .join(format!("{}.sqlite", database.name));

        let mut cache = self.sqlite_path_cache.lock().await;
        let path = cache.entry(cache_key).or_insert_with(|| path.clone()).clone();
        trace!(
            skill = %skill_ref.skill_name,
            path = %path.display(),
            elapsed_ms = started_at.elapsed().as_millis(),
            "sqlite path resolved",
        );
        Ok(path)
    }
}

#[derive(Debug, Clone)]
pub struct FileSystemSkillCatalogLoader {
    root: PathBuf,
}

impl FileSystemSkillCatalogLoader {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn load(&self) -> Result<SkillCatalog, StoreError> {
        let skills_dir = self.root.join("catalog").join("skills");
        if !skills_dir.exists() {
            return SkillCatalog::from_skills(Vec::new())
                .map_err(|error| StoreError::new(error.to_string()));
        }

        let mut skills = Vec::new();
        for skill_entry in read_dir_sorted(&skills_dir)? {
            if !skill_entry.file_type().map_err(io_error)?.is_dir() {
                continue;
            }
            let skill_name = skill_entry.file_name().to_string_lossy().into_owned();
            for version_entry in read_dir_sorted(skill_entry.path())? {
                if !version_entry.file_type().map_err(io_error)?.is_dir() {
                    continue;
                }
                let skill_version = version_entry.file_name().to_string_lossy().into_owned();
                for profile_entry in read_dir_sorted(version_entry.path())? {
                    if !profile_entry.file_type().map_err(io_error)?.is_dir() {
                        continue;
                    }
                    skills.push(load_catalog_skill(
                        &skill_name,
                        &skill_version,
                        &profile_entry.path(),
                    )?);
                }
            }
        }

        SkillCatalog::from_skills(skills).map_err(|error| StoreError::new(error.to_string()))
    }
}

#[derive(Debug, Clone)]
pub struct FileSystemSkillRuntimeLoader {
    root: PathBuf,
}

impl FileSystemSkillRuntimeLoader {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn load(&self) -> Result<SkillRuntime, StoreError> {
        let catalog = FileSystemSkillCatalogLoader::new(&self.root).load()?;
        SkillRuntime::new(&self.root, catalog)
    }
}

fn skill_runtime_engine(root: &Path) -> Result<Engine, StoreError> {
    let mut config = Config::new();
    config.wasm_component_model(true);
    let cache = Cache::new(skill_runtime_cache_config(root)?).map_err(anyhow_error)?;
    config.cache(Some(cache));
    Engine::new(&config).map_err(anyhow_error)
}

fn build_warm_instance(
    skill: &CatalogSkill,
    component: Arc<Component>,
    sqlite_provider: SqliteCapabilityProvider,
    engine: &Engine,
) -> Result<WarmInstance, StoreError> {
    let started_at = Instant::now();
    let linker_started_at = Instant::now();
    let mut linker = Linker::new(engine);
    wasmtime_wasi::p2::add_to_linker_sync(&mut linker).map_err(anyhow_error)?;
    link_imports(&mut linker, skill, Arc::new(sqlite_provider))?;
    debug!(
        skill = %skill.metadata.skill_name,
        elapsed_ms = linker_started_at.elapsed().as_millis(),
        "wasmtime linker configured",
    );
    let store_started_at = Instant::now();
    let mut store = Store::new(engine, WasmHostState::new());
    trace!(
        skill = %skill.metadata.skill_name,
        elapsed_ms = store_started_at.elapsed().as_millis(),
        "wasmtime store created",
    );
    let instantiate_started_at = Instant::now();
    let instance = linker
        .instantiate(&mut store, &component)
        .map_err(anyhow_error)?;
    debug!(
        skill = %skill.metadata.skill_name,
        elapsed_ms = instantiate_started_at.elapsed().as_millis(),
        "wasmtime component instantiated",
    );
    let export_started_at = Instant::now();
    let mut function_exports = BTreeMap::new();
    for export_interface in &skill.world.exports {
        let instance_export = component
            .get_export_index(None, &export_interface.name)
            .ok_or_else(|| {
                StoreError::new(format!(
                    "component interface export '{}' was not found",
                    export_interface.name
                ))
            })?;
        for function in &export_interface.functions {
            let function_export = component
                .get_export_index(Some(&instance_export), &function.name)
                .ok_or_else(|| {
                    StoreError::new(format!(
                        "component export '{}.{}' was not found",
                        export_interface.name, function.name
                    ))
                })?;
            function_exports.insert(function.name.clone(), function_export);
        }
    }
    trace!(
        skill = %skill.metadata.skill_name,
        export_count = function_exports.len(),
        elapsed_ms = export_started_at.elapsed().as_millis(),
        "wasmtime exports indexed",
    );
    debug!(
        skill = %skill.metadata.skill_name,
        elapsed_ms = started_at.elapsed().as_millis(),
        "warm instance lifecycle complete",
    );
    Ok(WarmInstance {
        store,
        instance,
        function_exports,
    })
}

fn link_imports(
    linker: &mut Linker<WasmHostState>,
    skill: &CatalogSkill,
    sqlite_provider: Arc<SqliteCapabilityProvider>,
) -> Result<(), StoreError> {
    for import in &skill.world.imports {
        if import.functions.is_empty() {
            let _ = linker.root().instance(&import.name).map_err(anyhow_error)?;
            continue;
        }

        if is_sqlite_import(import) {
            let sqlite = Arc::clone(&sqlite_provider);
            linker
                .root()
                .instance(&import.name)
                .and_then(|mut instance| {
                    let sqlite = Arc::clone(&sqlite);
                    let import_name = import.name.clone();
                    instance.func_wrap(
                        "execute",
                        move |_store,
                              (sql, params_json): (String, Option<String>)|
                              -> Result<(String,), anyhow::Error> {
                            trace!(
                                import = %import_name,
                                db = %sqlite.database_path().display(),
                                sql = ?sql,
                                params_json = ?params_json,
                                "sqlite import call",
                            );
                            let result = sqlite.execute(&sql, params_json.as_deref()).map_err(
                                |error| {
                                    tracing::error!(
                                        import = %import_name,
                                        db = %sqlite.database_path().display(),
                                        sql = ?sql,
                                        params_json = ?params_json,
                                        error = %error,
                                        "sqlite import error",
                                    );
                                    anyhow!(error.to_string())
                                },
                            )?;
                            trace!(
                                import = %import_name,
                                db = %sqlite.database_path().display(),
                                result_json = %result,
                                "sqlite import ok",
                            );
                            Ok((result,))
                        },
                    )
                })
                .map_err(anyhow_error)?;
            continue;
        }

        return Err(StoreError::new(format!(
            "unsupported component import '{}'",
            import.name
        )));
    }
    Ok(())
}

fn is_sqlite_import(import: &pera_canonical::CanonicalInterface) -> bool {
    import
        .functions
        .iter()
        .any(|function| function.name == "execute")
        && import.name.contains("sqlite")
}

fn skill_runtime_cache_config(root: &Path) -> Result<wasmtime::CacheConfig, StoreError> {
    let cache_dir = absolute_cache_dir(root.join("cache").join("wasmtime"))?;
    let mut config = wasmtime::CacheConfig::new();
    config.with_directory(cache_dir);
    Ok(config)
}

fn absolute_cache_dir(path: PathBuf) -> Result<PathBuf, StoreError> {
    if path.is_absolute() {
        return Ok(path);
    }
    let cwd = std::env::current_dir().map_err(io_error)?;
    Ok(cwd.join(path))
}

fn load_catalog_skill(
    skill_name: &str,
    skill_version: &str,
    profile_dir: &Path,
) -> Result<CatalogSkill, StoreError> {
    let meta_path = profile_dir.join("meta.json");
    let meta_bytes = fs::read(&meta_path).map_err(io_error)?;
    let meta: CompiledSkillMeta = serde_json::from_slice(&meta_bytes).map_err(json_error)?;

    let world_path = profile_dir.join("world.wit");
    let world =
        load_canonical_world_from_wit(&world_path, &meta.runtime.world).map_err(|error| {
            StoreError::new(format!(
                "failed to load canonical world from {}: {error}",
                world_path.display()
            ))
        })?;

    let mut metadata = SkillMetadata::new(skill_name.to_owned(), meta.runtime.world.clone());
    metadata.skill_version = Some(skill_version.to_owned());
    metadata.profile_name = Some(meta.profile_name.clone());
    metadata.runtime_kind = Some(meta.runtime.kind.clone());
    metadata.artifact_ref = Some(
        profile_dir
            .join(&meta.runtime.artifact)
            .display()
            .to_string(),
    );

    Ok(CatalogSkill { metadata, world })
}

fn resolve_manifest_path(skill_dir: &Path) -> Result<PathBuf, StoreError> {
    for candidate in ["manifest.yaml", "skill.yaml", "skill.yml"] {
        let path = skill_dir.join(candidate);
        if path.exists() {
            return Ok(path);
        }
    }

    Err(StoreError::new(format!(
        "no manifest found in {}",
        skill_dir.display()
    )))
}

fn read_dir_sorted(path: impl AsRef<Path>) -> Result<Vec<fs::DirEntry>, StoreError> {
    let mut entries = fs::read_dir(path)
        .map_err(io_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(io_error)?;
    entries.sort_by_key(|entry| entry.file_name());
    Ok(entries)
}

fn io_error(error: std::io::Error) -> StoreError {
    StoreError::new(error.to_string())
}

fn json_error(error: serde_json::Error) -> StoreError {
    StoreError::new(error.to_string())
}

fn yaml_error(error: serde_yaml::Error) -> StoreError {
    StoreError::new(error.to_string())
}

fn anyhow_error(error: anyhow::Error) -> StoreError {
    StoreError::new(error.to_string())
}

fn join_error(error: tokio::task::JoinError) -> StoreError {
    StoreError::new(error.to_string())
}

fn skill_runtime_key(skill_ref: &ActionSkillRef) -> String {
    format!(
        "{}::{}::{}",
        skill_ref.skill_name,
        skill_ref
            .skill_version
            .as_ref()
            .map(|version| version.as_str())
            .unwrap_or_default(),
        skill_ref.profile_name.as_deref().unwrap_or_default()
    )
}

#[derive(Debug, serde::Deserialize)]
struct CompiledSkillMeta {
    profile_name: String,
    runtime: CompiledSkillRuntimeMeta,
}

#[derive(Debug, serde::Deserialize)]
struct CompiledSkillRuntimeMeta {
    kind: String,
    world: String,
    artifact: String,
}
