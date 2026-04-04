use std::error::Error;
use std::fmt::{Display, Formatter};
use std::io;
use std::path::PathBuf;

use pera_skills::SkillProvisionError;

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

impl From<SkillProvisionError> for EvalError {
    fn from(value: SkillProvisionError) -> Self {
        match value {
            SkillProvisionError::ReadFile { path, source } => Self::ReadFile { path, source },
            SkillProvisionError::WriteFile { path, source }
            | SkillProvisionError::CreateDir { path, source } => Self::ReadFile { path, source },
            SkillProvisionError::CopyPath {
                source_path,
                source,
                ..
            } => Self::ReadFile {
                path: source_path,
                source,
            },
            SkillProvisionError::InvalidManifest(message)
            | SkillProvisionError::InvalidArguments(message) => Self::InvalidSpec(message),
            SkillProvisionError::ToolNotInstalled { tool, hint } => {
                Self::Internal(format!("{tool} is not installed. {hint}"))
            }
            SkillProvisionError::ToolIo { tool, source } => {
                Self::Internal(format!("failed to run {tool}: {source}"))
            }
            SkillProvisionError::ToolFailed {
                tool,
                status,
                stderr,
            } => {
                if stderr.trim().is_empty() {
                    Self::Internal(format!("{tool} exited with status {status}"))
                } else {
                    Self::Internal(format!(
                        "{tool} exited with status {status}: {}",
                        stderr.trim()
                    ))
                }
            }
            SkillProvisionError::Runtime(message) | SkillProvisionError::Internal(message) => {
                Self::Internal(message)
            }
        }
    }
}
