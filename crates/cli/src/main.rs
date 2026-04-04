mod cli;
mod commands;
mod config;
mod error;
mod repl;

use std::process::ExitCode;

use clap::Parser;
use tracing_subscriber::EnvFilter;

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
    let _ = dotenvy::dotenv();
    let cli = Cli::parse();
    init_logger(&cli.log_level);

    match cli.command {
        Command::Bindings(command) => command.execute().await,
        Command::Eval(command) => command.execute().await,
        Command::Repl(command) => command.execute().await,
        Command::Run(command) => command.execute().await,
        Command::Skill(command) => command.execute().await,
    }
}

fn init_logger(level: &str) {
    let filter = EnvFilter::try_new(level).unwrap_or_else(|_| EnvFilter::new("warn"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(true)
        .with_ansi(true)
        .compact()
        .try_init();
}
