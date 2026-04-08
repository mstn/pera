use std::collections::BTreeMap;

use crate::{ActionId, ActionName, CanonicalValue, RunId, SkillVersion};

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CanonicalInvocation {
    pub action_name: ActionName,
    pub arguments: BTreeMap<String, CanonicalValue>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ActionSkillRef {
    pub skill_name: String,
    #[serde(default)]
    pub skill_version: Option<SkillVersion>,
    #[serde(default)]
    pub profile_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ActionRequest {
    pub id: ActionId,
    pub run_id: RunId,
    pub skill: ActionSkillRef,
    pub invocation: CanonicalInvocation,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ActionResult {
    pub action_id: ActionId,
    pub value: CanonicalValue,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ActionInvocationDiagnostics {
    pub canonical_action_id: String,
    pub export_name: String,
    pub status: String,
    pub current_phase: String,
    pub elapsed_ms: u128,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub events: Vec<ActionInvocationEvent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ActionInvocationError>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ActionInvocationEvent {
    pub source: String,
    pub elapsed_ms: u128,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ActionInvocationError {
    pub source: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ActionStatus {
    Pending,
    Completed(ActionResult),
    Failed { message: String },
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ActionRecord {
    pub request: ActionRequest,
    pub status: ActionStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagnostics: Option<ActionInvocationDiagnostics>,
}
