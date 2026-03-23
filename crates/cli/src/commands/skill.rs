use std::fs;
use std::path::{Path, PathBuf};

use clap::{Args, Subcommand};
use pera_core::{
    SkillDatabaseSpec, SkillManifest, SkillProfileManifest,
};

use crate::commands::bindings::run_componentize_py;
use crate::error::CliError;

#[derive(Debug, Args)]
pub struct SkillCommand {
    #[command(subcommand)]
    command: SkillSubcommand,
}

impl SkillCommand {
    pub async fn execute(&self) -> Result<(), CliError> {
        match &self.command {
            SkillSubcommand::Compile(command) => command.execute(),
            SkillSubcommand::Upload(command) => command.execute(),
        }
    }
}

#[derive(Debug, Subcommand)]
enum SkillSubcommand {
    Compile(CompileSkillCommand),
    Upload(UploadSkillCommand),
}

#[derive(Debug, Args)]
struct CompileSkillCommand {
    #[arg(long)]
    skill_dir: PathBuf,
    #[arg(long)]
    profile: Option<String>,
    #[arg(long, default_value = "uvx")]
    uvx: String,
}

#[derive(Debug, Args)]
struct UploadSkillCommand {
    #[arg(long)]
    compiled_dir: PathBuf,
    #[arg(long)]
    root: PathBuf,
}

impl CompileSkillCommand {
    fn execute(&self) -> Result<(), CliError> {
        let manifest_dir = self.skill_dir.canonicalize().map_err(|source| CliError::ReadFile {
            path: self.skill_dir.clone(),
            source,
        })?;
        let manifest_path = resolve_manifest_path(&manifest_dir)?;
        let manifest_source = fs::read_to_string(&manifest_path).map_err(|source| CliError::ReadFile {
            path: manifest_path.clone(),
            source,
        })?;
        let manifest: SkillManifest = serde_yaml::from_str(&manifest_source)
            .map_err(|error| CliError::UnexpectedStateOwned(format!("invalid manifest: {error}")))?;
        let profile = select_profile(&manifest, self.profile.as_deref())?;
        let wasm = profile
            .runtime
            .wasm
            .as_ref()
            .ok_or(CliError::InvalidArguments("only wasm-component profiles are supported"))?;
        let artifact_dir = manifest_dir.join(&wasm.artifacts.dir);
        fs::create_dir_all(&artifact_dir).map_err(|source| CliError::CreateDir {
            path: artifact_dir.clone(),
            source,
        })?;
        let wit_path = manifest_dir.join(&wasm.wit.path);

        let component_path = artifact_dir.join("component.wasm");
        let module = wasm
            .build
            .as_ref()
            .ok_or(CliError::InvalidArguments("wasm build configuration is required"))?
            .module
            .clone();
        run_componentize_py(
            &self.uvx,
            Some(&manifest_dir),
            [
                "--wit-path".to_owned(),
                wit_path.display().to_string(),
                "--world".to_owned(),
                wasm.wit.world.clone(),
                "componentize".to_owned(),
                "--stub-wasi".to_owned(),
                module,
                "-o".to_owned(),
                component_path.display().to_string(),
            ],
        )?;

        copy_file(
            &manifest_path,
            &artifact_dir.join(
                manifest_path
                    .file_name()
                    .ok_or(CliError::InvalidArguments("manifest path is missing a file name"))?,
            ),
        )?;

        if let Some(instructions) = &manifest.defaults.instructions {
            copy_path_relative(&manifest_dir, &artifact_dir, &instructions.source)?;
        }
        copy_path_relative(&manifest_dir, &artifact_dir, &wasm.wit.path)?;
        for database in &manifest.defaults.databases {
            copy_database_assets(&manifest_dir, &artifact_dir, database)?;
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
        let meta_path = artifact_dir.join("meta.json");
        let meta_bytes = serde_json::to_vec_pretty(&meta)
            .map_err(|error| CliError::UnexpectedStateOwned(format!("failed to serialize meta.json: {error}")))?;
        fs::write(&meta_path, meta_bytes).map_err(|source| CliError::WriteFile {
            path: meta_path,
            source,
        })?;

        println!("Compiled profile '{}' into {}", profile.name, artifact_dir.display());
        Ok(())
    }
}

impl UploadSkillCommand {
    fn execute(&self) -> Result<(), CliError> {
        let compiled_dir = self
            .compiled_dir
            .canonicalize()
            .map_err(|source| CliError::ReadFile {
                path: self.compiled_dir.clone(),
                source,
            })?;
        let meta_path = compiled_dir.join("meta.json");
        let meta_source = fs::read_to_string(&meta_path).map_err(|source| CliError::ReadFile {
            path: meta_path.clone(),
            source,
        })?;
        let meta: CompiledSkillMeta = serde_json::from_str(&meta_source).map_err(|error| {
            CliError::UnexpectedStateOwned(format!("invalid meta.json in {}: {error}", meta_path.display()))
        })?;

        let catalog_dir = self
            .root
            .join("catalog")
            .join("skills")
            .join(&meta.skill_name)
            .join(&meta.skill_version)
            .join(&meta.profile_name);

        if catalog_dir.exists() {
            fs::remove_dir_all(&catalog_dir).map_err(|source| CliError::CopyPath {
                source_path: compiled_dir.clone(),
                target_path: catalog_dir.clone(),
                source,
            })?;
        }

        copy_dir_recursive(&compiled_dir, &catalog_dir)?;
        println!(
            "Uploaded skill '{}' profile '{}' to {}",
            meta.skill_name,
            meta.profile_name,
            catalog_dir.display()
        );
        Ok(())
    }
}

fn resolve_manifest_path(skill_dir: &Path) -> Result<PathBuf, CliError> {
    let candidates = [
        skill_dir.join("manifest.yaml"),
        skill_dir.join("skill.yaml"),
        skill_dir.join("skill.yml"),
    ];

    candidates
        .into_iter()
        .find(|path| path.exists())
        .ok_or_else(|| {
            CliError::UnexpectedStateOwned(format!(
                "no manifest found in {}",
                skill_dir.display()
            ))
        })
}

#[derive(Debug, serde::Deserialize)]
struct CompiledSkillMeta {
    skill_name: String,
    skill_version: String,
    profile_name: String,
}

fn select_profile<'a>(
    manifest: &'a SkillManifest,
    profile_name: Option<&str>,
) -> Result<&'a SkillProfileManifest, CliError> {
    match profile_name {
        Some(profile_name) => manifest
            .profiles
            .iter()
            .find(|profile| profile.name == profile_name)
            .ok_or_else(|| CliError::UnexpectedStateOwned(format!("unknown profile '{profile_name}'"))),
        None => manifest
            .profiles
            .iter()
            .find(|profile| profile.default)
            .or_else(|| manifest.profiles.first())
            .ok_or(CliError::UnexpectedStateOwned("manifest has no profiles".to_owned())),
    }
}

fn copy_database_assets(
    manifest_dir: &Path,
    artifact_dir: &Path,
    database: &SkillDatabaseSpec,
) -> Result<(), CliError> {
    if let Some(migrations) = &database.migrations {
        copy_path_relative(manifest_dir, artifact_dir, &migrations.dir)?;
    }
    if let Some(seeds) = &database.seeds {
        copy_path_relative(manifest_dir, artifact_dir, &seeds.dir)?;
    }
    Ok(())
}

fn copy_path_relative(
    source_root: &Path,
    target_root: &Path,
    relative: &str,
) -> Result<(), CliError> {
    let source = source_root.join(relative);
    let target = target_root.join(relative);
    if source.is_dir() {
        copy_dir_recursive(&source, &target)
    } else {
        copy_file(&source, &target)
    }
}

fn copy_dir_recursive(source: &Path, target: &Path) -> Result<(), CliError> {
    fs::create_dir_all(target).map_err(|source_err| CliError::CreateDir {
        path: target.to_path_buf(),
        source: source_err,
    })?;
    for entry in fs::read_dir(source).map_err(|source_err| CliError::CopyPath {
        source_path: source.to_path_buf(),
        target_path: target.to_path_buf(),
        source: source_err,
    })? {
        let entry = entry.map_err(|source_err| CliError::CopyPath {
            source_path: source.to_path_buf(),
            target_path: target.to_path_buf(),
            source: source_err,
        })?;
        let path = entry.path();
        let target_path = target.join(entry.file_name());
        if entry.file_type().map_err(|source_err| CliError::CopyPath {
            source_path: path.clone(),
            target_path: target_path.clone(),
            source: source_err,
        })?.is_dir() {
            copy_dir_recursive(&path, &target_path)?;
        } else {
            copy_file(&path, &target_path)?;
        }
    }
    Ok(())
}

fn copy_file(source: &Path, target: &Path) -> Result<(), CliError> {
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent).map_err(|source_err| CliError::CreateDir {
            path: parent.to_path_buf(),
            source: source_err,
        })?;
    }
    fs::copy(source, target).map_err(|source_err| CliError::CopyPath {
        source_path: source.to_path_buf(),
        target_path: target.to_path_buf(),
        source: source_err,
    })?;
    Ok(())
}
