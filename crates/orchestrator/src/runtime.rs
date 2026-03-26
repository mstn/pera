use async_trait::async_trait;
pub use pera_runtime::{
    CodeEnvironment, CodeEnvironmentAction as CodeAction, CodeEnvironmentError,
    CodeEnvironmentEvent, CodeEnvironmentObservation as CodeObservation,
    CodeEnvironmentOutcome as CodeOutcome, CodeEnvironmentSnapshot as CodeSnapshot,
    SubmittedCodeAction,
};

use crate::error::EnvironmentError;
use crate::traits::Environment;
use crate::types::{EnvironmentEvent, ParticipantId, SubmittedAction, TaskSpec};

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
        _actor: ParticipantId,
        action: Self::Action,
    ) -> Result<Self::Outcome, EnvironmentError> {
        self.inner
            .step(action)
            .await
            .map_err(|error| EnvironmentError::new(error.to_string()))
    }

    async fn submit(
        &mut self,
        actor: ParticipantId,
        action: Self::Action,
    ) -> Result<SubmittedAction, EnvironmentError> {
        self.inner
            .submit(format_participant_id(&actor), action)
            .await
            .map(|submitted| SubmittedAction {
                action_id: submitted.action_id,
            })
            .map_err(|error| EnvironmentError::new(error.to_string()))
    }

    async fn poll_events(
        &mut self,
    ) -> Result<Vec<EnvironmentEvent<Self::Action, Self::Outcome>>, EnvironmentError> {
        let events = self
            .inner
            .poll_events()
            .await
            .map_err(|error| EnvironmentError::new(error.to_string()))?;
        Ok(events
            .into_iter()
            .map(runtime_event_to_orchestrator_event)
            .collect())
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

fn format_participant_id(participant: &ParticipantId) -> String {
    match participant {
        ParticipantId::Agent => "agent".to_owned(),
        ParticipantId::User => "user".to_owned(),
        ParticipantId::Custom(value) => value.clone(),
    }
}

fn parse_participant_id(value: String) -> ParticipantId {
    match value.as_str() {
        "agent" => ParticipantId::Agent,
        "user" => ParticipantId::User,
        _ => ParticipantId::Custom(value),
    }
}

fn runtime_event_to_orchestrator_event(
    event: CodeEnvironmentEvent,
) -> EnvironmentEvent<CodeAction, CodeOutcome> {
    match event {
        CodeEnvironmentEvent::ActionAccepted {
            actor,
            action_id,
            action,
        } => EnvironmentEvent::ActionAccepted {
            participant: parse_participant_id(actor),
            action_id,
            action,
        },
        CodeEnvironmentEvent::ActionCompleted {
            actor,
            action_id,
            outcome,
        } => EnvironmentEvent::ActionCompleted {
            participant: parse_participant_id(actor),
            action_id,
            outcome,
        },
        CodeEnvironmentEvent::ActionFailed {
            actor,
            action_id,
            error,
        } => EnvironmentEvent::ActionFailed {
            participant: parse_participant_id(actor),
            action_id,
            error,
        },
        CodeEnvironmentEvent::Notification { actor, message } => {
            EnvironmentEvent::Notification {
                participant: parse_participant_id(actor),
                message,
            }
        }
    }
}
