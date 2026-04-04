use std::fs;
use std::path::PathBuf;

use clap::{Args, Subcommand};
use pera_canonical::{load_canonical_world_from_wit, render_python_stubs};
use pera_skills::{Componentizer, UvxComponentizer};

use crate::error::CliError;

#[derive(Debug, Args)]
pub struct BindingsCommand {
    #[command(subcommand)]
    command: BindingsSubcommand,
}

impl BindingsCommand {
    pub async fn execute(&self) -> Result<(), CliError> {
        match &self.command {
            BindingsSubcommand::Python(command) => command.execute(),
            BindingsSubcommand::PythonStubs(command) => command.execute(),
        }
    }
}

#[derive(Debug, Subcommand)]
enum BindingsSubcommand {
    Python(PythonBindingsCommand),
    PythonStubs(PythonStubsCommand),
}

#[derive(Debug, Args)]
struct PythonBindingsCommand {
    #[arg(long)]
    wit_path: PathBuf,
    #[arg(long)]
    world: String,
    #[arg(long)]
    out_dir: PathBuf,
    #[arg(long, default_value = "uvx")]
    uvx: String,
}

#[derive(Debug, Args)]
struct PythonStubsCommand {
    #[arg(long)]
    wit_path: PathBuf,
    #[arg(long)]
    world: String,
    #[arg(long)]
    out_file: PathBuf,
}

impl PythonBindingsCommand {
    fn execute(&self) -> Result<(), CliError> {
        fs::create_dir_all(&self.out_dir).map_err(|source| CliError::CreateDir {
            path: self.out_dir.clone(),
            source,
        })?;
        UvxComponentizer::new(&self.uvx)
            .generate_bindings(&self.wit_path, &self.world, &self.out_dir)
            .map_err(CliError::from)
    }
}

impl PythonStubsCommand {
    fn execute(&self) -> Result<(), CliError> {
        if let Some(parent) = self.out_file.parent() {
            fs::create_dir_all(parent).map_err(|source| CliError::CreateDir {
                path: parent.to_path_buf(),
                source,
            })?;
        }

        let world = load_canonical_world_from_wit(&self.wit_path, &self.world).map_err(|error| {
            CliError::UnexpectedStateOwned(format!("failed to build canonical IR from WIT: {error}"))
        })?;
        let stubs = render_python_stubs(&world);
        fs::write(&self.out_file, stubs).map_err(|source| CliError::WriteFile {
            path: self.out_file.clone(),
            source,
        })?;
        Ok(())
    }
}
