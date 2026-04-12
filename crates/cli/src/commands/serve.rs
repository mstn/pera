use std::net::SocketAddr;

use clap::Args;
use pera_server::ServeConfig;

use crate::error::CliError;

#[derive(Debug, Args)]
pub struct ServeCommand {
    #[arg(long, default_value = "127.0.0.1:3000")]
    pub addr: SocketAddr,
}

impl ServeCommand {
    pub async fn execute(&self) -> Result<(), CliError> {
        pera_server::serve(ServeConfig { addr: self.addr })
            .await
            .map_err(|error| CliError::UnexpectedStateOwned(error.to_string()))
    }
}
