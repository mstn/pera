use std::collections::BTreeMap;
use std::error::Error;
use std::fmt::{Display, Formatter};
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use pera_core::{
    ActionId, ActionRequest, ActionSkillRef, CanonicalInvocation, CanonicalValue, RunId,
};
use tokio::task::JoinHandle;

use crate::{ActionExecutionUpdate, ActionExecutor, SkillRuntime, WasmtimeComponentActionExecutor};
use crate::code_tools::default_code_environment_tools;
use crate::CodeEnvironmentTool;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeEnvironmentObservation {
    pub available_tools: Vec<CodeEnvironmentTool>,
    pub available_skills: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodeEnvironmentAction {
    CallTool {
        skill: ActionSkillRef,
        invocation: CanonicalInvocation,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodeEnvironmentOutcome {
    ToolCall {
        skill: ActionSkillRef,
        invocation: CanonicalInvocation,
        value: CanonicalValue,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodeEnvironmentEvent {
    ActionAccepted {
        actor: String,
        action_id: ActionId,
        action: CodeEnvironmentAction,
    },
    ActionCompleted {
        actor: String,
        action_id: ActionId,
        outcome: CodeEnvironmentOutcome,
    },
    ActionFailed {
        actor: String,
        action_id: ActionId,
        error: String,
    },
    Notification {
        actor: String,
        message: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubmittedCodeAction {
    pub action_id: ActionId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeEnvironmentSnapshot;

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

struct PendingCodeAction {
    actor: String,
    handle: JoinHandle<Result<CodeEnvironmentOutcome, CodeEnvironmentError>>,
}

pub struct CodeEnvironment {
    skill_runtime: Option<Arc<SkillRuntime>>,
    tool_executor: Option<Arc<dyn CodeToolExecutor>>,
    pending_actions: BTreeMap<ActionId, PendingCodeAction>,
}

impl std::fmt::Debug for CodeEnvironment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CodeEnvironment")
            .field("has_skill_runtime", &self.skill_runtime.is_some())
            .field("has_tool_executor", &self.tool_executor.is_some())
            .field("pending_actions", &self.pending_actions.len())
            .finish()
    }
}

impl CodeEnvironment {
    pub fn new(
        _workspace_root: impl Into<PathBuf>,
        skill_runtime: Option<SkillRuntime>,
    ) -> Result<Self, CodeEnvironmentError> {
        let tool_executor = skill_runtime
            .clone()
            .map(WasmtimeComponentActionExecutor::new)
            .transpose()
            .map_err(|error| CodeEnvironmentError::new(error.to_string()))?
            .map(|executor| Arc::new(executor) as Arc<dyn CodeToolExecutor>);

        Ok(Self {
            skill_runtime: skill_runtime.map(Arc::new),
            tool_executor,
            pending_actions: BTreeMap::new(),
        })
    }

    pub fn with_tool_executor(
        _workspace_root: impl Into<PathBuf>,
        skill_runtime: Option<SkillRuntime>,
        tool_executor: Arc<dyn CodeToolExecutor>,
    ) -> Self {
        Self {
            skill_runtime: skill_runtime.map(Arc::new),
            tool_executor: Some(tool_executor),
            pending_actions: BTreeMap::new(),
        }
    }

    pub async fn reset(&mut self) -> Result<CodeEnvironmentObservation, CodeEnvironmentError> {
        self.pending_actions.clear();
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
            available_tools: default_code_environment_tools(),
            available_skills,
        })
    }

    pub async fn snapshot(&self) -> Result<CodeEnvironmentSnapshot, CodeEnvironmentError> {
        Ok(CodeEnvironmentSnapshot)
    }

    pub async fn restore(
        &mut self,
        _snapshot: &CodeEnvironmentSnapshot,
    ) -> Result<(), CodeEnvironmentError> {
        Ok(())
    }

    pub async fn step(
        &mut self,
        action: CodeEnvironmentAction,
    ) -> Result<CodeEnvironmentOutcome, CodeEnvironmentError> {
        run_action(self.tool_executor.clone(), action).await
    }

    pub async fn submit(
        &mut self,
        actor: String,
        action: CodeEnvironmentAction,
    ) -> Result<SubmittedCodeAction, CodeEnvironmentError> {
        let action_id = ActionId::generate();
        let tool_executor = self.tool_executor.clone();
        let handle = tokio::spawn(async move { run_action(tool_executor, action).await });
        self.pending_actions.insert(
            action_id,
            PendingCodeAction {
                actor,
                handle,
            },
        );

        Ok(SubmittedCodeAction { action_id })
    }

    pub async fn poll_events(&mut self) -> Result<Vec<CodeEnvironmentEvent>, CodeEnvironmentError> {
        let ready_action_ids = self
            .pending_actions
            .iter()
            .filter_map(|(action_id, pending)| pending.handle.is_finished().then_some(*action_id))
            .collect::<Vec<_>>();
        let mut events = Vec::new();

        for action_id in ready_action_ids {
            let pending = self.pending_actions.remove(&action_id).ok_or_else(|| {
                CodeEnvironmentError::new(format!("missing pending action '{action_id}'"))
            })?;
            let actor = pending.actor;
            match pending.handle.await {
                Ok(Ok(outcome)) => {
                    events.push(CodeEnvironmentEvent::ActionCompleted {
                        actor,
                        action_id,
                        outcome,
                    });
                }
                Ok(Err(error)) => {
                    events.push(CodeEnvironmentEvent::ActionFailed {
                        actor,
                        action_id,
                        error: error.to_string(),
                    });
                }
                Err(error) => {
                    events.push(CodeEnvironmentEvent::ActionFailed {
                        actor,
                        action_id,
                        error: error.to_string(),
                    });
                }
            }
        }

        Ok(events)
    }
}

async fn run_action(
    tool_executor: Option<Arc<dyn CodeToolExecutor>>,
    action: CodeEnvironmentAction,
) -> Result<CodeEnvironmentOutcome, CodeEnvironmentError> {
    match action {
        CodeEnvironmentAction::CallTool { skill, invocation } => {
            call_tool(tool_executor, skill, invocation).await
        }
    }
}

async fn call_tool(
    tool_executor: Option<Arc<dyn CodeToolExecutor>>,
    skill: ActionSkillRef,
    invocation: CanonicalInvocation,
) -> Result<CodeEnvironmentOutcome, CodeEnvironmentError> {
    let tool_executor = tool_executor
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
