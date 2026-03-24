mod cli;
mod commands;
mod error;

use std::process::ExitCode;

use clap::Parser;
use tracing_subscriber::filter::{LevelFilter, Targets};
use tracing_subscriber::{EnvFilter, prelude::*};

use crate::cli::{Cli, Command};

#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<(), error::CliError> {
    let cli = Cli::parse();
    init_logger(&cli.log_level);

    match cli.command {
        Command::Bindings(command) => command.execute().await,
        Command::Run(command) => command.execute().await,
        Command::Skill(command) => command.execute().await,
    }
}

fn init_logger(level: &str) {
    if level.contains('=') || level.contains(',') {
        let filter = EnvFilter::try_new(level).unwrap_or_else(|_| EnvFilter::new("warn"));
        let _ = tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_ansi(true)
            .compact()
            .try_init();
        return;
    }

    let level = level.parse::<LevelFilter>().unwrap_or(LevelFilter::WARN);
    let targets = Targets::new()
        .with_default(LevelFilter::OFF)
        .with_target("pera_cli", level)
        .with_target("pera_runtime", level)
        .with_target("pera_core", level)
        .with_target("pera_canonical", level);

    let fmt_layer = tracing_subscriber::fmt::layer()
        .with_target(true)
        .with_ansi(true)
        .compact();

    let _ = tracing_subscriber::registry()
        .with(targets)
        .with(fmt_layer)
        .try_init();
}
