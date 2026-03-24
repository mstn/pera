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
pub enum ActionStatus {
    Pending,
    Completed(ActionResult),
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ActionRecord {
    pub request: ActionRequest,
    pub status: ActionStatus,
}
