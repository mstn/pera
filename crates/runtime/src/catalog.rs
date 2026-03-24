use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use wasmtime::component::Component;
use wasmtime::Engine;

use pera_canonical::{CatalogSkill, SkillCatalog, SkillMetadata, load_canonical_world_from_wit};
use pera_core::{ActionSkillRef, StoreError};

use crate::capabilities::SqliteCapabilityProvider;

#[derive(Clone)]
pub struct SkillRuntime {
    root: PathBuf,
    catalog: SkillCatalog,
    component_cache: Arc<tokio::sync::Mutex<BTreeMap<String, Arc<Component>>>>,
    sqlite_path_cache: Arc<tokio::sync::Mutex<BTreeMap<String, PathBuf>>>,
}

impl SkillRuntime {
    pub fn new(root: impl Into<PathBuf>, catalog: SkillCatalog) -> Self {
        Self {
            root: root.into(),
            catalog,
            component_cache: Arc::new(tokio::sync::Mutex::new(BTreeMap::new())),
            sqlite_path_cache: Arc::new(tokio::sync::Mutex::new(BTreeMap::new())),
        }
    }

    pub fn catalog(&self) -> &SkillCatalog {
        &self.catalog
    }

    pub fn resolve_skill(&self, skill_ref: &ActionSkillRef) -> Option<&CatalogSkill> {
        self.catalog.resolve_skill(
            &skill_ref.skill_name,
            skill_ref.skill_version.as_ref().map(|version| version.as_str()),
            skill_ref.profile_name.as_deref(),
        )
    }

    pub async fn load_component(
        &self,
        skill_ref: &ActionSkillRef,
        engine: &Engine,
    ) -> Result<Arc<Component>, StoreError> {
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
            return Ok(component);
        }

        let component_bytes = tokio::fs::read(artifact_ref).await.map_err(io_error)?;
        let engine = engine.clone();
        let component = tokio::task::spawn_blocking(move || {
            Component::new(&engine, component_bytes)
                .map(Arc::new)
                .map_err(|error| StoreError::new(error.to_string()))
        })
        .await
        .map_err(join_error)??;

        let mut cache = self.component_cache.lock().await;
        Ok(cache
            .entry(artifact_ref.to_owned())
            .or_insert_with(|| Arc::clone(&component))
            .clone())
    }

    pub async fn sqlite_provider(
        &self,
        skill_ref: &ActionSkillRef,
    ) -> Result<SqliteCapabilityProvider, StoreError> {
        let database_path = self.sqlite_database_path(skill_ref).await?;
        tokio::task::spawn_blocking(move || {
            SqliteCapabilityProvider::new(database_path)
                .map_err(|error| StoreError::new(error.to_string()))
        })
        .await
        .map_err(join_error)?
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
        let cache_key = skill_runtime_key(skill_ref);
        if let Some(path) = self.sqlite_path_cache.lock().await.get(&cache_key).cloned() {
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
        Ok(cache.entry(cache_key).or_insert_with(|| path.clone()).clone())
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
        Ok(SkillRuntime::new(&self.root, catalog))
    }
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
