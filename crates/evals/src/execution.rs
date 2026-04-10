use std::path::PathBuf;

use pera_orchestrator::{EvalResult, FinishReason};
use serde::Serialize;
use serde_yaml::Value;

#[derive(Debug, Clone)]
pub struct EvalProjectLayout {
    pub root: PathBuf,
    pub evals_dir: PathBuf,
    pub catalog_dir: PathBuf,
    pub cache_dir: PathBuf,
}

#[derive(Debug, Clone)]
pub struct PreparedCatalogSkill {
    pub skill_name: String,
    pub skill_version: String,
    pub profile_name: String,
    pub compiled_dir: PathBuf,
    pub catalog_dir: PathBuf,
    pub compiled_now: bool,
    pub uploaded_now: bool,
}

#[derive(Debug, Clone)]
pub struct EvalPreparation {
    pub project: EvalProjectLayout,
    pub skills: Vec<PreparedCatalogSkill>,
}

#[derive(Debug, Clone)]
pub struct EvalRunWorkspace {
    pub root: PathBuf,
    pub run_dir: PathBuf,
}

#[derive(Debug, Clone)]
pub struct EvalRunResult {
    pub passed: bool,
    pub finish_reason: FinishReason,
    pub evaluation: EvalResult,
    pub final_agent_message: Option<String>,
    pub judge_results: Vec<EvalJudgeResult>,
    pub trace: Vec<EvalTraceEvent>,
    pub trajectory: Vec<EvalTrajectoryEvent>,
    pub workspace: EvalRunWorkspace,
}

#[derive(Debug, Clone, Serialize)]
pub struct EvalJudgeResult {
    pub criterion_index: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub passed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub score: Option<f64>,
    pub summary: String,
    pub rubric: String,
    pub response: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EvalTraceEvent {
    UserMessage { content: String },
    AgentMessage { content: String },
    ActionRequested { action: SerializedAction },
    ActionCompleted { outcome: SerializedOutcome },
    ActionFailed { message: String },
}

#[derive(Debug, Clone, Serialize)]
pub struct EvalTrajectoryEvent {
    pub sequence: usize,
    #[serde(flatten)]
    pub payload: EvalTrajectoryPayload,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EvalTrajectoryPayload {
    SessionStarted {
        task_id: String,
        instructions: String,
    },
    ObservationRecorded,
    ParticipantMessage {
        participant: String,
        content: String,
    },
    ActionRequested {
        participant: String,
        action: SerializedAction,
        execution: String,
    },
    ActionRunStatus {
        participant: String,
        action_id: String,
        run_id: String,
        status: EvalTrajectoryActionRunStatus,
    },
    ActionScheduled {
        participant: String,
        action_id: String,
        action: SerializedAction,
        execution: String,
    },
    ActionCompleted {
        participant: String,
        action_id: String,
        outcome: SerializedOutcome,
    },
    ActionFailed {
        participant: String,
        action_id: String,
        user_message: String,
        detail: String,
        origin: String,
    },
    ParticipantNotification {
        participant: String,
        content: String,
    },
    ParticipantYielded {
        participant: String,
    },
    ParticipantLoopCompleted {
        participant: String,
    },
    ParticipantFinished {
        participant: String,
    },
    SessionFinished {
        reason: String,
    },
    EvaluationCompleted {
        passed: bool,
        score: Option<f64>,
        summary: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EvalTrajectoryActionRunStatus {
    RunSubmitted,
    RunStarted,
    ActionEnqueued {
        engine_action_id: String,
        skill_name: String,
        action_name: String,
    },
    ActionClaimed {
        engine_action_id: String,
        skill_name: String,
        action_name: String,
        worker_id: String,
    },
    ActionCompleted {
        engine_action_id: String,
        skill_name: String,
        action_name: String,
    },
    ActionFailed {
        engine_action_id: String,
        skill_name: String,
        action_name: String,
        message: String,
    },
    RunResumed,
}

#[derive(Debug, Clone, Serialize)]
pub struct SerializedAction {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arguments: Option<Value>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SerializedOutcome {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payload: Option<Value>,
}
