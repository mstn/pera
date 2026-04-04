use std::path::{Path, PathBuf};

use pera_core::{SkillManifest, SkillProfileManifest};

use crate::error::SkillProvisionError;
use crate::host::ProjectHost;

#[derive(Debug, Clone, serde::Deserialize)]
pub struct CompiledSkillMeta {
    pub skill_name: String,
    pub skill_version: String,
    pub profile_name: String,
}

pub fn resolve_manifest_path(path: &Path) -> Result<PathBuf, SkillProvisionError> {
    let candidates = [
        path.join("manifest.yaml"),
        path.join("skill.yaml"),
        path.join("skill.yml"),
    ];

    candidates.into_iter().find(|candidate| candidate.exists()).ok_or_else(|| {
        SkillProvisionError::InvalidManifest(format!("no manifest found in {}", path.display()))
    })
}

pub fn load_manifest<H: ProjectHost>(
    host: &H,
    skill_dir: &Path,
) -> Result<(PathBuf, SkillManifest), SkillProvisionError> {
    let manifest_path = resolve_manifest_path(skill_dir)?;
    let manifest_source = host.read_to_string(&manifest_path)?;
    let manifest = serde_yaml::from_str(&manifest_source).map_err(|error| {
        SkillProvisionError::InvalidManifest(format!(
            "invalid manifest {}: {error}",
            manifest_path.display()
        ))
    })?;
    Ok((manifest_path, manifest))
}

pub fn select_profile<'a>(
    manifest: &'a SkillManifest,
    profile_name: Option<&str>,
) -> Result<&'a SkillProfileManifest, SkillProvisionError> {
    match profile_name {
        Some(profile_name) => manifest
            .profiles
            .iter()
            .find(|profile| profile.name == profile_name)
            .ok_or_else(|| {
                SkillProvisionError::InvalidArguments(format!("unknown profile '{profile_name}'"))
            }),
        None => manifest
            .profiles
            .iter()
            .find(|profile| profile.default)
            .or_else(|| manifest.profiles.first())
            .ok_or_else(|| SkillProvisionError::InvalidManifest("manifest has no profiles".to_owned())),
    }
}
