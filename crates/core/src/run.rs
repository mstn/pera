use std::error::Error;
use std::fmt::{Display, Formatter};

use crate::{
    ActionId, ActionRecord, CodeArtifact, ExecutionEvent, ExecutionOutput, ExecutionSnapshot,
    InputValues, RunId,
};

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct StartExecutionRequest {
    pub code: CodeArtifact,
    pub inputs: InputValues,
    pub repl_state: Option<ExecutionSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ExecutionStatus {
    Running,
    WaitingForAction(ActionId),
    Completed(ExecutionOutput),
    Failed(String),
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ExecutionSession {
    pub id: RunId,
    pub status: ExecutionStatus,
    pub snapshot: Option<ExecutionSnapshot>,
    pub repl_state: Option<ExecutionSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreError {
    message: String,
}

impl StoreError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl Display for StoreError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl Error for StoreError {}

pub trait RunStore {
    fn create_run(&mut self, session: ExecutionSession) -> Result<(), StoreError>;

    fn save_run(&mut self, session: ExecutionSession) -> Result<(), StoreError>;

    fn load_run(&self, run_id: RunId) -> Result<ExecutionSession, StoreError>;

    fn list_runs(&self) -> Result<Vec<RunId>, StoreError>;

    fn save_code_artifact(
        &mut self,
        _run_id: RunId,
        _artifact: &CodeArtifact,
    ) -> Result<(), StoreError> {
        Ok(())
    }

    fn save_action(&mut self, action: ActionRecord) -> Result<(), StoreError>;

    fn load_action(&self, action_id: ActionId) -> Result<ActionRecord, StoreError>;

    fn list_actions(&self) -> Result<Vec<ActionId>, StoreError>;
}

pub trait EventPublisher {
    fn publish(&mut self, event: ExecutionEvent) -> Result<(), StoreError>;
}
