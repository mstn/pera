mod cli;
mod commands;
mod error;

use std::process::ExitCode;

use clap::Parser;

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

    match cli.command {
        Command::Bindings(command) => command.execute().await,
        Command::Run(command) => command.execute().await,
        Command::Skill(command) => command.execute().await,
    }
}
