use async_trait::async_trait;
pub use pera_runtime::{
    CodeEnvironment, CodeEnvironmentAction as CodeAction, CodeEnvironmentError,
    CodeEnvironmentObservation as CodeObservation, CodeEnvironmentOutcome as CodeOutcome,
    CodeEnvironmentSnapshot as CodeSnapshot,
};

use crate::error::EnvironmentError;
use crate::traits::Environment;
use crate::types::TaskSpec;

#[derive(Debug, Clone)]
pub struct RuntimeCodeEnvironment {
    inner: CodeEnvironment,
}

impl RuntimeCodeEnvironment {
    pub fn new(inner: CodeEnvironment) -> Self {
        Self { inner }
    }

    pub fn into_inner(self) -> CodeEnvironment {
        self.inner
    }
}

#[async_trait]
impl Environment for RuntimeCodeEnvironment {
    type Observation = CodeObservation;
    type Action = CodeAction;
    type Outcome = CodeOutcome;
    type Snapshot = CodeSnapshot;

    async fn reset(&mut self, _task: &TaskSpec) -> Result<Self::Observation, EnvironmentError> {
        self.inner
            .reset()
            .await
            .map_err(|error| EnvironmentError::new(error.to_string()))
    }

    async fn observe(&self) -> Result<Self::Observation, EnvironmentError> {
        self.inner
            .observe()
            .await
            .map_err(|error| EnvironmentError::new(error.to_string()))
    }

    async fn step(
        &mut self,
        action: Self::Action,
    ) -> Result<Self::Outcome, EnvironmentError> {
        self.inner
            .step(action)
            .await
            .map_err(|error| EnvironmentError::new(error.to_string()))
    }

    async fn snapshot(&self) -> Result<Self::Snapshot, EnvironmentError> {
        self.inner
            .snapshot()
            .await
            .map_err(|error| EnvironmentError::new(error.to_string()))
    }

    async fn restore(&mut self, snapshot: &Self::Snapshot) -> Result<(), EnvironmentError> {
        self.inner
            .restore(snapshot)
            .await
            .map_err(|error| EnvironmentError::new(error.to_string()))
    }

    async fn terminal_status(&self) -> Result<Option<String>, EnvironmentError> {
        Ok(None)
    }
}
