use std::fs;
use std::path::{Path, PathBuf};

use clap::{Args, Subcommand};
use pera_core::{
    SkillDatabaseSpec, SkillManifest, SkillProfileManifest,
};
use pera_runtime::SqliteCapabilityProvider;

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
            SkillSubcommand::Db(command) => command.execute(),
            SkillSubcommand::Upload(command) => command.execute(),
        }
    }
}

#[derive(Debug, Subcommand)]
enum SkillSubcommand {
    Compile(CompileSkillCommand),
    Db(SkillDbCommand),
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

#[derive(Debug, Args)]
struct SkillDbCommand {
    #[command(subcommand)]
    command: SkillDbSubcommand,
}

impl SkillDbCommand {
    fn execute(&self) -> Result<(), CliError> {
        match &self.command {
            SkillDbSubcommand::Create(command) => command.execute(DbMode::Create),
            SkillDbSubcommand::Reset(command) => command.execute(DbMode::Reset),
        }
    }
}

#[derive(Debug, Subcommand)]
enum SkillDbSubcommand {
    Create(SkillDatabaseLifecycleCommand),
    Reset(SkillDatabaseLifecycleCommand),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DbMode {
    Create,
    Reset,
}

#[derive(Debug, Args)]
struct SkillDatabaseLifecycleCommand {
    #[arg(long)]
    root: PathBuf,
    #[arg(long)]
    skill: String,
    #[arg(long)]
    version: Option<String>,
    #[arg(long)]
    profile: Option<String>,
    #[arg(long, num_args = 0..=1, default_missing_value = "")]
    seed: Option<String>,
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

impl SkillDatabaseLifecycleCommand {
    fn execute(&self, mode: DbMode) -> Result<(), CliError> {
        let root = self.root.canonicalize().map_err(|source| CliError::ReadFile {
            path: self.root.clone(),
            source,
        })?;
        let profile_dir = resolve_catalog_profile_dir(
            &root,
            &self.skill,
            self.version.as_deref(),
            self.profile.as_deref(),
        )?;
        let manifest_path = resolve_manifest_path(&profile_dir)?;
        let manifest_source = fs::read_to_string(&manifest_path).map_err(|source| CliError::ReadFile {
            path: manifest_path.clone(),
            source,
        })?;
        let manifest: SkillManifest = serde_yaml::from_str(&manifest_source)
            .map_err(|error| CliError::UnexpectedStateOwned(format!("invalid manifest: {error}")))?;
        let profile_name = profile_dir
            .file_name()
            .and_then(|value| value.to_str())
            .ok_or_else(|| {
                CliError::UnexpectedStateOwned(format!(
                    "invalid profile directory name: {}",
                    profile_dir.display()
                ))
            })?;
        let profile = manifest
            .profiles
            .iter()
            .find(|profile| profile.name == profile_name)
            .ok_or_else(|| {
                CliError::UnexpectedStateOwned(format!(
                    "profile '{profile_name}' not found in {}",
                    manifest_path.display()
                ))
            })?;

        let sqlite_specs = manifest
            .defaults
            .databases
            .iter()
            .filter(|database| database.engine == "sqlite")
            .collect::<Vec<_>>();
        if sqlite_specs.is_empty() {
            return Err(CliError::UnexpectedStateOwned(format!(
                "skill '{}' profile '{}' has no sqlite databases",
                manifest.skill.name, profile.name
            )));
        }

        let state_profile_dir = root
            .join("state")
            .join("skills")
            .join(&manifest.skill.name)
            .join(manifest.skill.version.as_str())
            .join(profile_name)
            .join("databases");
        fs::create_dir_all(&state_profile_dir).map_err(|source| CliError::CreateDir {
            path: state_profile_dir.clone(),
            source,
        })?;

        let selected_seed = self.seed.as_deref();
        let mut initialized = 0usize;
        for database in sqlite_specs {
            let database_path = state_profile_dir.join(format!("{}.sqlite", database.name));
            prepare_database_path(mode, &database_path)?;
            initialize_sqlite_database(
                &profile_dir,
                profile,
                database,
                selected_seed,
                &database_path,
            )?;
            initialized += 1;
        }

        let verb = match mode {
            DbMode::Create => "Created",
            DbMode::Reset => "Reset",
        };
        println!(
            "{verb} {initialized} SQLite database{} for skill '{}' profile '{}'",
            if initialized == 1 { "" } else { "s" },
            manifest.skill.name,
            profile.name
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

fn initialize_sqlite_database(
    profile_dir: &Path,
    profile: &SkillProfileManifest,
    database: &SkillDatabaseSpec,
    selected_seed: Option<&str>,
    database_path: &Path,
) -> Result<(), CliError> {
    let provider = SqliteCapabilityProvider::new(database_path)
        .map_err(|error| CliError::UnexpectedStateOwned(format!(
            "failed to initialize sqlite database {}: {error}",
            database_path.display()
        )))?;

    if database.on_load.as_deref() == Some("migrate") {
        let Some(migrations) = &database.migrations else {
            return Err(CliError::UnexpectedStateOwned(format!(
                "database '{}' requested migrate on load but has no migrations directory",
                database.name
            )));
        };
        let migrations_dir = profile_dir.join(&migrations.dir);
        apply_sql_directory(&provider, &migrations_dir)?;
    }

    if let Some(seed_name) = resolve_seed_name(database, selected_seed)? {
        let seeds = database.seeds.as_ref().ok_or_else(|| {
            CliError::UnexpectedStateOwned(format!(
                "database '{}' does not define seeds",
                database.name
            ))
        })?;
        let seed_path = profile_dir.join(&seeds.dir).join(format!("{seed_name}.sql"));
        apply_sql_file(&provider, &seed_path)?;
    }

    println!(
        "{} SQLite database '{}' for profile '{}' at {}",
        if database_path.exists() { "Prepared" } else { "Created" },
        database.name,
        profile.name,
        database_path.display()
    );
    Ok(())
}

fn resolve_catalog_profile_dir(
    root: &Path,
    skill: &str,
    version: Option<&str>,
    profile: Option<&str>,
) -> Result<PathBuf, CliError> {
    let skill_dir = root.join("catalog").join("skills").join(skill);
    if !skill_dir.exists() {
        return Err(CliError::UnexpectedStateOwned(format!(
            "skill '{skill}' is not installed in {}",
            skill_dir.display()
        )));
    }

    let version_dir = match version {
        Some(version) => {
            let path = skill_dir.join(version);
            if !path.exists() {
                return Err(CliError::UnexpectedStateOwned(format!(
                    "skill '{skill}' version '{version}' is not installed"
                )));
            }
            path
        }
        None => select_single_directory(&skill_dir, "version")?,
    };

    let profile_dir = match profile {
        Some(profile) => {
            let path = version_dir.join(profile);
            if !path.exists() {
                return Err(CliError::UnexpectedStateOwned(format!(
                    "skill '{skill}' version '{}' profile '{profile}' is not installed",
                    version_dir
                        .file_name()
                        .and_then(|value| value.to_str())
                        .unwrap_or("<unknown>")
                )));
            }
            path
        }
        None => select_default_or_single_profile(&version_dir)?,
    };

    Ok(profile_dir)
}

fn select_single_directory(dir: &Path, label: &str) -> Result<PathBuf, CliError> {
    let mut entries = fs::read_dir(dir)
        .map_err(|source| CliError::ReadFile {
            path: dir.to_path_buf(),
            source,
        })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|source| CliError::ReadFile {
            path: dir.to_path_buf(),
            source,
        })?
        .into_iter()
        .filter_map(|entry| match entry.file_type() {
            Ok(file_type) if file_type.is_dir() => Some(Ok(entry.path())),
            Ok(_) => None,
            Err(source) => Some(Err(CliError::ReadFile {
                path: entry.path(),
                source,
            })),
        })
        .collect::<Result<Vec<_>, _>>()?;
    entries.sort();

    match entries.as_slice() {
        [single] => Ok(single.clone()),
        [] => Err(CliError::UnexpectedStateOwned(format!(
            "no {label} directories found in {}",
            dir.display()
        ))),
        _ => Err(CliError::UnexpectedStateOwned(format!(
            "multiple {label} directories found in {}; specify --{label}",
            dir.display()
        ))),
    }
}

fn select_default_or_single_profile(version_dir: &Path) -> Result<PathBuf, CliError> {
    let mut entries = fs::read_dir(version_dir)
        .map_err(|source| CliError::ReadFile {
            path: version_dir.to_path_buf(),
            source,
        })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|source| CliError::ReadFile {
            path: version_dir.to_path_buf(),
            source,
        })?
        .into_iter()
        .filter_map(|entry| match entry.file_type() {
            Ok(file_type) if file_type.is_dir() => Some(Ok(entry.path())),
            Ok(_) => None,
            Err(source) => Some(Err(CliError::ReadFile {
                path: entry.path(),
                source,
            })),
        })
        .collect::<Result<Vec<_>, _>>()?;
    entries.sort();

    match entries.as_slice() {
        [single] => Ok(single.clone()),
        [] => Err(CliError::UnexpectedStateOwned(format!(
            "no profile directories found in {}",
            version_dir.display()
        ))),
        _ => {
            for profile_dir in &entries {
                let manifest_path = resolve_manifest_path(profile_dir)?;
                let manifest_source = fs::read_to_string(&manifest_path).map_err(|source| {
                    CliError::ReadFile {
                        path: manifest_path.clone(),
                        source,
                    }
                })?;
                let manifest: SkillManifest = serde_yaml::from_str(&manifest_source).map_err(|error| {
                    CliError::UnexpectedStateOwned(format!("invalid manifest: {error}"))
                })?;
                let profile_name = profile_dir
                    .file_name()
                    .and_then(|value| value.to_str())
                    .unwrap_or_default();
                if manifest.profiles.iter().any(|profile| profile.name == profile_name && profile.default)
                {
                    return Ok(profile_dir.clone());
                }
            }
            Err(CliError::UnexpectedStateOwned(format!(
                "multiple profiles found in {}; specify --profile",
                version_dir.display()
            )))
        }
    }
}

fn prepare_database_path(mode: DbMode, database_path: &Path) -> Result<(), CliError> {
    match mode {
        DbMode::Create => {
            if database_path.exists() {
                return Err(CliError::UnexpectedStateOwned(format!(
                    "database already exists at {}",
                    database_path.display()
                )));
            }
        }
        DbMode::Reset => {
            if database_path.exists() {
                fs::remove_file(database_path).map_err(|source| CliError::WriteFile {
                    path: database_path.to_path_buf(),
                    source,
                })?;
            }
        }
    }
    Ok(())
}

fn resolve_seed_name(
    database: &SkillDatabaseSpec,
    selected_seed: Option<&str>,
) -> Result<Option<String>, CliError> {
    match selected_seed {
        None => Ok(None),
        Some("") => {
            let default_seed = database
                .seeds
                .as_ref()
                .and_then(|seeds| seeds.default.clone())
                .ok_or_else(|| {
                    CliError::UnexpectedStateOwned(format!(
                        "database '{}' has no default seed",
                        database.name
                    ))
                })?;
            Ok(Some(default_seed))
        }
        Some(seed_name) => Ok(Some(seed_name.to_owned())),
    }
}

fn apply_sql_directory(provider: &SqliteCapabilityProvider, dir: &Path) -> Result<(), CliError> {
    let mut entries = fs::read_dir(dir)
        .map_err(|source| CliError::ReadFile {
            path: dir.to_path_buf(),
            source,
        })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|source| CliError::ReadFile {
            path: dir.to_path_buf(),
            source,
        })?;
    entries.sort_by_key(|entry| entry.file_name());

    for entry in entries {
        let path = entry.path();
        if entry
            .file_type()
            .map_err(|source| CliError::ReadFile {
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

fn apply_sql_file(provider: &SqliteCapabilityProvider, path: &Path) -> Result<(), CliError> {
    let sql = fs::read_to_string(path).map_err(|source| CliError::ReadFile {
        path: path.to_path_buf(),
        source,
    })?;
    provider
        .execute_batch(&sql)
        .map_err(|error| CliError::UnexpectedStateOwned(format!(
            "failed to apply SQL from {}: {error}",
            path.display()
        )))?;
    Ok(())
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
