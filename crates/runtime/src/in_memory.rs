use std::collections::BTreeMap;

use pera_core::{
    ActionId, ActionRecord, EventPublisher, ExecutionEvent, ExecutionSession, RunId, RunStore,
    StoreError,
};

#[derive(Debug, Default)]
pub struct InMemoryRunStore {
    runs: BTreeMap<RunId, ExecutionSession>,
    actions: BTreeMap<ActionId, ActionRecord>,
}

impl InMemoryRunStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl RunStore for InMemoryRunStore {
    fn create_run(&mut self, session: ExecutionSession) -> Result<(), StoreError> {
        self.runs.insert(session.id, session);
        Ok(())
    }

    fn save_run(&mut self, session: ExecutionSession) -> Result<(), StoreError> {
        self.runs.insert(session.id, session);
        Ok(())
    }

    fn load_run(&self, run_id: RunId) -> Result<ExecutionSession, StoreError> {
        self.runs
            .get(&run_id)
            .cloned()
            .ok_or_else(|| StoreError::new(format!("run {run_id} not found")))
    }

    fn list_runs(&self) -> Result<Vec<RunId>, StoreError> {
        Ok(self.runs.keys().copied().collect())
    }

    fn save_action(&mut self, action: ActionRecord) -> Result<(), StoreError> {
        self.actions.insert(action.request.id, action);
        Ok(())
    }

    fn load_action(&self, action_id: ActionId) -> Result<ActionRecord, StoreError> {
        self.actions
            .get(&action_id)
            .cloned()
            .ok_or_else(|| StoreError::new(format!("action {action_id} not found")))
    }

    fn list_actions(&self) -> Result<Vec<ActionId>, StoreError> {
        Ok(self.actions.keys().copied().collect())
    }
}

#[derive(Debug, Default)]
pub struct RecordingEventPublisher {
    events: Vec<ExecutionEvent>,
}

impl RecordingEventPublisher {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn events(&self) -> &[ExecutionEvent] {
        &self.events
    }
}

impl EventPublisher for RecordingEventPublisher {
    fn publish(&mut self, event: ExecutionEvent) -> Result<(), StoreError> {
        self.events.push(event);
        Ok(())
    }
}
