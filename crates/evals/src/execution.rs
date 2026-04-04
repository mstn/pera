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
    pub trace: Vec<EvalTraceEvent>,
    pub workspace: EvalRunWorkspace,
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
