use clap::{Parser, Subcommand};

use crate::commands::run::RunCommand;

#[derive(Debug, Parser)]
#[command(name = "pera")]
#[command(about = "Pera development CLI")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    Run(RunCommand),
}
