use std::error::Error;
use std::fmt::{Display, Formatter};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use pera_core::{
    ActionId, ActionRequest, ActionSkillRef, CanonicalInvocation, CanonicalValue, RunId,
};
use tokio::process::Command;

use crate::{ActionExecutionUpdate, ActionExecutor, SkillRuntime, WasmtimeComponentActionExecutor};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeEnvironmentObservation {
    pub workspace_root: PathBuf,
    pub cwd: PathBuf,
    pub available_skills: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodeEnvironmentAction {
    Shell {
        command: String,
    },
    ReadFile {
        path: PathBuf,
    },
    WriteFile {
        path: PathBuf,
        content: Vec<u8>,
    },
    CallTool {
        skill: ActionSkillRef,
        invocation: CanonicalInvocation,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodeEnvironmentOutcome {
    Shell {
        command: String,
        exit_code: i32,
        stdout: String,
        stderr: String,
    },
    ReadFile {
        path: PathBuf,
        content: Vec<u8>,
    },
    WriteFile {
        path: PathBuf,
        bytes_written: usize,
    },
    ToolCall {
        skill: ActionSkillRef,
        invocation: CanonicalInvocation,
        value: CanonicalValue,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeEnvironmentSnapshot {
    pub cwd: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeEnvironmentError {
    message: String,
}

impl CodeEnvironmentError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl Display for CodeEnvironmentError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl Error for CodeEnvironmentError {}

#[async_trait]
pub trait CodeToolExecutor: Send + Sync {
    async fn execute_tool(
        &self,
        request: ActionRequest,
    ) -> Result<CanonicalValue, CodeEnvironmentError>;
}

#[async_trait]
impl CodeToolExecutor for WasmtimeComponentActionExecutor {
    async fn execute_tool(
        &self,
        request: ActionRequest,
    ) -> Result<CanonicalValue, CodeEnvironmentError> {
        match self.execute(request).await {
            ActionExecutionUpdate::Completed(result) => Ok(result.value),
            ActionExecutionUpdate::Failed { message, .. } => {
                Err(CodeEnvironmentError::new(message))
            }
            ActionExecutionUpdate::Claimed { .. } => Err(CodeEnvironmentError::new(
                "unexpected claimed update returned by tool executor",
            )),
        }
    }
}

#[derive(Clone)]
pub struct CodeEnvironment {
    workspace_root: PathBuf,
    cwd: PathBuf,
    skill_runtime: Option<Arc<SkillRuntime>>,
    tool_executor: Option<Arc<dyn CodeToolExecutor>>,
}

impl std::fmt::Debug for CodeEnvironment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CodeEnvironment")
            .field("workspace_root", &self.workspace_root)
            .field("cwd", &self.cwd)
            .field("has_skill_runtime", &self.skill_runtime.is_some())
            .field("has_tool_executor", &self.tool_executor.is_some())
            .finish()
    }
}

impl CodeEnvironment {
    pub fn new(
        workspace_root: impl Into<PathBuf>,
        skill_runtime: Option<SkillRuntime>,
    ) -> Result<Self, CodeEnvironmentError> {
        let workspace_root = workspace_root.into();
        let tool_executor = skill_runtime
            .clone()
            .map(WasmtimeComponentActionExecutor::new)
            .transpose()
            .map_err(|error| CodeEnvironmentError::new(error.to_string()))?
            .map(|executor| Arc::new(executor) as Arc<dyn CodeToolExecutor>);

        Ok(Self {
            cwd: workspace_root.clone(),
            workspace_root,
            skill_runtime: skill_runtime.map(Arc::new),
            tool_executor,
        })
    }

    pub fn with_tool_executor(
        workspace_root: impl Into<PathBuf>,
        skill_runtime: Option<SkillRuntime>,
        tool_executor: Arc<dyn CodeToolExecutor>,
    ) -> Self {
        let workspace_root = workspace_root.into();
        Self {
            cwd: workspace_root.clone(),
            workspace_root,
            skill_runtime: skill_runtime.map(Arc::new),
            tool_executor: Some(tool_executor),
        }
    }

    pub async fn reset(&mut self) -> Result<CodeEnvironmentObservation, CodeEnvironmentError> {
        self.cwd = self.workspace_root.clone();
        self.observe().await
    }

    pub async fn observe(&self) -> Result<CodeEnvironmentObservation, CodeEnvironmentError> {
        let available_skills = self
            .skill_runtime
            .as_ref()
            .map(|runtime| {
                runtime
                    .catalog()
                    .skills()
                    .map(|skill| skill.metadata.skill_name.clone())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        Ok(CodeEnvironmentObservation {
            workspace_root: self.workspace_root.clone(),
            cwd: self.cwd.clone(),
            available_skills,
        })
    }

    pub async fn snapshot(&self) -> Result<CodeEnvironmentSnapshot, CodeEnvironmentError> {
        Ok(CodeEnvironmentSnapshot {
            cwd: self.cwd.clone(),
        })
    }

    pub async fn restore(
        &mut self,
        snapshot: &CodeEnvironmentSnapshot,
    ) -> Result<(), CodeEnvironmentError> {
        let cwd = self.resolve_path(&snapshot.cwd)?;
        self.cwd = cwd;
        Ok(())
    }

    pub async fn step(
        &mut self,
        action: CodeEnvironmentAction,
    ) -> Result<CodeEnvironmentOutcome, CodeEnvironmentError> {
        match action {
            CodeEnvironmentAction::Shell { command } => self.execute_shell(command).await,
            CodeEnvironmentAction::ReadFile { path } => self.read_file(path).await,
            CodeEnvironmentAction::WriteFile { path, content } => {
                self.write_file(path, content).await
            }
            CodeEnvironmentAction::CallTool { skill, invocation } => {
                self.call_tool(skill, invocation).await
            }
        }
    }

    async fn execute_shell(
        &self,
        command: String,
    ) -> Result<CodeEnvironmentOutcome, CodeEnvironmentError> {
        let output = Command::new("bash")
            .arg("-lc")
            .arg(&command)
            .current_dir(&self.cwd)
            .output()
            .await
            .map_err(|error| {
                CodeEnvironmentError::new(format!("failed to run shell command: {error}"))
            })?;

        Ok(CodeEnvironmentOutcome::Shell {
            command,
            exit_code: output.status.code().unwrap_or_default(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }

    async fn read_file(
        &self,
        path: PathBuf,
    ) -> Result<CodeEnvironmentOutcome, CodeEnvironmentError> {
        let resolved = self.resolve_path(&path)?;
        let content = tokio::fs::read(&resolved).await.map_err(|error| {
            CodeEnvironmentError::new(format!(
                "failed to read file '{}': {error}",
                resolved.display()
            ))
        })?;

        Ok(CodeEnvironmentOutcome::ReadFile { path, content })
    }

    async fn write_file(
        &self,
        path: PathBuf,
        content: Vec<u8>,
    ) -> Result<CodeEnvironmentOutcome, CodeEnvironmentError> {
        let resolved = self.resolve_path(&path)?;
        if let Some(parent) = resolved.parent() {
            tokio::fs::create_dir_all(parent).await.map_err(|error| {
                CodeEnvironmentError::new(format!(
                    "failed to create parent directories for '{}': {error}",
                    resolved.display()
                ))
            })?;
        }

        tokio::fs::write(&resolved, &content)
            .await
            .map_err(|error| {
                CodeEnvironmentError::new(format!(
                    "failed to write file '{}': {error}",
                    resolved.display()
                ))
            })?;

        Ok(CodeEnvironmentOutcome::WriteFile {
            path,
            bytes_written: content.len(),
        })
    }

    async fn call_tool(
        &self,
        skill: ActionSkillRef,
        invocation: CanonicalInvocation,
    ) -> Result<CodeEnvironmentOutcome, CodeEnvironmentError> {
        let tool_executor = self
            .tool_executor
            .as_ref()
            .ok_or_else(|| CodeEnvironmentError::new("no tool executor is configured"))?;

        let value = tool_executor
            .execute_tool(ActionRequest {
                id: ActionId::generate(),
                run_id: RunId::generate(),
                skill: skill.clone(),
                invocation: invocation.clone(),
            })
            .await?;

        Ok(CodeEnvironmentOutcome::ToolCall {
            skill,
            invocation,
            value,
        })
    }

    fn resolve_path(&self, path: &Path) -> Result<PathBuf, CodeEnvironmentError> {
        let base = if path.is_absolute() {
            self.workspace_root.clone()
        } else {
            self.cwd.clone()
        };

        let relative = if path.is_absolute() {
            path.strip_prefix(&self.workspace_root).map_err(|_| {
                CodeEnvironmentError::new(format!(
                    "absolute path '{}' is outside the workspace root '{}'",
                    path.display(),
                    self.workspace_root.display()
                ))
            })?
        } else {
            path
        };

        let mut normalized = PathBuf::new();
        for component in relative.components() {
            match component {
                Component::CurDir => {}
                Component::Normal(value) => normalized.push(value),
                Component::ParentDir => {
                    if !normalized.pop() {
                        return Err(CodeEnvironmentError::new(format!(
                            "path '{}' escapes the workspace root",
                            path.display()
                        )));
                    }
                }
                Component::Prefix(_) | Component::RootDir => {
                    return Err(CodeEnvironmentError::new(format!(
                        "path '{}' is not supported",
                        path.display()
                    )));
                }
            }
        }

        Ok(base.join(normalized))
    }
}
