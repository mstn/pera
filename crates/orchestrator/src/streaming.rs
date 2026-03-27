use async_trait::async_trait;

use crate::error::ParticipantError;
use crate::types::ParticipantId;

#[async_trait]
pub trait ParticipantOutput<A>: Send {
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

    async fn action_planned(
        &mut self,
        participant: &ParticipantId,
        _action: &A,
    ) -> Result<(), ParticipantError> {
        let _ = participant;
        Ok(())
    }
}

#[derive(Debug, Default)]
pub struct NoopParticipantOutput;

#[async_trait]
impl<A> ParticipantOutput<A> for NoopParticipantOutput
where
    A: Send + Sync + 'static,
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
