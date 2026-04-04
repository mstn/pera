use std::error::Error;
use std::fmt::{Display, Formatter};
use std::io;
use std::path::PathBuf;

#[derive(Debug)]
pub enum EvalError {
    ReadFile {
        path: PathBuf,
        source: io::Error,
    },
    InvalidSpec(String),
    InvalidOverride(String),
    Internal(String),
}

impl Display for EvalError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ReadFile { path, source } => {
                write!(f, "failed to read {}: {source}", path.display())
            }
            Self::InvalidSpec(message) => write!(f, "invalid eval spec: {message}"),
            Self::InvalidOverride(message) => write!(f, "invalid override: {message}"),
            Self::Internal(message) => write!(f, "eval engine error: {message}"),
        }
    }
}

impl Error for EvalError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::ReadFile { source, .. } => Some(source),
            Self::InvalidSpec(_) | Self::InvalidOverride(_) | Self::Internal(_) => None,
        }
    }
}
