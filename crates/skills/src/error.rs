use std::error::Error;
use std::fmt::{Display, Formatter};
use std::io;
use std::path::PathBuf;
use std::process::ExitStatus;

#[derive(Debug)]
pub enum SkillProvisionError {
    ReadFile { path: PathBuf, source: io::Error },
    WriteFile { path: PathBuf, source: io::Error },
    CreateDir { path: PathBuf, source: io::Error },
    CopyPath {
        source_path: PathBuf,
        target_path: PathBuf,
        source: io::Error,
    },
    InvalidManifest(String),
    InvalidArguments(String),
    ToolNotInstalled { tool: &'static str, hint: String },
    ToolIo { tool: &'static str, source: io::Error },
    ToolFailed {
        tool: &'static str,
        status: ExitStatus,
        stderr: String,
    },
    Runtime(String),
    Internal(String),
}

impl Display for SkillProvisionError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ReadFile { path, source } => write!(f, "failed to read {}: {source}", path.display()),
            Self::WriteFile { path, source } => write!(f, "failed to write {}: {source}", path.display()),
            Self::CreateDir { path, source } => write!(f, "failed to create {}: {source}", path.display()),
            Self::CopyPath { source_path, target_path, source } => write!(
                f,
                "failed to copy {} to {}: {source}",
                source_path.display(),
                target_path.display()
            ),
            Self::InvalidManifest(message)
            | Self::InvalidArguments(message)
            | Self::Runtime(message)
            | Self::Internal(message) => f.write_str(message),
            Self::ToolNotInstalled { tool, hint } => write!(f, "{tool} is not installed. {hint}"),
            Self::ToolIo { tool, source } => write!(f, "failed to run {tool}: {source}"),
            Self::ToolFailed { tool, status, stderr } => {
                if stderr.trim().is_empty() {
                    write!(f, "{tool} exited with status {status}")
                } else {
                    write!(f, "{tool} exited with status {status}: {}", stderr.trim())
                }
            }
        }
    }
}

impl Error for SkillProvisionError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::ReadFile { source, .. }
            | Self::WriteFile { source, .. }
            | Self::CreateDir { source, .. }
            | Self::CopyPath { source, .. }
            | Self::ToolIo { source, .. } => Some(source),
            Self::InvalidManifest(_)
            | Self::InvalidArguments(_)
            | Self::ToolNotInstalled { .. }
            | Self::ToolFailed { .. }
            | Self::Runtime(_)
            | Self::Internal(_) => None,
        }
    }
}
