use crate::{ActionId, RunId, Value};

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ExecutionEvent {
    RunSubmitted { run_id: RunId },
    RunStarted { run_id: RunId },
    ActionEnqueued {
        run_id: RunId,
        action_id: ActionId,
        #[serde(default)]
        skill_name: String,
        #[serde(default)]
        action_name: String,
    },
    ActionClaimed {
        run_id: RunId,
        action_id: ActionId,
        #[serde(default)]
        skill_name: String,
        #[serde(default)]
        action_name: String,
        worker_id: String,
    },
    ActionCompleted {
        run_id: RunId,
        action_id: ActionId,
        #[serde(default)]
        skill_name: String,
        #[serde(default)]
        action_name: String,
    },
    ActionFailed {
        run_id: RunId,
        action_id: ActionId,
        #[serde(default)]
        skill_name: String,
        #[serde(default)]
        action_name: String,
        message: String,
    },
    RunResumed { run_id: RunId },
    RunCompleted { run_id: RunId, value: Value },
    RunFailed { run_id: RunId, message: String },
}

impl ExecutionEvent {
    pub fn run_id(&self) -> RunId {
        match self {
            Self::RunSubmitted { run_id }
            | Self::RunStarted { run_id }
            | Self::ActionEnqueued { run_id, .. }
            | Self::ActionClaimed { run_id, .. }
            | Self::ActionCompleted { run_id, .. }
            | Self::ActionFailed { run_id, .. }
            | Self::RunResumed { run_id }
            | Self::RunCompleted { run_id, .. }
            | Self::RunFailed { run_id, .. } => *run_id,
        }
    }
}
