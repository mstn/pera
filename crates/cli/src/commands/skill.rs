use std::fs;
use std::path::{Path, PathBuf};

use crate::error::CliError;
use clap::{Args, Subcommand};
use pera_core::{SkillDatabaseSpec, SkillManifest, SkillProfileManifest};
use pera_runtime::{FileSystemSkillRuntimeLoader, SqliteCapabilityProvider};
use pera_skills::{
    FileSystemProjectHost, SkillProvisioner, UvxComponentizer, load_manifest, select_profile,
};

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
            SkillSubcommand::Precompile(command) => command.execute().await,
            SkillSubcommand::RebuildCatalog(command) => command.execute().await,
            SkillSubcommand::Upload(command) => command.execute().await,
        }
    }
}

#[derive(Debug, Subcommand)]
enum SkillSubcommand {
    Compile(CompileSkillCommand),
    Db(SkillDbCommand),
    Precompile(PrecompileSkillCommand),
    RebuildCatalog(RebuildCatalogCommand),
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
struct PrecompileSkillCommand {
    #[arg(long)]
    root: PathBuf,
}

#[derive(Debug, Args)]
struct RebuildCatalogCommand {
    #[arg(long)]
    root: PathBuf,
    #[arg(long)]
    source_dir: PathBuf,
    #[arg(long)]
    skill: Option<String>,
    #[arg(long)]
    profile: Option<String>,
    #[arg(long, default_value = "uvx")]
    uvx: String,
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
        let provisioner =
            SkillProvisioner::new(FileSystemProjectHost, UvxComponentizer::new(&self.uvx));
        let compiled = provisioner
            .compile_skill(&self.skill_dir, self.profile.as_deref())
            .map_err(CliError::from)?;
        println!(
            "Compiled profile '{}' into {}",
            compiled.profile_name,
            compiled.compiled_dir.display()
        );
        Ok(())
    }
}

impl UploadSkillCommand {
    async fn execute(&self) -> Result<(), CliError> {
        let provisioner = SkillProvisioner::new(FileSystemProjectHost, UvxComponentizer::default());
        let installed = provisioner
            .install_compiled_skill(&self.compiled_dir, &self.root)
            .map_err(CliError::from)?;
        provisioner
            .precompile_installed_skill(&self.root, &installed)
            .await
            .map_err(CliError::from)?;
        println!(
            "Uploaded and precompiled skill '{}' profile '{}' to {}",
            installed.compiled.skill_name,
            installed.compiled.profile_name,
            installed.catalog_dir.display()
        );
        Ok(())
    }
}

impl PrecompileSkillCommand {
    async fn execute(&self) -> Result<(), CliError> {
        let root = self
            .root
            .canonicalize()
            .map_err(|source| CliError::ReadFile {
                path: self.root.clone(),
                source,
            })?;
        let skill_runtime = FileSystemSkillRuntimeLoader::new(&root)
            .load()
            .map_err(CliError::Store)?;
        skill_runtime
            .warm_components()
            .await
            .map_err(CliError::Store)?;
        println!(
            "Precompiled installed skills into {}",
            root.join("cache").join("wasmtime").display()
        );
        Ok(())
    }
}

impl RebuildCatalogCommand {
    async fn execute(&self) -> Result<(), CliError> {
        let root = self
            .root
            .canonicalize()
            .map_err(|source| CliError::ReadFile {
                path: self.root.clone(),
                source,
            })?;
        let source_dir = self
            .source_dir
            .canonicalize()
            .map_err(|source| CliError::ReadFile {
                path: self.source_dir.clone(),
                source,
            })?;

        let provisioner =
            SkillProvisioner::new(FileSystemProjectHost, UvxComponentizer::new(&self.uvx));
        provisioner
            .ensure_project_layout(&root)
            .map_err(CliError::from)?;

        let skill_dirs = discover_skill_dirs(&source_dir, self.skill.as_deref())?;
        if skill_dirs.is_empty() {
            return Err(CliError::UnexpectedStateOwned(format!(
                "no skill sources found in {}",
                source_dir.display()
            )));
        }

        let mut rebuilt_profiles = 0usize;
        for skill_dir in skill_dirs {
            let (manifest_path, manifest) =
                load_manifest(&FileSystemProjectHost, &skill_dir).map_err(CliError::from)?;

            let profiles = if let Some(profile_name) = self.profile.as_deref() {
                vec![select_profile(&manifest, Some(profile_name)).map_err(CliError::from)?]
            } else {
                manifest.profiles.iter().collect::<Vec<_>>()
            };

            for profile in profiles {
                let artifact_dir = profile
                    .runtime
                    .wasm
                    .as_ref()
                    .ok_or_else(|| {
                        CliError::UnexpectedStateOwned(format!(
                            "profile '{}' in {} is not a wasm-component profile",
                            profile.name,
                            manifest_path.display()
                        ))
                    })?
                    .artifacts
                    .dir
                    .clone();
                let artifact_dir = skill_dir.join(artifact_dir);
                if artifact_dir.exists() {
                    fs::remove_dir_all(&artifact_dir).map_err(|source| CliError::WriteFile {
                        path: artifact_dir.clone(),
                        source,
                    })?;
                }

                let installed = provisioner
                    .ensure_catalog_skill(&skill_dir, Some(profile.name.as_str()), &root)
                    .await
                    .map_err(CliError::from)?;
                println!(
                    "Rebuilt catalog skill '{}' profile '{}' into {}",
                    installed.compiled.skill_name,
                    installed.compiled.profile_name,
                    installed.catalog_dir.display()
                );
                rebuilt_profiles += 1;
            }
        }

        println!("Rebuilt {rebuilt_profiles} catalog profile(s).");
        Ok(())
    }
}

impl SkillDatabaseLifecycleCommand {
    fn execute(&self, mode: DbMode) -> Result<(), CliError> {
        let root = self
            .root
            .canonicalize()
            .map_err(|source| CliError::ReadFile {
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
        let manifest_source =
            fs::read_to_string(&manifest_path).map_err(|source| CliError::ReadFile {
                path: manifest_path.clone(),
                source,
            })?;
        let manifest: SkillManifest = serde_yaml::from_str(&manifest_source).map_err(|error| {
            CliError::UnexpectedStateOwned(format!("invalid manifest: {error}"))
        })?;
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
            .databases_for_profile(profile)
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
            CliError::UnexpectedStateOwned(format!("no manifest found in {}", skill_dir.display()))
        })
}

fn discover_skill_dirs(source_dir: &Path, selected_skill: Option<&str>) -> Result<Vec<PathBuf>, CliError> {
    let mut skill_dirs = Vec::new();

    if let Some(skill_name) = selected_skill {
        let skill_dir = source_dir.join(skill_name);
        if !skill_dir.exists() {
            return Err(CliError::UnexpectedStateOwned(format!(
                "skill source '{}' not found in {}",
                skill_name,
                source_dir.display()
            )));
        }
        resolve_manifest_path(&skill_dir)?;
        skill_dirs.push(skill_dir);
        return Ok(skill_dirs);
    }

    let mut entries = fs::read_dir(source_dir)
        .map_err(|source| CliError::ReadFile {
            path: source_dir.to_path_buf(),
            source,
        })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|source| CliError::ReadFile {
            path: source_dir.to_path_buf(),
            source,
        })?;
    entries.sort_by_key(|entry| entry.file_name());

    for entry in entries {
        let path = entry.path();
        let file_type = entry.file_type().map_err(|source| CliError::ReadFile {
            path: path.clone(),
            source,
        })?;
        if !file_type.is_dir() {
            continue;
        }
        if resolve_manifest_path(&path).is_ok() {
            skill_dirs.push(path);
        }
    }

    Ok(skill_dirs)
}

fn initialize_sqlite_database(
    profile_dir: &Path,
    profile: &SkillProfileManifest,
    database: &SkillDatabaseSpec,
    selected_seed: Option<&str>,
    database_path: &Path,
) -> Result<(), CliError> {
    let provider = SqliteCapabilityProvider::new(database_path).map_err(|error| {
        CliError::UnexpectedStateOwned(format!(
            "failed to initialize sqlite database {}: {error}",
            database_path.display()
        ))
    })?;

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
        let seed_path = profile_dir
            .join(&seeds.dir)
            .join(format!("{seed_name}.sql"));
        apply_sql_file(&provider, &seed_path)?;
    }

    println!(
        "{} SQLite database '{}' for profile '{}' at {}",
        if database_path.exists() {
            "Prepared"
        } else {
            "Created"
        },
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
                let manifest_source =
                    fs::read_to_string(&manifest_path).map_err(|source| CliError::ReadFile {
                        path: manifest_path.clone(),
                        source,
                    })?;
                let manifest: SkillManifest =
                    serde_yaml::from_str(&manifest_source).map_err(|error| {
                        CliError::UnexpectedStateOwned(format!("invalid manifest: {error}"))
                    })?;
                let profile_name = profile_dir
                    .file_name()
                    .and_then(|value| value.to_str())
                    .unwrap_or_default();
                if manifest
                    .profiles
                    .iter()
                    .any(|profile| profile.name == profile_name && profile.default)
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
    provider.execute_batch(&sql).map_err(|error| {
        CliError::UnexpectedStateOwned(format!(
            "failed to apply SQL from {}: {error}",
            path.display()
        ))
    })?;
    Ok(())
}
