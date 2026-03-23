use std::fs;
use std::path::{Path, PathBuf};

use pera_canonical::{CatalogSkill, SkillCatalog, SkillMetadata, load_canonical_world_from_wit};
use pera_core::StoreError;

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
