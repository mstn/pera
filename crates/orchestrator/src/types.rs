use std::time::Duration;

use pera_core::RunId;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskSpec {
    pub id: String,
    pub instructions: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RunLimits {
    pub max_steps: usize,
    pub max_actions: usize,
    pub max_messages: usize,
    pub max_duration: Option<Duration>,
}

impl Default for RunLimits {
    fn default() -> Self {
        Self {
            max_steps: 64,
            max_actions: 64,
            max_messages: 64,
            max_duration: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunRequest {
    pub task: TaskSpec,
    pub limits: RunLimits,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EvalResult {
    pub passed: bool,
    pub score: Option<f64>,
    pub summary: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FinishReason {
    AgentFinished,
    StepLimitExceeded,
    ActionLimitExceeded,
    MessageLimitExceeded,
    TimeLimitExceeded,
    AgentError(String),
    EnvironmentError(String),
    EnvironmentTerminated(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum TrajectoryEvent<O, A, U> {
    SessionStarted { task: TaskSpec },
    ObservationRecorded { observation: O },
    AgentMessage { content: String },
    AgentActionRequested { action: A },
    EnvironmentActionCompleted { action: A, outcome: U },
    EnvironmentActionFailed { action: A, error: String },
    SessionFinished { reason: FinishReason },
    EvaluationCompleted { result: EvalResult },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Trajectory<O, A, U> {
    pub run_id: RunId,
    pub events: Vec<TrajectoryEvent<O, A, U>>,
}

impl<O, A, U> Trajectory<O, A, U> {
    pub fn new(run_id: RunId) -> Self {
        Self {
            run_id,
            events: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct AgentTurnInput<O, A, U> {
    pub run_id: RunId,
    pub task: TaskSpec,
    pub limits: RunLimits,
    pub observation: O,
    pub trajectory: Trajectory<O, A, U>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentDecision<A> {
    Message { content: String },
    EnvironmentAction { action: A },
    Finish { reason: FinishReason },
}

#[derive(Debug, Clone, PartialEq)]
pub struct RunResult<O, A, U> {
    pub run_id: RunId,
    pub finish_reason: FinishReason,
    pub trajectory: Trajectory<O, A, U>,
    pub evaluation: Option<EvalResult>,
}
