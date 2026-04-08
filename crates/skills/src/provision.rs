use std::path::{Path, PathBuf};

use pera_core::{ActionSkillRef, SkillManifest, SkillVersion};
use pera_runtime::{FileSystemLayout, FileSystemSkillRuntimeLoader, SqliteCapabilityProvider};

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
        let skill_ref = ActionSkillRef {
            skill_name: installed.compiled.skill_name.clone(),
            skill_version: Some(SkillVersion::new(installed.compiled.skill_version.clone())),
            profile_name: Some(installed.compiled.profile_name.clone()),
        };
        let skill_runtime = FileSystemSkillRuntimeLoader::new(&root)
            .load_only(&skill_ref)
            .map_err(|error| SkillProvisionError::Runtime(error.to_string()))?;
        skill_runtime
            .precompile_skill(&skill_ref)
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

    pub fn reset_installed_skill_state(
        &self,
        project_root: &Path,
        installed: &InstalledSkill,
        selected_seed: Option<&str>,
    ) -> Result<(), SkillProvisionError> {
        let project_root = self.host.canonicalize(project_root)?;
        let (manifest_path, manifest) = load_manifest(&self.host, &installed.catalog_dir)?;
        let profile = manifest
            .profiles
            .iter()
            .find(|profile| profile.name == installed.compiled.profile_name)
            .ok_or_else(|| {
                SkillProvisionError::InvalidManifest(format!(
                    "profile '{}' not found in {}",
                    installed.compiled.profile_name,
                    manifest_path.display()
                ))
            })?;

        let sqlite_specs = manifest
            .databases_for_profile(profile)
            .iter()
            .filter(|database| database.engine == "sqlite")
            .collect::<Vec<_>>();

        let state_profile_dir = project_root
            .join("state")
            .join("skills")
            .join(&manifest.skill.name)
            .join(manifest.skill.version.as_str())
            .join(&installed.compiled.profile_name)
            .join("databases");
        self.host.create_dir_all(&state_profile_dir)?;

        for database in sqlite_specs {
            let database_path = state_profile_dir.join(format!("{}.sqlite", database.name));
            if self.host.exists(&database_path) {
                std::fs::remove_file(&database_path).map_err(|source| {
                    SkillProvisionError::WriteFile {
                        path: database_path.clone(),
                        source,
                    }
                })?;
            }
            initialize_sqlite_database(
                &installed.catalog_dir,
                profile.name.as_str(),
                database,
                selected_seed,
                &database_path,
            )?;
        }

        Ok(())
    }
}

fn copy_skill_assets<H: ProjectHost>(
    host: &H,
    skill_dir: &Path,
    artifact_dir: &Path,
    manifest: &SkillManifest,
    profile_name: &str,
    wit_path: &str,
) -> Result<(), SkillProvisionError> {
    if let Some(instructions) = &manifest.defaults.instructions {
        copy_path_relative(host, skill_dir, artifact_dir, &instructions.source)?;
    }
    copy_path_relative(host, skill_dir, artifact_dir, wit_path)?;
    let databases = manifest.databases_for_profile(
        manifest
            .profiles
            .iter()
            .find(|profile| profile.name == profile_name)
            .ok_or_else(|| {
                SkillProvisionError::InvalidManifest(format!(
                    "profile '{}' not found in manifest",
                    profile_name
                ))
            })?,
    );
    for database in databases {
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

fn initialize_sqlite_database(
    profile_dir: &Path,
    profile_name: &str,
    database: &pera_core::SkillDatabaseSpec,
    selected_seed: Option<&str>,
    database_path: &Path,
) -> Result<(), SkillProvisionError> {
    let provider = SqliteCapabilityProvider::new(database_path)
        .map_err(|error| SkillProvisionError::Runtime(error.to_string()))?;

    if database.on_load.as_deref() == Some("migrate") {
        let Some(migrations) = &database.migrations else {
            return Err(SkillProvisionError::InvalidManifest(format!(
                "database '{}' requested migrate on load but has no migrations directory",
                database.name
            )));
        };
        let migrations_dir = profile_dir.join(&migrations.dir);
        apply_sql_directory(&provider, &migrations_dir)?;
    }

    if let Some(seed_name) = resolve_seed_name(database, selected_seed)? {
        let seeds = database.seeds.as_ref().ok_or_else(|| {
            SkillProvisionError::InvalidManifest(format!(
                "database '{}' does not define seeds",
                database.name
            ))
        })?;
        let seed_path = profile_dir.join(&seeds.dir).join(format!("{seed_name}.sql"));
        apply_sql_file(&provider, &seed_path)?;
    }

    let _ = profile_name;
    Ok(())
}

fn resolve_seed_name(
    database: &pera_core::SkillDatabaseSpec,
    selected_seed: Option<&str>,
) -> Result<Option<String>, SkillProvisionError> {
    match selected_seed {
        None => Ok(database
            .seeds
            .as_ref()
            .and_then(|seeds| seeds.default.clone())),
        Some("") => {
            let default_seed = database
                .seeds
                .as_ref()
                .and_then(|seeds| seeds.default.clone())
                .ok_or_else(|| {
                    SkillProvisionError::InvalidManifest(format!(
                        "database '{}' has no default seed",
                        database.name
                    ))
                })?;
            Ok(Some(default_seed))
        }
        Some(seed_name) => Ok(Some(seed_name.to_owned())),
    }
}

fn apply_sql_directory(
    provider: &SqliteCapabilityProvider,
    dir: &Path,
) -> Result<(), SkillProvisionError> {
    let mut entries = std::fs::read_dir(dir)
        .map_err(|source| SkillProvisionError::ReadFile {
            path: dir.to_path_buf(),
            source,
        })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|source| SkillProvisionError::ReadFile {
            path: dir.to_path_buf(),
            source,
        })?;
    entries.sort_by_key(|entry| entry.file_name());

    for entry in entries {
        let path = entry.path();
        if entry
            .file_type()
            .map_err(|source| SkillProvisionError::ReadFile {
                path: path.clone(),
                source,
            })?
            .is_file()
            && path.extension().and_then(|ext| ext.to_str()) == Some("sql")
        {
            apply_sql_file(provider, &path)?;
        }
    }
    Ok(())
}

fn apply_sql_file(
    provider: &SqliteCapabilityProvider,
    path: &Path,
) -> Result<(), SkillProvisionError> {
    let sql = std::fs::read_to_string(path).map_err(|source| SkillProvisionError::ReadFile {
        path: path.to_path_buf(),
        source,
    })?;
    provider
        .execute_batch(&sql)
        .map_err(|error| SkillProvisionError::Runtime(error.to_string()))
}
