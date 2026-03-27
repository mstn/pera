use std::path::PathBuf;

use clap::Args;

use crate::config::AgentConfig;
use crate::error::CliError;
use crate::repl::session::run_repl;

#[derive(Debug, Args)]
pub struct ReplCommand {
    #[arg(long)]
    pub root: PathBuf,

    #[arg(long, default_value_t = false)]
    pub debug: bool,

    #[arg(long, env = "OPENAI_API_KEY")]
    pub openai_api_key: Option<String>,

    #[arg(long, env = "OPENAI_MODEL")]
    pub openai_model: Option<String>,
}

impl ReplCommand {
    pub async fn execute(&self) -> Result<(), CliError> {
        let root = self
            .root
            .canonicalize()
            .map_err(|source| CliError::ReadFile {
                path: self.root.clone(),
                source,
            })?;
        let agent_config = AgentConfig::from_openai(
            root.clone(),
            self.debug,
            self.openai_api_key.clone(),
            self.openai_model.clone(),
        )?;
        run_repl(agent_config).await
    }
}
