use clap::{Parser, Subcommand};

use crate::commands::bindings::BindingsCommand;
use crate::commands::repl::ReplCommand;
use crate::commands::run::RunCommand;
use crate::commands::skill::SkillCommand;

#[derive(Debug, Parser)]
#[command(name = "pera")]
#[command(about = "Pera development CLI")]
pub struct Cli {
    #[arg(long, global = true, env = "PERA_LOG_LEVEL", default_value = "warn")]
    pub log_level: String,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    Bindings(BindingsCommand),
    Repl(ReplCommand),
    Run(RunCommand),
    Skill(SkillCommand),
}
