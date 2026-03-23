use std::error::Error;
use std::fmt::{Display, Formatter};
use std::fs;
use std::path::{Path, PathBuf};

use pera_core::{
    SkillBuildSpec, SkillDescription, SkillManifest, SkillProfileManifest, SkillRuntimeKind,
    SkillVersion,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillBundle {
    pub root: PathBuf,
    pub manifest_path: PathBuf,
    pub manifest: SkillManifest,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedSkillProfile {
    pub skill_name: String,
    pub skill_version: SkillVersion,
    pub profile_name: String,
    pub runtime_kind: SkillRuntimeKind,
    pub instructions_path: Option<PathBuf>,
    pub capabilities: Vec<String>,
    pub artifact_dir: Option<PathBuf>,
    pub build: Option<SkillBuildSpec>,
    pub wasm: Option<LoadedWasmSkillRuntime>,
    pub bundle_root: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedWasmSkillRuntime {
    pub wit_path: PathBuf,
    pub world: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillRegistryError {
    message: String,
}

impl SkillRegistryError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl Display for SkillRegistryError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl Error for SkillRegistryError {}

pub trait SkillRegistry {
    fn list_skills(&self) -> Result<Vec<SkillDescription>, SkillRegistryError>;

    fn load_skill(&self, skill_name: &str) -> Result<SkillBundle, SkillRegistryError>;

    fn load_profile(
        &self,
        skill_name: &str,
        profile_name: Option<&str>,
    ) -> Result<LoadedSkillProfile, SkillRegistryError>;
}

#[derive(Debug, Clone)]
pub struct FileSystemSkillRegistry {
    root: PathBuf,
}

impl FileSystemSkillRegistry {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    fn manifest_candidates(skill_root: &Path) -> [PathBuf; 2] {
        [skill_root.join("skill.yaml"), skill_root.join("skill.yml")]
    }

    fn bundle_root(&self, skill_name: &str) -> PathBuf {
        self.root.join(skill_name)
    }

    fn load_bundle_from_dir(&self, skill_root: &Path) -> Result<SkillBundle, SkillRegistryError> {
        let manifest_path = Self::manifest_candidates(skill_root)
            .into_iter()
            .find(|path| path.exists())
            .ok_or_else(|| {
                SkillRegistryError::new(format!(
                    "no skill manifest found in {}",
                    skill_root.display()
                ))
            })?;

        let source = fs::read_to_string(&manifest_path).map_err(io_error)?;
        let manifest: SkillManifest = serde_yaml::from_str(&source).map_err(yaml_error)?;
        validate_manifest(&manifest)?;

        Ok(SkillBundle {
            root: skill_root.to_path_buf(),
            manifest_path,
            manifest,
        })
    }

    fn select_profile<'a>(
        bundle: &'a SkillBundle,
        profile_name: Option<&str>,
    ) -> Result<&'a SkillProfileManifest, SkillRegistryError> {
        match profile_name {
            Some(profile_name) => bundle
                .manifest
                .profiles
                .iter()
                .find(|profile| profile.name == profile_name)
                .ok_or_else(|| {
                    SkillRegistryError::new(format!(
                        "profile '{}' not found for skill '{}'",
                        profile_name, bundle.manifest.skill.name
                    ))
                }),
            None => {
                if let Some(profile) = bundle.manifest.profiles.iter().find(|profile| profile.default)
                {
                    return Ok(profile);
                }

                bundle
                    .manifest
                    .profiles
                    .first()
                    .ok_or_else(|| SkillRegistryError::new("skill has no profiles"))
            }
        }
    }
}

impl SkillRegistry for FileSystemSkillRegistry {
    fn list_skills(&self) -> Result<Vec<SkillDescription>, SkillRegistryError> {
        if !self.root.exists() {
            return Ok(Vec::new());
        }

        let mut skills = Vec::new();
        for entry in fs::read_dir(&self.root).map_err(io_error)? {
            let entry = entry.map_err(io_error)?;
            if !entry.file_type().map_err(io_error)?.is_dir() {
                continue;
            }

            let bundle = self.load_bundle_from_dir(&entry.path())?;
            skills.push(SkillDescription {
                name: bundle.manifest.skill.name.clone(),
                version: bundle.manifest.skill.version.clone(),
                profile_count: bundle.manifest.profiles.len(),
            });
        }

        skills.sort_by(|left, right| left.name.cmp(&right.name));
        Ok(skills)
    }

    fn load_skill(&self, skill_name: &str) -> Result<SkillBundle, SkillRegistryError> {
        self.load_bundle_from_dir(&self.bundle_root(skill_name))
    }

    fn load_profile(
        &self,
        skill_name: &str,
        profile_name: Option<&str>,
    ) -> Result<LoadedSkillProfile, SkillRegistryError> {
        let bundle = self.load_skill(skill_name)?;
        let profile = Self::select_profile(&bundle, profile_name)?;

        let instructions_path = bundle
            .manifest
            .defaults
            .instructions
            .as_ref()
            .map(|instructions| bundle.root.join(&instructions.source));

        let wasm = profile.runtime.wasm.as_ref().map(|wasm| LoadedWasmSkillRuntime {
            wit_path: bundle.root.join(&wasm.wit.path),
            world: wasm.wit.world.clone(),
        });
        let artifact_dir = profile
            .runtime
            .wasm
            .as_ref()
            .map(|wasm| bundle.root.join(&wasm.artifacts.dir));
        let build = profile
            .runtime
            .wasm
            .as_ref()
            .and_then(|wasm| wasm.build.as_ref())
            .map(|build| SkillBuildSpec {
                tool: build.tool.clone(),
                module: build.module.clone(),
            });

        Ok(LoadedSkillProfile {
            skill_name: bundle.manifest.skill.name.clone(),
            skill_version: bundle.manifest.skill.version.clone(),
            profile_name: profile.name.clone(),
            runtime_kind: profile.runtime.kind.clone(),
            instructions_path,
            capabilities: profile.capabilities.clone(),
            artifact_dir,
            build,
            wasm,
            bundle_root: bundle.root,
        })
    }
}

fn validate_manifest(manifest: &SkillManifest) -> Result<(), SkillRegistryError> {
    if manifest.schema_version != 1 {
        return Err(SkillRegistryError::new(format!(
            "unsupported skill schema version {}",
            manifest.schema_version
        )));
    }

    if manifest.skill.name.trim().is_empty() {
        return Err(SkillRegistryError::new("skill name cannot be empty"));
    }

    if manifest.profiles.is_empty() {
        return Err(SkillRegistryError::new("skill must define at least one profile"));
    }

    let default_count = manifest
        .profiles
        .iter()
        .filter(|profile| profile.default)
        .count();
    if default_count > 1 {
        return Err(SkillRegistryError::new(
            "skill manifest cannot declare more than one default profile",
        ));
    }

    for profile in &manifest.profiles {
        if profile.name.trim().is_empty() {
            return Err(SkillRegistryError::new("profile name cannot be empty"));
        }

        if profile.runtime.kind.as_str() == "wasm-component" && profile.runtime.wasm.is_none() {
            return Err(SkillRegistryError::new(format!(
                "profile '{}' declares wasm-component runtime without wasm config",
                profile.name
            )));
        }
    }

    Ok(())
}

fn io_error(error: std::io::Error) -> SkillRegistryError {
    SkillRegistryError::new(error.to_string())
}

fn yaml_error(error: serde_yaml::Error) -> SkillRegistryError {
    SkillRegistryError::new(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::{FileSystemSkillRegistry, SkillRegistry};
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn loads_default_profile_from_filesystem_bundle() {
        let root = temp_root("skills-default");
        let skill_root = root.join("orchestration");
        fs::create_dir_all(skill_root.join("build/orchestration-default")).unwrap();
        fs::write(skill_root.join("SKILL.md"), "# Orchestration").unwrap();
        fs::write(skill_root.join("world.wit"), "package test:skill;").unwrap();
        fs::write(
            skill_root.join("skill.yaml"),
            r#"
schema_version: 1
skill:
  name: orchestration
  version: 0.1.0
  description: Build workflows.
defaults:
  instructions:
    source: SKILL.md
profiles:
  - name: orchestration-default
    default: true
    runtime:
      kind: wasm-component
      wasm:
        wit:
          path: world.wit
          world: orchestration-default
        artifacts:
          dir: build/orchestration-default
        build:
          tool: componentize-py
          module: app
    capabilities:
      - memory
      - system
"#,
        )
        .unwrap();

        let registry = FileSystemSkillRegistry::new(&root);
        let profile = registry.load_profile("orchestration", None).unwrap();

        assert_eq!(profile.skill_name, "orchestration");
        assert_eq!(profile.profile_name, "orchestration-default");
        assert_eq!(profile.runtime_kind.as_str(), "wasm-component");
        assert_eq!(profile.instructions_path.unwrap(), skill_root.join("SKILL.md"));
        assert_eq!(
            profile.artifact_dir.unwrap(),
            skill_root.join("build/orchestration-default")
        );
        assert_eq!(profile.wasm.unwrap().world, "orchestration-default");

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn lists_installed_skill_bundles() {
        let root = temp_root("skills-list");
        let skill_root = root.join("orchestration");
        fs::create_dir_all(&skill_root).unwrap();
        fs::write(
            skill_root.join("skill.yaml"),
            r#"
schema_version: 1
skill:
  name: orchestration
  version: 0.1.0
  description: Build workflows.
profiles:
  - name: orchestration-default
    default: true
    runtime:
      kind: wasm-component
      wasm:
        wit:
          path: world.wit
          world: orchestration-default
        artifacts:
          dir: build/orchestration-default
"#,
        )
        .unwrap();

        let registry = FileSystemSkillRegistry::new(&root);
        let skills = registry.list_skills().unwrap();

        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name, "orchestration");
        assert_eq!(skills[0].version.as_str(), "0.1.0");

        let _ = fs::remove_dir_all(root);
    }

    fn temp_root(prefix: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("pera-{prefix}-{nanos}"))
    }
}
