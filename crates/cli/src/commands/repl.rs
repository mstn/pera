use std::path::PathBuf;

use clap::Args;

use crate::error::CliError;
use crate::repl::session::run_repl;

#[derive(Debug, Args)]
pub struct ReplCommand {
    #[arg(long)]
    pub root: PathBuf,
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
        run_repl(&root).await
    }
}
