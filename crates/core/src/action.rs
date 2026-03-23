use crate::{ActionId, ActionName, RunId, Value};

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ActionRequest {
    pub id: ActionId,
    pub run_id: RunId,
    pub action_name: ActionName,
    pub arguments: Vec<Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ActionResult {
    pub action_id: ActionId,
    pub value: Value,
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
