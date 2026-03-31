use async_trait::async_trait;

use crate::error::ParticipantError;
use crate::types::{ActionError, ParticipantId};

#[async_trait]
pub trait ParticipantOutput<A, U = ()>: Send {
    async fn message_start(
        &mut self,
        participant: &ParticipantId,
    ) -> Result<(), ParticipantError>;

    async fn message_delta(
        &mut self,
        participant: &ParticipantId,
        delta: &str,
    ) -> Result<(), ParticipantError>;

    async fn message_end(
        &mut self,
        participant: &ParticipantId,
    ) -> Result<(), ParticipantError>;

    async fn status_update(
        &mut self,
        participant: &ParticipantId,
        _status: &str,
    ) -> Result<(), ParticipantError> {
        let _ = participant;
        Ok(())
    }

    async fn tool_call_start(
        &mut self,
        participant: &ParticipantId,
        _tool_name: &str,
    ) -> Result<(), ParticipantError> {
        let _ = participant;
        Ok(())
    }

    async fn tool_call_delta(
        &mut self,
        participant: &ParticipantId,
        _tool_name: &str,
        _delta: &str,
    ) -> Result<(), ParticipantError> {
        let _ = participant;
        Ok(())
    }

    async fn tool_call_end(
        &mut self,
        participant: &ParticipantId,
        _tool_name: &str,
    ) -> Result<(), ParticipantError> {
        let _ = participant;
        Ok(())
    }

    async fn action_planned(
        &mut self,
        participant: &ParticipantId,
        _action: &A,
    ) -> Result<(), ParticipantError> {
        let _ = participant;
        Ok(())
    }

    async fn action_completed(
        &mut self,
        participant: &ParticipantId,
        _action: &A,
        _outcome: &U,
    ) -> Result<(), ParticipantError> {
        let _ = participant;
        Ok(())
    }

    async fn action_failed(
        &mut self,
        participant: &ParticipantId,
        _action: &A,
        _error: &ActionError,
    ) -> Result<(), ParticipantError> {
        let _ = participant;
        Ok(())
    }
}

#[derive(Debug, Default)]
pub struct NoopParticipantOutput;

#[async_trait]
impl<A, U> ParticipantOutput<A, U> for NoopParticipantOutput
where
    A: Send + Sync + 'static,
    U: Send + Sync + 'static,
{
    async fn message_start(
        &mut self,
        _participant: &ParticipantId,
    ) -> Result<(), ParticipantError> {
        Ok(())
    }

    async fn message_delta(
        &mut self,
        _participant: &ParticipantId,
        _delta: &str,
    ) -> Result<(), ParticipantError> {
        Ok(())
    }

    async fn message_end(
        &mut self,
        _participant: &ParticipantId,
    ) -> Result<(), ParticipantError> {
        Ok(())
    }
}
