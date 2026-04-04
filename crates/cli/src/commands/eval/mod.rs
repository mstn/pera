mod artifacts;
mod overrides;
mod spec;

use std::path::PathBuf;

use clap::{Args, Subcommand};

use self::artifacts::{RunArtifacts, create_run_artifacts};
use self::overrides::OverrideSet;
use self::spec::{EvalSpec, load_eval_spec};
use crate::error::CliError;

#[derive(Debug, Args)]
pub struct EvalCommand {
    #[command(subcommand)]
    command: EvalSubcommand,
}

impl EvalCommand {
    pub async fn execute(&self) -> Result<(), CliError> {
        match &self.command {
            EvalSubcommand::Run(command) => command.execute(EvalMode::Run).await,
            EvalSubcommand::Optimize(command) => command.execute(EvalMode::Optimize).await,
        }
    }
}

#[derive(Debug, Subcommand)]
enum EvalSubcommand {
    Run(EvalModeCommand),
    Optimize(EvalModeCommand),
}

#[derive(Debug, Clone, Copy)]
enum EvalMode {
    Run,
    Optimize,
}

impl EvalMode {
    fn as_str(self) -> &'static str {
        match self {
            Self::Run => "run",
            Self::Optimize => "optimize",
        }
    }
}

#[derive(Debug, Args)]
struct EvalModeCommand {
    pub spec: PathBuf,
    #[arg(long)]
    pub output_folder: Option<PathBuf>,
    #[arg(long)]
    pub name: Option<String>,
    #[arg(long = "set", value_name = "PATH=VALUE")]
    pub set_values: Vec<String>,
    #[arg(long = "set-json", value_name = "PATH=JSON")]
    pub set_json_values: Vec<String>,
}

impl EvalModeCommand {
    async fn execute(&self, mode: EvalMode) -> Result<(), CliError> {
        let overrides = OverrideSet::from_cli(&self.set_values, &self.set_json_values)?;
        let mut loaded = load_eval_spec(&self.spec, &overrides)?;
        if let Some(path) = &self.output_folder {
            loaded.override_output_folder(path.clone())?;
        }
        let output_root = resolved_output_folder(&loaded.spec, self.output_folder.as_ref())?;
        let artifacts = create_run_artifacts(
            &output_root,
            self.name.as_deref().unwrap_or(&loaded.spec.id),
            mode.as_str(),
            &self.spec,
            &loaded,
            &overrides,
        )?;

        print_summary(mode, &artifacts);
        Ok(())
    }
}

fn resolved_output_folder(
    spec: &EvalSpec,
    cli_output_folder: Option<&PathBuf>,
) -> Result<PathBuf, CliError> {
    if let Some(path) = cli_output_folder {
        return Ok(path.clone());
    }

    if spec.runtime.output_folder.as_os_str().is_empty() {
        return Err(CliError::InvalidArguments(
            "spec runtime.output_folder cannot be empty",
        ));
    }

    Ok(spec.runtime.output_folder.clone())
}

fn print_summary(mode: EvalMode, artifacts: &RunArtifacts) {
    println!("mode: {}", mode.as_str());
    println!("run_dir: {}", artifacts.run_dir.display());
    println!("resolved_spec: {}", artifacts.resolved_spec_path.display());
    println!("manifest: {}", artifacts.manifest_path.display());
}
