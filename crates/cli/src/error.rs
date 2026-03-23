use std::error::Error;
use std::fmt::{Display, Formatter};
use std::io;
use std::path::PathBuf;

use pera_core::{RunId, StoreError};
use pera_runtime::ExecutionEngineError;

#[derive(Debug)]
pub enum CliError {
    ReadFile { path: PathBuf, source: io::Error },
    InvalidArguments(&'static str),
    UnknownRun(RunId),
    Store(StoreError),
    Engine(ExecutionEngineError),
    UnexpectedStateOwned(String),
}

impl Display for CliError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ReadFile { path, source } => {
                write!(f, "failed to read {}: {source}", path.display())
            }
            Self::InvalidArguments(message) => f.write_str(message),
            Self::UnknownRun(run_id) => write!(f, "run {run_id} was not found"),
            Self::Store(error) => write!(f, "filesystem state initialization failed: {error}"),
            Self::Engine(error) => write!(f, "execution failed: {error}"),
            Self::UnexpectedStateOwned(message) => write!(f, "execution failed: {message}"),
        }
    }
}

impl Error for CliError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::ReadFile { source, .. } => Some(source),
            Self::InvalidArguments(_) | Self::UnknownRun(_) => None,
            Self::Store(error) => Some(error),
            Self::Engine(error) => Some(error),
            Self::UnexpectedStateOwned(_) => None,
        }
    }
}
