use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::UNIX_EPOCH;
use std::time::Instant;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracing::{debug, trace};
use wasmtime::component::{Component, ComponentExportIndex, Instance, Linker, ResourceTable};
use wasmtime::{Cache, Config, Engine, Store};
use wasmtime_wasi::{WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};

use pera_canonical::{CatalogSkill, SkillCatalog, SkillMetadata, load_canonical_world_from_wit};
use pera_core::{ActionId, ActionSkillRef, RunId, StoreError};

use crate::capabilities::{
    build_sqlite_provider, matches_sqlite_import, resolve_sqlite_database_path,
    CapabilityProviderHandle, CapabilityProviderRegistry,
    SqliteCapabilityProvider,
};

#[derive(Clone)]
pub struct SkillRuntime {
    root: PathBuf,
    engine: Engine,
    cache: Cache,
    catalog: SkillCatalog,
    component_cache: Arc<tokio::sync::Mutex<BTreeMap<String, Arc<Component>>>>,
    sqlite_path_cache: Arc<tokio::sync::Mutex<BTreeMap<String, PathBuf>>>,
    warm_instance_cache: Arc<tokio::sync::Mutex<BTreeMap<String, Arc<Mutex<WarmInstance>>>>>,
}

pub(crate) struct WarmInstance {
    pub(crate) store: Store<WasmHostState>,
    pub(crate) instance: Instance,
    pub(crate) function_exports: BTreeMap<String, ComponentExportIndex>,
    pub(crate) invocation_count: u64,
}

pub(crate) struct WasmHostState {
    table: ResourceTable,
    wasi: WasiCtx,
    invocation: InvocationContext,
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
            invocation: InvocationContext::bootstrap(),
        }
    }

    pub(crate) fn begin_invocation(&mut self, invocation: InvocationContext) {
        self.invocation = invocation;
    }

    pub(crate) fn invocation(&self) -> &InvocationContext {
        &self.invocation
    }

    pub(crate) fn record_event(
        &mut self,
        source: InvocationEventSource,
        message: impl Into<String>,
    ) {
        self.invocation.events.push(InvocationEvent {
            source,
            message: message.into(),
        });
    }

    pub(crate) fn fail(
        &mut self,
        source: InvocationErrorSource,
        message: impl Into<String>,
    ) {
        self.invocation.status = InvocationStatus::Failed;
        self.invocation.error = Some(InvocationError {
            source,
            message: message.into(),
        });
    }

    pub(crate) fn finish_invocation_success(&mut self) {
        self.invocation.status = InvocationStatus::Succeeded;
    }

    pub(crate) fn finish_invocation_failure(&mut self) {
        self.invocation.status = InvocationStatus::Failed;
    }
}

#[derive(Debug, Clone)]
pub(crate) struct InvocationContext {
    pub(crate) run_id: RunId,
    pub(crate) action_id: ActionId,
    pub(crate) canonical_action_id: String,
    pub(crate) export_name: String,
    pub(crate) started_at: Instant,
    pub(crate) status: InvocationStatus,
    pub(crate) events: Vec<InvocationEvent>,
    pub(crate) error: Option<InvocationError>,
}

impl InvocationContext {
    pub(crate) fn new(
        run_id: RunId,
        action_id: ActionId,
        canonical_action_id: impl Into<String>,
        export_name: impl Into<String>,
    ) -> Self {
        Self {
            run_id,
            action_id,
            canonical_action_id: canonical_action_id.into(),
            export_name: export_name.into(),
            started_at: Instant::now(),
            status: InvocationStatus::Running,
            events: Vec::new(),
            error: None,
        }
    }

    fn bootstrap() -> Self {
        Self::new(
            RunId::generate(),
            ActionId::generate(),
            "<bootstrap>",
            "<bootstrap>",
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InvocationStatus {
    Running,
    Succeeded,
    Failed,
}

#[derive(Debug, Clone)]
pub(crate) struct InvocationEvent {
    pub(crate) source: InvocationEventSource,
    pub(crate) message: String,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub(crate) enum InvocationEventSource {
    Provider { name: String, operation: String },
    Wasi { operation: String },
    Runtime { operation: String },
    Component,
}

#[derive(Debug, Clone)]
pub(crate) struct InvocationError {
    pub(crate) source: InvocationErrorSource,
    pub(crate) message: String,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub(crate) enum InvocationErrorSource {
    Provider { name: String, operation: String },
    Wasi { operation: String },
    Runtime { operation: String },
    Component,
}

impl WasiView for WasmHostState {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
        }
    }
}

impl SkillRuntime {
    pub fn new(root: impl Into<PathBuf>, catalog: SkillCatalog) -> Result<Self, StoreError> {
        let root = root.into();
        let (engine, cache) = skill_runtime_engine(&root)?;

        Ok(Self {
            root,
            engine,
            cache,
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

    pub async fn precompile_skill(&self, skill_ref: &ActionSkillRef) -> Result<(), StoreError> {
        self.load_component(skill_ref).await.map(|_| ())
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
        let component_metadata = fs::metadata(artifact_ref).map_err(io_error)?;
        let current_inputs = CacheInputSnapshot::from_artifact(
            artifact_ref,
            &component_bytes,
            &component_metadata,
            self.cache.directory(),
        );
        let previous_inputs = read_cache_input_snapshot(self.cache.directory(), artifact_ref);
        debug!(
            skill = %skill_ref.skill_name,
            artifact_ref = %artifact_ref,
            elapsed_ms = read_started_at.elapsed().as_millis(),
            byte_len = component_bytes.len(),
            "component file read completed",
        );
        let engine = self.engine.clone();
        let cache_hits_before = self.cache.cache_hits();
        let cache_misses_before = self.cache.cache_misses();
        let compile_started_at = Instant::now();
        let component = tokio::task::spawn_blocking(move || {
            Component::new(&engine, component_bytes)
                .map(Arc::new)
                .map_err(|error| StoreError::new(error.to_string()))
        })
        .await
        .map_err(join_error)??;
        let cache_hits_after = self.cache.cache_hits();
        let cache_misses_after = self.cache.cache_misses();
        let cache_hit_delta = cache_hits_after.saturating_sub(cache_hits_before);
        let cache_miss_delta = cache_misses_after.saturating_sub(cache_misses_before);
        debug!(
            skill = %skill_ref.skill_name,
            artifact_ref = %artifact_ref,
            elapsed_ms = compile_started_at.elapsed().as_millis(),
            cache_hit_delta,
            cache_miss_delta,
            "Component::new completed",
        );
        log_cache_outcome(
            skill_ref,
            artifact_ref,
            cache_hit_delta,
            cache_miss_delta,
            &current_inputs,
            previous_inputs.as_ref(),
        );
        write_cache_input_snapshot(self.cache.directory(), artifact_ref, &current_inputs)?;

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
        let capability_providers = self.capability_providers(skill_ref).await?;
        let engine = self.engine.clone();
        let skill_for_build = skill.clone();
        let component_for_build = Arc::clone(&component);
        let build_started_at = Instant::now();
        let warm_instance = tokio::task::spawn_blocking(move || {
            build_warm_instance(
                &skill_for_build,
                component_for_build,
                capability_providers,
                &engine,
            )
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

    async fn capability_providers(
        &self,
        skill_ref: &ActionSkillRef,
    ) -> Result<CapabilityProviderRegistry, StoreError> {
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
        let mut providers = CapabilityProviderRegistry::new();

        for capability in &skill.capabilities {
            match capability.as_str() {
                "sqlite" => {
                    let started_at = Instant::now();
                    let cache_key = skill_runtime_key(skill_ref);
                    let database_path =
                        if let Some(path) = self.sqlite_path_cache.lock().await.get(&cache_key).cloned()
                        {
                            trace!(
                                skill = %skill_ref.skill_name,
                                path = %path.display(),
                                elapsed_ms = started_at.elapsed().as_millis(),
                                "sqlite path cache hit",
                            );
                            path
                        } else {
                            let path = resolve_sqlite_database_path(&self.root, skill_ref, skill)?;
                            let mut cache = self.sqlite_path_cache.lock().await;
                            let path = cache.entry(cache_key).or_insert_with(|| path.clone()).clone();
                            trace!(
                                skill = %skill_ref.skill_name,
                                path = %path.display(),
                                elapsed_ms = started_at.elapsed().as_millis(),
                                "sqlite path resolved",
                            );
                            path
                        };
                    let provider: SqliteCapabilityProvider =
                        tokio::task::spawn_blocking(move || build_sqlite_provider(database_path))
                    .await
                    .map_err(join_error)??;
                    trace!(
                        skill = %skill_ref.skill_name,
                        db = %provider.database_path().display(),
                        capability = "sqlite",
                        elapsed_ms = started_at.elapsed().as_millis(),
                        "capability provider ready",
                    );
                    providers.insert(CapabilityProviderHandle::Sqlite(Arc::new(provider)));
                }
                other => {
                    return Err(StoreError::new(format!(
                        "skill '{}' declares unsupported capability '{}'",
                        skill_ref.skill_name, other
                    )));
                }
            }
        }

        Ok(providers)
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

fn skill_runtime_engine(root: &Path) -> Result<(Engine, Cache), StoreError> {
    let mut config = Config::new();
    config.wasm_component_model(true);
    let cache = Cache::new(skill_runtime_cache_config(root)?).map_err(anyhow_error)?;
    config.cache(Some(cache.clone()));
    let engine = Engine::new(&config).map_err(anyhow_error)?;
    Ok((engine, cache))
}

fn build_warm_instance(
    skill: &CatalogSkill,
    component: Arc<Component>,
    capability_providers: CapabilityProviderRegistry,
    engine: &Engine,
) -> Result<WarmInstance, StoreError> {
    let started_at = Instant::now();
    let linker_started_at = Instant::now();
    let mut linker = Linker::new(engine);
    wasmtime_wasi::p2::add_to_linker_sync(&mut linker).map_err(anyhow_error)?;
    link_imports(&mut linker, skill, &capability_providers)?;
    debug!(
        skill = %skill.metadata.skill_name,
        elapsed_ms = linker_started_at.elapsed().as_millis(),
        "linker setup completed",
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
        "instantiate completed",
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
        invocation_count: 0,
    })
}

fn link_imports(
    linker: &mut Linker<WasmHostState>,
    skill: &CatalogSkill,
    capability_providers: &CapabilityProviderRegistry,
) -> Result<(), StoreError> {
    for import in &skill.world.imports {
        if import.functions.is_empty() {
            let _ = linker.root().instance(&import.name).map_err(anyhow_error)?;
            continue;
        }

        if matches_sqlite_import(import) {
            let sqlite = capability_providers.sqlite().ok_or_else(|| {
                StoreError::new(format!(
                    "skill '{}' imports sqlite but does not declare the sqlite capability",
                    skill.metadata.skill_name
                ))
            })?;
            sqlite.link_import(linker, import)?;
            continue;
        }

        return Err(StoreError::new(format!(
            "unsupported component import '{}'",
            import.name
        )));
    }
    Ok(())
}

fn skill_runtime_cache_config(root: &Path) -> Result<wasmtime::CacheConfig, StoreError> {
    let cache_dir = absolute_cache_dir(root.join("cache").join("wasmtime"))?;
    let mut config = wasmtime::CacheConfig::new();
    config.with_directory(cache_dir);
    Ok(config)
}

fn cache_input_metadata_dir(cache_dir: &Path) -> PathBuf {
    cache_dir.join("pera-inputs")
}

fn cache_input_metadata_path(cache_dir: &Path, artifact_ref: &str) -> PathBuf {
    let mut hasher = Sha256::new();
    hasher.update(artifact_ref.as_bytes());
    let digest = hasher.finalize();
    cache_input_metadata_dir(cache_dir).join(format!("{}.json", hex_lower(&digest)))
}

fn read_cache_input_snapshot(cache_dir: &Path, artifact_ref: &str) -> Option<CacheInputSnapshot> {
    let path = cache_input_metadata_path(cache_dir, artifact_ref);
    let bytes = fs::read(path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn write_cache_input_snapshot(
    cache_dir: &Path,
    artifact_ref: &str,
    snapshot: &CacheInputSnapshot,
) -> Result<(), StoreError> {
    let dir = cache_input_metadata_dir(cache_dir);
    fs::create_dir_all(&dir).map_err(io_error)?;
    let path = cache_input_metadata_path(cache_dir, artifact_ref);
    let bytes = serde_json::to_vec_pretty(snapshot).map_err(json_error)?;
    fs::write(path, bytes).map_err(io_error)
}

fn log_cache_outcome(
    skill_ref: &ActionSkillRef,
    artifact_ref: &str,
    cache_hit_delta: usize,
    cache_miss_delta: usize,
    current: &CacheInputSnapshot,
    previous: Option<&CacheInputSnapshot>,
) {
    if cache_hit_delta > 0 {
        debug!(
            skill = %skill_ref.skill_name,
            artifact_ref = %artifact_ref,
            cache_hit_delta,
            cache_miss_delta,
            component_sha256 = %current.component_sha256,
            "wasmtime disk cache hit",
        );
        return;
    }

    if cache_miss_delta > 0 {
        debug!(
            skill = %skill_ref.skill_name,
            artifact_ref = %artifact_ref,
            cache_hit_delta,
            cache_miss_delta,
            inferred_reason = %infer_cache_miss_reason(current, previous),
            component_sha256 = %current.component_sha256,
            "wasmtime disk cache miss",
        );
        return;
    }

    trace!(
        skill = %skill_ref.skill_name,
        artifact_ref = %artifact_ref,
        cache_hit_delta,
        cache_miss_delta,
        component_sha256 = %current.component_sha256,
        "wasmtime disk cache outcome unavailable",
    );
}

fn infer_cache_miss_reason(
    current: &CacheInputSnapshot,
    previous: Option<&CacheInputSnapshot>,
) -> &'static str {
    let Some(previous) = previous else {
        return "no prior cache input snapshot for artifact";
    };

    if previous.component_sha256 != current.component_sha256 {
        return "component bytes changed";
    }
    if previous.byte_len != current.byte_len {
        return "component byte length changed";
    }
    if previous.modified_unix_ms != current.modified_unix_ms {
        return "component mtime changed";
    }
    if previous.cache_dir != current.cache_dir {
        return "cache directory changed";
    }

    "same component bytes but cache namespace likely changed (wasmtime version, compiler settings, platform, or cache eviction)"
}

fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct CacheInputSnapshot {
    component_sha256: String,
    byte_len: usize,
    modified_unix_ms: Option<u128>,
    cache_dir: String,
}

impl CacheInputSnapshot {
    fn from_artifact(
        artifact_ref: &str,
        component_bytes: &[u8],
        metadata: &fs::Metadata,
        cache_dir: &Path,
    ) -> Self {
        let _ = artifact_ref;
        let mut hasher = Sha256::new();
        hasher.update(component_bytes);
        let modified_unix_ms = metadata
            .modified()
            .ok()
            .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
            .map(|duration| duration.as_millis());
        Self {
            component_sha256: hex_lower(&hasher.finalize()),
            byte_len: component_bytes.len(),
            modified_unix_ms,
            cache_dir: cache_dir.display().to_string(),
        }
    }
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
    let manifest_path = resolve_manifest_path(profile_dir)?;
    let manifest_bytes = fs::read(&manifest_path).map_err(io_error)?;
    let manifest: pera_core::SkillManifest =
        serde_yaml::from_slice(&manifest_bytes).map_err(yaml_error)?;
    let profile = manifest
        .profiles
        .iter()
        .find(|profile| profile.name == meta.profile_name)
        .ok_or_else(|| {
            StoreError::new(format!(
                "profile '{}' is not defined in {}",
                meta.profile_name,
                manifest_path.display()
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

    Ok(CatalogSkill {
        metadata,
        world,
        capabilities: profile.capabilities.clone(),
        databases: manifest.defaults.databases.clone(),
    })
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

fn anyhow_error(error: impl std::fmt::Display) -> StoreError {
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
