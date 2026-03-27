use std::time::Duration;

use pera_core::{ActionId, RunId};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ParticipantId {
    Agent,
    User,
    Custom(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionExecution {
    Immediate,
    DeferredBlocking,
    DeferredNonBlocking,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminationCondition {
    AllParticipantsFinished,
    AnyParticipantFinished,
    AnyOfParticipantsFinished(Vec<ParticipantId>),
}

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
    pub termination_condition: TerminationCondition,
    pub initial_messages: Vec<InitialInboxMessage>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InitialInboxMessage {
    pub to: ParticipantId,
    pub from: ParticipantId,
    pub content: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EvalResult {
    pub passed: bool,
    pub score: Option<f64>,
    pub summary: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FinishReason {
    ParticipantsFinished,
    ParticipantFinished {
        participant: ParticipantId,
    },
    StepLimitExceeded,
    ActionLimitExceeded,
    MessageLimitExceeded,
    TimeLimitExceeded,
    ParticipantError {
        participant: ParticipantId,
        message: String,
    },
    EnvironmentError(String),
    EnvironmentTerminated(String),
    Deadlocked,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ParticipantInboxEvent<A, U> {
    Message {
        from: ParticipantId,
        content: String,
    },
    ActionAccepted {
        action_id: ActionId,
        action: A,
    },
    ActionCompleted {
        action_id: ActionId,
        outcome: U,
    },
    ActionFailed {
        action_id: ActionId,
        error: String,
    },
    Notification {
        message: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubmittedAction {
    pub action_id: ActionId,
}

#[derive(Debug, Clone, PartialEq)]
pub enum EnvironmentEvent<A, U> {
    ActionAccepted {
        participant: ParticipantId,
        action_id: ActionId,
        action: A,
    },
    ActionCompleted {
        participant: ParticipantId,
        action_id: ActionId,
        outcome: U,
    },
    ActionFailed {
        participant: ParticipantId,
        action_id: ActionId,
        error: String,
    },
    Notification {
        participant: ParticipantId,
        message: String,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum TrajectoryEvent<O, A, U> {
    SessionStarted { task: TaskSpec },
    ObservationRecorded { observation: O },
    ParticipantMessage {
        participant: ParticipantId,
        content: String,
    },
    ActionRequested {
        participant: ParticipantId,
        action: A,
        execution: ActionExecution,
    },
    ActionSubmitted {
        participant: ParticipantId,
        action_id: ActionId,
        action: A,
        execution: ActionExecution,
    },
    ActionCompleted {
        participant: ParticipantId,
        action_id: ActionId,
        outcome: U,
    },
    ActionFailed {
        participant: ParticipantId,
        action_id: ActionId,
        error: String,
    },
    ParticipantYielded {
        participant: ParticipantId,
    },
    ParticipantFinished {
        participant: ParticipantId,
    },
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
pub struct ParticipantInput<O, A, U> {
    pub run_id: RunId,
    pub participant: ParticipantId,
    pub task: TaskSpec,
    pub limits: RunLimits,
    pub observation: O,
    pub inbox: Vec<ParticipantInboxEvent<A, U>>,
    pub trajectory: Trajectory<O, A, U>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParticipantDecision<A> {
    Message { content: String },
    FinalMessage { content: String },
    Action {
        action: A,
        execution: ActionExecution,
    },
    Yield,
    Finish,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RunResult<O, A, U> {
    pub run_id: RunId,
    pub finish_reason: FinishReason,
    pub trajectory: Trajectory<O, A, U>,
    pub evaluation: Option<EvalResult>,
}
