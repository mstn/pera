use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use pera_core::{ActionSkillRef, SkillManifest, SkillProfileManifest, SkillVersion};
use pera_runtime::{FileSystemLayout, FileSystemSkillRuntimeLoader};

use crate::error::EvalError;
use crate::execution::{EvalPreparation, EvalProjectLayout, PreparedCatalogSkill};
use crate::spec::{EvalCatalogSkillSpec, EvalSkillSourceSpec, EvalSpec};

#[derive(Debug, Clone)]
pub struct EvalRunner {
    uvx: String,
}

impl Default for EvalRunner {
    fn default() -> Self {
        Self {
            uvx: "uvx".to_owned(),
        }
    }
}

impl EvalRunner {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn prepare(&self, spec: &EvalSpec) -> Result<EvalPreparation, EvalError> {
        let project = prepare_project_layout(&spec.runtime.output_folder)?;
        let source_map = spec
            .runtime
            .skill_sources
            .iter()
            .map(|source| (source.id.clone(), source.clone()))
            .collect::<BTreeMap<_, _>>();

        let mut skills = Vec::new();
        for catalog_skill in &spec.runtime.catalog {
            let prepared = self
                .prepare_catalog_skill(&project, &source_map, catalog_skill)
                .await?;
            skills.push(prepared);
        }

        Ok(EvalPreparation { project, skills })
    }

    async fn prepare_catalog_skill(
        &self,
        project: &EvalProjectLayout,
        source_map: &BTreeMap<String, EvalSkillSourceSpec>,
        entry: &EvalCatalogSkillSpec,
    ) -> Result<PreparedCatalogSkill, EvalError> {
        let source = source_map.get(&entry.source).ok_or_else(|| {
            EvalError::InvalidSpec(format!(
                "skill '{}' references unknown source '{}'",
                entry.skill, entry.source
            ))
        })?;
        let skill_dir = source.path.join(&entry.skill);
        let manifest_path = resolve_manifest_path(&skill_dir)?;
        let manifest_source =
            fs::read_to_string(&manifest_path).map_err(|source| EvalError::ReadFile {
                path: manifest_path.clone(),
                source,
            })?;
        let manifest: SkillManifest = serde_yaml::from_str(&manifest_source).map_err(|error| {
            EvalError::InvalidSpec(format!(
                "invalid skill manifest {}: {error}",
                manifest_path.display()
            ))
        })?;
        let profile = select_profile(&manifest, entry.profile.as_deref())?;
        let (compiled_dir, compiled_now) =
            ensure_compiled_skill(&self.uvx, &skill_dir, &manifest_path, &manifest, profile)?;
        let catalog_dir = project
            .catalog_dir
            .join(&manifest.skill.name)
            .join(manifest.skill.version.as_str())
            .join(&profile.name);
        let uploaded_now = sync_compiled_skill_into_catalog(&compiled_dir, &catalog_dir)?;

        let skill_runtime = FileSystemSkillRuntimeLoader::new(&project.root)
            .load()
            .map_err(|error| EvalError::Internal(error.to_string()))?;
        skill_runtime
            .precompile_skill(&ActionSkillRef {
                skill_name: manifest.skill.name.clone(),
                skill_version: Some(SkillVersion::new(
                    manifest.skill.version.as_str().to_owned(),
                )),
                profile_name: Some(profile.name.clone()),
            })
            .await
            .map_err(|error| EvalError::Internal(error.to_string()))?;

        Ok(PreparedCatalogSkill {
            skill_name: manifest.skill.name.clone(),
            profile_name: profile.name.clone(),
            compiled_dir,
            catalog_dir,
            compiled_now,
            uploaded_now,
        })
    }
}

fn prepare_project_layout(root: &Path) -> Result<EvalProjectLayout, EvalError> {
    fs::create_dir_all(root).map_err(|source| EvalError::ReadFile {
        path: root.to_path_buf(),
        source,
    })?;
    let root = root.canonicalize().map_err(|source| EvalError::ReadFile {
        path: root.to_path_buf(),
        source,
    })?;
    let _layout =
        FileSystemLayout::new(&root).map_err(|error| EvalError::Internal(error.to_string()))?;
    let catalog_dir = root.join("catalog").join("skills");
    let cache_dir = root.join("cache").join("wasmtime");
    let evals_dir = root.join("evals");
    fs::create_dir_all(&catalog_dir).map_err(|source| EvalError::ReadFile {
        path: catalog_dir.clone(),
        source,
    })?;
    fs::create_dir_all(&cache_dir).map_err(|source| EvalError::ReadFile {
        path: cache_dir.clone(),
        source,
    })?;
    fs::create_dir_all(&evals_dir).map_err(|source| EvalError::ReadFile {
        path: evals_dir.clone(),
        source,
    })?;

    Ok(EvalProjectLayout {
        root,
        evals_dir,
        catalog_dir,
        cache_dir,
    })
}

fn resolve_manifest_path(skill_dir: &Path) -> Result<PathBuf, EvalError> {
    let candidates = [
        skill_dir.join("manifest.yaml"),
        skill_dir.join("skill.yaml"),
        skill_dir.join("skill.yml"),
    ];

    candidates
        .into_iter()
        .find(|path| path.exists())
        .ok_or_else(|| {
            EvalError::InvalidSpec(format!("no manifest found in {}", skill_dir.display()))
        })
}

fn select_profile<'a>(
    manifest: &'a SkillManifest,
    profile_name: Option<&str>,
) -> Result<&'a SkillProfileManifest, EvalError> {
    match profile_name {
        Some(profile_name) => manifest
            .profiles
            .iter()
            .find(|profile| profile.name == profile_name)
            .ok_or_else(|| EvalError::InvalidSpec(format!("unknown profile '{profile_name}'"))),
        None => manifest
            .profiles
            .iter()
            .find(|profile| profile.default)
            .or_else(|| manifest.profiles.first())
            .ok_or_else(|| EvalError::InvalidSpec("manifest has no profiles".to_owned())),
    }
}

fn ensure_compiled_skill(
    uvx: &str,
    skill_dir: &Path,
    manifest_path: &Path,
    manifest: &SkillManifest,
    profile: &SkillProfileManifest,
) -> Result<(PathBuf, bool), EvalError> {
    let wasm = profile.runtime.wasm.as_ref().ok_or_else(|| {
        EvalError::InvalidSpec("only wasm-component profiles are supported".to_owned())
    })?;
    let artifact_dir = skill_dir.join(&wasm.artifacts.dir);
    let component_path = artifact_dir.join("component.wasm");
    let meta_path = artifact_dir.join("meta.json");
    if component_path.exists() && meta_path.exists() {
        return Ok((artifact_dir, false));
    }

    fs::create_dir_all(&artifact_dir).map_err(|source| EvalError::ReadFile {
        path: artifact_dir.clone(),
        source,
    })?;
    let build = wasm
        .build
        .as_ref()
        .ok_or_else(|| EvalError::InvalidSpec("wasm build configuration is required".to_owned()))?;
    run_componentize_py(
        uvx,
        skill_dir,
        &skill_dir.join(&wasm.wit.path),
        &wasm.wit.world,
        &build.module,
        &component_path,
    )?;

    copy_file(
        manifest_path,
        &artifact_dir.join(manifest_path.file_name().ok_or_else(|| {
            EvalError::Internal("manifest path is missing a file name".to_owned())
        })?),
    )?;
    if let Some(instructions) = &manifest.defaults.instructions {
        copy_path_relative(skill_dir, &artifact_dir, &instructions.source)?;
    }
    copy_path_relative(skill_dir, &artifact_dir, &wasm.wit.path)?;
    for database in &manifest.defaults.databases {
        if let Some(migrations) = &database.migrations {
            copy_path_relative(skill_dir, &artifact_dir, &migrations.dir)?;
        }
        if let Some(seeds) = &database.seeds {
            copy_path_relative(skill_dir, &artifact_dir, &seeds.dir)?;
        }
    }

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
    let meta_bytes = serde_json::to_vec_pretty(&meta)
        .map_err(|error| EvalError::Internal(format!("failed to serialize meta.json: {error}")))?;
    fs::write(&meta_path, meta_bytes).map_err(|source| EvalError::ReadFile {
        path: meta_path,
        source,
    })?;

    Ok((artifact_dir, true))
}

fn run_componentize_py(
    uvx: &str,
    skill_dir: &Path,
    wit_path: &Path,
    world: &str,
    module: &str,
    component_path: &Path,
) -> Result<(), EvalError> {
    let output = Command::new(uvx)
        .current_dir(skill_dir)
        .args([
            "componentize-py",
            "--wit-path",
            &wit_path.display().to_string(),
            "--world",
            world,
            "componentize",
            module,
            "-o",
            &component_path.display().to_string(),
        ])
        .output()
        .map_err(|source| EvalError::Internal(format!("failed to run {uvx}: {source}")))?;

    if output.status.success() {
        return Ok(());
    }

    Err(EvalError::Internal(format!(
        "{uvx} exited with status {}: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr).trim()
    )))
}

fn sync_compiled_skill_into_catalog(
    compiled_dir: &Path,
    catalog_dir: &Path,
) -> Result<bool, EvalError> {
    if directory_equivalent(compiled_dir, catalog_dir)? {
        return Ok(false);
    }

    if catalog_dir.exists() {
        fs::remove_dir_all(catalog_dir).map_err(|source| {
            EvalError::Internal(format!(
                "failed to replace {}: {source}",
                catalog_dir.display()
            ))
        })?;
    }
    copy_dir_recursive(compiled_dir, catalog_dir)?;
    Ok(true)
}

fn directory_equivalent(left: &Path, right: &Path) -> Result<bool, EvalError> {
    if !left.exists() || !right.exists() {
        return Ok(false);
    }
    let left_component = left.join("component.wasm");
    let right_component = right.join("component.wasm");
    let left_meta = left.join("meta.json");
    let right_meta = right.join("meta.json");
    if !(left_component.exists()
        && right_component.exists()
        && left_meta.exists()
        && right_meta.exists())
    {
        return Ok(false);
    }
    let left_component_bytes = fs::read(&left_component).map_err(|source| EvalError::ReadFile {
        path: left_component,
        source,
    })?;
    let right_component_bytes =
        fs::read(&right_component).map_err(|source| EvalError::ReadFile {
            path: right_component,
            source,
        })?;
    let left_meta_bytes = fs::read(&left_meta).map_err(|source| EvalError::ReadFile {
        path: left_meta,
        source,
    })?;
    let right_meta_bytes = fs::read(&right_meta).map_err(|source| EvalError::ReadFile {
        path: right_meta,
        source,
    })?;
    Ok(left_component_bytes == right_component_bytes && left_meta_bytes == right_meta_bytes)
}

fn copy_path_relative(
    source_root: &Path,
    target_root: &Path,
    relative: &str,
) -> Result<(), EvalError> {
    let source = source_root.join(relative);
    let target = target_root.join(relative);
    if source.is_dir() {
        copy_dir_recursive(&source, &target)
    } else {
        copy_file(&source, &target)
    }
}

fn copy_dir_recursive(source: &Path, target: &Path) -> Result<(), EvalError> {
    fs::create_dir_all(target).map_err(|source_err| EvalError::ReadFile {
        path: target.to_path_buf(),
        source: source_err,
    })?;
    for entry in fs::read_dir(source).map_err(|source_err| EvalError::ReadFile {
        path: source.to_path_buf(),
        source: source_err,
    })? {
        let entry = entry.map_err(|source_err| EvalError::ReadFile {
            path: source.to_path_buf(),
            source: source_err,
        })?;
        let path = entry.path();
        let target_path = target.join(entry.file_name());
        if entry
            .file_type()
            .map_err(|source_err| EvalError::ReadFile {
                path: path.clone(),
                source: source_err,
            })?
            .is_dir()
        {
            copy_dir_recursive(&path, &target_path)?;
        } else {
            copy_file(&path, &target_path)?;
        }
    }
    Ok(())
}

fn copy_file(source: &Path, target: &Path) -> Result<(), EvalError> {
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent).map_err(|source_err| EvalError::ReadFile {
            path: parent.to_path_buf(),
            source: source_err,
        })?;
    }
    fs::copy(source, target).map_err(|source_err| {
        EvalError::Internal(format!(
            "failed to copy {} to {}: {source_err}",
            source.display(),
            target.display()
        ))
    })?;
    Ok(())
}
