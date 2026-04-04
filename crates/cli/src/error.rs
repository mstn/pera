use std::error::Error;
use std::fmt::{Display, Formatter};
use std::io;
use std::path::PathBuf;
use std::process::ExitStatus;

use pera_core::{RunId, StoreError};
use pera_evals::EvalError;
use pera_runtime::ExecutionEngineError;
use pera_skills::SkillProvisionError;

#[derive(Debug)]
pub enum CliError {
    ReadFile { path: PathBuf, source: io::Error },
    InvalidArguments(&'static str),
    UnknownRun(RunId),
    Store(StoreError),
    Engine(ExecutionEngineError),
    CreateDir { path: PathBuf, source: io::Error },
    WriteFile { path: PathBuf, source: io::Error },
    CopyPath {
        source_path: PathBuf,
        target_path: PathBuf,
        source: io::Error,
    },
    ToolNotInstalled { tool: &'static str, hint: String },
    ToolIo { tool: &'static str, source: io::Error },
    ToolFailed {
        tool: &'static str,
        status: ExitStatus,
        stderr: String,
    },
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
            Self::CreateDir { path, source } => {
                write!(f, "failed to create {}: {source}", path.display())
            }
            Self::WriteFile { path, source } => {
                write!(f, "failed to write {}: {source}", path.display())
            }
            Self::CopyPath {
                source_path,
                target_path,
                source,
            } => write!(
                f,
                "failed to copy {} to {}: {source}",
                source_path.display(),
                target_path.display()
            ),
            Self::ToolNotInstalled { tool, hint } => {
                write!(f, "{tool} is not installed. {hint}")
            }
            Self::ToolIo { tool, source } => write!(f, "failed to run {tool}: {source}"),
            Self::ToolFailed {
                tool,
                status,
                stderr,
            } => {
                if stderr.trim().is_empty() {
                    write!(f, "{tool} exited with status {status}")
                } else {
                    write!(f, "{tool} exited with status {status}: {}", stderr.trim())
                }
            }
            Self::UnexpectedStateOwned(message) => write!(f, "execution failed: {message}"),
        }
    }
}

impl Error for CliError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::ReadFile { source, .. } => Some(source),
            Self::InvalidArguments(_) | Self::UnknownRun(_) | Self::ToolNotInstalled { .. } => None,
            Self::Store(error) => Some(error),
            Self::Engine(error) => Some(error),
            Self::CreateDir { source, .. } => Some(source),
            Self::WriteFile { source, .. } => Some(source),
            Self::CopyPath { source, .. } => Some(source),
            Self::ToolIo { source, .. } => Some(source),
            Self::ToolFailed { .. } => None,
            Self::UnexpectedStateOwned(_) => None,
        }
    }
}

impl From<EvalError> for CliError {
    fn from(value: EvalError) -> Self {
        match value {
            EvalError::ReadFile { path, source } => Self::ReadFile { path, source },
            EvalError::InvalidSpec(message)
            | EvalError::InvalidOverride(message)
            | EvalError::Internal(message) => Self::UnexpectedStateOwned(message),
        }
    }
}

impl From<SkillProvisionError> for CliError {
    fn from(value: SkillProvisionError) -> Self {
        match value {
            SkillProvisionError::ReadFile { path, source } => Self::ReadFile { path, source },
            SkillProvisionError::WriteFile { path, source } => Self::WriteFile { path, source },
            SkillProvisionError::CreateDir { path, source } => Self::CreateDir { path, source },
            SkillProvisionError::CopyPath {
                source_path,
                target_path,
                source,
            } => Self::CopyPath {
                source_path,
                target_path,
                source,
            },
            SkillProvisionError::InvalidManifest(message)
            | SkillProvisionError::InvalidArguments(message)
            | SkillProvisionError::Runtime(message)
            | SkillProvisionError::Internal(message) => Self::UnexpectedStateOwned(message),
            SkillProvisionError::ToolNotInstalled { tool, hint } => {
                Self::ToolNotInstalled { tool, hint }
            }
            SkillProvisionError::ToolIo { tool, source } => Self::ToolIo { tool, source },
            SkillProvisionError::ToolFailed {
                tool,
                status,
                stderr,
            } => Self::ToolFailed {
                tool,
                status,
                stderr,
            },
        }
    }
}
