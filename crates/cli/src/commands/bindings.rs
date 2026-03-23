use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;

use clap::{Args, Subcommand};

use crate::error::CliError;

const COMPONENTIZE_PY_VERSION: &str = "0.17.1";

#[derive(Debug, Args)]
pub struct BindingsCommand {
    #[command(subcommand)]
    command: BindingsSubcommand,
}

impl BindingsCommand {
    pub async fn execute(&self) -> Result<(), CliError> {
        match &self.command {
            BindingsSubcommand::Python(command) => command.execute(),
        }
    }
}

#[derive(Debug, Subcommand)]
enum BindingsSubcommand {
    Python(PythonBindingsCommand),
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

impl PythonBindingsCommand {
    fn execute(&self) -> Result<(), CliError> {
        fs::create_dir_all(&self.out_dir).map_err(|source| CliError::CreateDir {
            path: self.out_dir.clone(),
            source,
        })?;

        run_componentize_py(&self.uvx, None, [
            "--wit-path".to_owned(),
            self.wit_path.display().to_string(),
            "--world".to_owned(),
            self.world.clone(),
            "bindings".to_owned(),
            self.out_dir.display().to_string(),
        ])
    }
}

pub fn run_componentize_py(
    uvx: &str,
    cwd: Option<&Path>,
    args: impl IntoIterator<Item = String>,
) -> Result<(), CliError> {
    let mut command = ProcessCommand::new(uvx);
    command
        .arg("--from")
        .arg(format!("componentize-py=={COMPONENTIZE_PY_VERSION}"))
        .arg("componentize-py");
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    for arg in args {
        command.arg(arg);
    }

    let output = command.output().map_err(|source| {
        if source.kind() == std::io::ErrorKind::NotFound {
            CliError::ToolNotInstalled {
                tool: "uvx",
                hint: "Install uv or pass --uvx <path-to-uvx>.".to_owned(),
            }
        } else {
            CliError::ToolIo { tool: "uvx", source }
        }
    })?;

    if !output.status.success() {
        return Err(CliError::ToolFailed {
            tool: "uvx componentize-py",
            status: output.status,
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        });
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    if !stdout.trim().is_empty() {
        print!("{stdout}");
    }

    Ok(())
}
