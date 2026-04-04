use std::path::{Path, PathBuf};

use pera_core::{ActionSkillRef, SkillManifest, SkillVersion};
use pera_runtime::{FileSystemLayout, FileSystemSkillRuntimeLoader};

use crate::componentizer::Componentizer;
use crate::error::SkillProvisionError;
use crate::host::ProjectHost;
use crate::manifest::{CompiledSkillMeta, load_manifest, select_profile};

#[derive(Debug, Clone)]
pub struct ProjectLayout {
    pub root: PathBuf,
    pub catalog_dir: PathBuf,
    pub cache_dir: PathBuf,
}

#[derive(Debug, Clone)]
pub struct CompiledSkill {
    pub skill_name: String,
    pub skill_version: String,
    pub profile_name: String,
    pub compiled_dir: PathBuf,
    pub compiled_now: bool,
}

#[derive(Debug, Clone)]
pub struct InstalledSkill {
    pub compiled: CompiledSkill,
    pub catalog_dir: PathBuf,
    pub uploaded_now: bool,
}

#[derive(Debug, Clone)]
pub struct SkillProvisioner<H, C> {
    host: H,
    componentizer: C,
}

impl<H, C> SkillProvisioner<H, C>
where
    H: ProjectHost,
    C: Componentizer,
{
    pub fn new(host: H, componentizer: C) -> Self {
        Self { host, componentizer }
    }

    pub fn ensure_project_layout(&self, root: &Path) -> Result<ProjectLayout, SkillProvisionError> {
        self.host.create_dir_all(root)?;
        let root = self.host.canonicalize(root)?;
        let _ = FileSystemLayout::new(&root)
            .map_err(|error| SkillProvisionError::Runtime(error.to_string()))?;
        let catalog_dir = root.join("catalog").join("skills");
        let cache_dir = root.join("cache").join("wasmtime");
        self.host.create_dir_all(&catalog_dir)?;
        self.host.create_dir_all(&cache_dir)?;
        Ok(ProjectLayout {
            root,
            catalog_dir,
            cache_dir,
        })
    }

    pub fn compile_skill(
        &self,
        skill_dir: &Path,
        profile_name: Option<&str>,
    ) -> Result<CompiledSkill, SkillProvisionError> {
        let skill_dir = self.host.canonicalize(skill_dir)?;
        let (manifest_path, manifest) = load_manifest(&self.host, &skill_dir)?;
        let profile = select_profile(&manifest, profile_name)?;
        let wasm = profile.runtime.wasm.as_ref().ok_or_else(|| {
            SkillProvisionError::InvalidArguments(
                "only wasm-component profiles are supported".to_owned(),
            )
        })?;

        let artifact_dir = skill_dir.join(&wasm.artifacts.dir);
        let component_path = artifact_dir.join("component.wasm");
        let meta_path = artifact_dir.join("meta.json");
        let compiled_now = !(self.host.exists(&component_path) && self.host.exists(&meta_path));

        if compiled_now {
            self.host.create_dir_all(&artifact_dir)?;
            let build = wasm.build.as_ref().ok_or_else(|| {
                SkillProvisionError::InvalidArguments(
                    "wasm build configuration is required".to_owned(),
                )
            })?;
            self.componentizer.componentize(
                &skill_dir,
                &skill_dir.join(&wasm.wit.path),
                &wasm.wit.world,
                &build.module,
                &component_path,
            )?;

            self.host.copy_file(
                &manifest_path,
                &artifact_dir.join(
                    manifest_path.file_name().ok_or_else(|| {
                        SkillProvisionError::Internal(
                            "manifest path is missing a file name".to_owned(),
                        )
                    })?,
                ),
            )?;
            copy_skill_assets(&self.host, &skill_dir, &artifact_dir, &manifest, profile.name.as_str(), &wasm.wit.path)?;

            let meta = serde_json::json!({
                "schema_version": manifest.schema_version,
                "skill_name": manifest.skill.name,
                "skill_version": manifest.skill.version.as_str(),
                "profile_name": profile.name,
                "runtime": {
                    "kind": profile.runtime.kind.as_str(),
                    "world": wasm.wit.world,
                    "artifact": "component.wasm"
                }
            });
            let meta_bytes = serde_json::to_vec_pretty(&meta).map_err(|error| {
                SkillProvisionError::Internal(format!("failed to serialize meta.json: {error}"))
            })?;
            self.host.write(&meta_path, &meta_bytes)?;
        }

        Ok(CompiledSkill {
            skill_name: manifest.skill.name.clone(),
            skill_version: manifest.skill.version.as_str().to_owned(),
            profile_name: profile.name.clone(),
            compiled_dir: artifact_dir,
            compiled_now,
        })
    }

    pub fn install_compiled_skill(
        &self,
        compiled_dir: &Path,
        project_root: &Path,
    ) -> Result<InstalledSkill, SkillProvisionError> {
        let project = self.ensure_project_layout(project_root)?;
        let compiled_dir = self.host.canonicalize(compiled_dir)?;
        let meta_path = compiled_dir.join("meta.json");
        let meta_source = self.host.read_to_string(&meta_path)?;
        let meta: CompiledSkillMeta = serde_json::from_str(&meta_source).map_err(|error| {
            SkillProvisionError::InvalidManifest(format!(
                "invalid meta.json in {}: {error}",
                meta_path.display()
            ))
        })?;
        let catalog_dir = project
            .catalog_dir
            .join(&meta.skill_name)
            .join(&meta.skill_version)
            .join(&meta.profile_name);
        let uploaded_now = sync_compiled_skill(&self.host, &compiled_dir, &catalog_dir)?;

        Ok(InstalledSkill {
            compiled: CompiledSkill {
                skill_name: meta.skill_name,
                skill_version: meta.skill_version,
                profile_name: meta.profile_name,
                compiled_dir,
                compiled_now: false,
            },
            catalog_dir,
            uploaded_now,
        })
    }

    pub async fn precompile_installed_skill(
        &self,
        project_root: &Path,
        installed: &InstalledSkill,
    ) -> Result<(), SkillProvisionError> {
        let root = self.host.canonicalize(project_root)?;
        let skill_runtime = FileSystemSkillRuntimeLoader::new(&root)
            .load()
            .map_err(|error| SkillProvisionError::Runtime(error.to_string()))?;
        skill_runtime
            .precompile_skill(&ActionSkillRef {
                skill_name: installed.compiled.skill_name.clone(),
                skill_version: Some(SkillVersion::new(installed.compiled.skill_version.clone())),
                profile_name: Some(installed.compiled.profile_name.clone()),
            })
            .await
            .map_err(|error| SkillProvisionError::Runtime(error.to_string()))
    }

    pub async fn ensure_catalog_skill(
        &self,
        skill_dir: &Path,
        profile_name: Option<&str>,
        project_root: &Path,
    ) -> Result<InstalledSkill, SkillProvisionError> {
        let compiled = self.compile_skill(skill_dir, profile_name)?;
        let installed = self.install_compiled_skill(&compiled.compiled_dir, project_root)?;
        self.precompile_installed_skill(project_root, &installed).await?;
        Ok(InstalledSkill {
            compiled: CompiledSkill {
                compiled_now: compiled.compiled_now,
                ..installed.compiled.clone()
            },
            ..installed
        })
    }
}

fn copy_skill_assets<H: ProjectHost>(
    host: &H,
    skill_dir: &Path,
    artifact_dir: &Path,
    manifest: &SkillManifest,
    _profile_name: &str,
    wit_path: &str,
) -> Result<(), SkillProvisionError> {
    if let Some(instructions) = &manifest.defaults.instructions {
        copy_path_relative(host, skill_dir, artifact_dir, &instructions.source)?;
    }
    copy_path_relative(host, skill_dir, artifact_dir, wit_path)?;
    for database in &manifest.defaults.databases {
        if let Some(migrations) = &database.migrations {
            copy_path_relative(host, skill_dir, artifact_dir, &migrations.dir)?;
        }
        if let Some(seeds) = &database.seeds {
            copy_path_relative(host, skill_dir, artifact_dir, &seeds.dir)?;
        }
    }
    Ok(())
}

fn copy_path_relative<H: ProjectHost>(
    host: &H,
    source_root: &Path,
    target_root: &Path,
    relative: &str,
) -> Result<(), SkillProvisionError> {
    let source = source_root.join(relative);
    let target = target_root.join(relative);
    if host.exists(&source) {
        let entries = host.read_dir(&source);
        if entries.is_ok() {
            return copy_dir_recursive(host, &source, &target);
        }
    }
    host.copy_file(&source, &target)
}

fn copy_dir_recursive<H: ProjectHost>(
    host: &H,
    source: &Path,
    target: &Path,
) -> Result<(), SkillProvisionError> {
    host.create_dir_all(target)?;
    for entry in host.read_dir(source)? {
        let target_path = target.join(&entry.file_name);
        if entry.is_dir {
            copy_dir_recursive(host, &entry.path, &target_path)?;
        } else {
            host.copy_file(&entry.path, &target_path)?;
        }
    }
    Ok(())
}

fn sync_compiled_skill<H: ProjectHost>(
    host: &H,
    compiled_dir: &Path,
    catalog_dir: &Path,
) -> Result<bool, SkillProvisionError> {
    if directories_equivalent(host, compiled_dir, catalog_dir)? {
        return Ok(false);
    }
    if host.exists(catalog_dir) {
        host.remove_dir_all(catalog_dir)?;
    }
    copy_dir_recursive(host, compiled_dir, catalog_dir)?;
    Ok(true)
}

fn directories_equivalent<H: ProjectHost>(
    host: &H,
    left: &Path,
    right: &Path,
) -> Result<bool, SkillProvisionError> {
    if !(host.exists(left) && host.exists(right)) {
        return Ok(false);
    }
    let left_component = left.join("component.wasm");
    let right_component = right.join("component.wasm");
    let left_meta = left.join("meta.json");
    let right_meta = right.join("meta.json");
    if !(host.exists(&left_component)
        && host.exists(&right_component)
        && host.exists(&left_meta)
        && host.exists(&right_meta))
    {
        return Ok(false);
    }
    Ok(host.read(&left_component)? == host.read(&right_component)?
        && host.read(&left_meta)? == host.read(&right_meta)?)
}
