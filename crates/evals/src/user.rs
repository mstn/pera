use std::marker::PhantomData;

use async_trait::async_trait;
use pera_orchestrator::{
    Participant, ParticipantDecision, ParticipantError, ParticipantId, ParticipantInboxEvent,
    ParticipantInput, ParticipantOutput, TrajectoryEvent,
};

use crate::spec::EvalUserSpec;

#[derive(Debug, Clone)]
pub struct ScriptedUserParticipant<O, A, U> {
    initial_message: String,
    _marker: PhantomData<fn() -> (O, A, U)>,
}

impl<O, A, U> ScriptedUserParticipant<O, A, U> {
    pub fn from_spec(spec: &EvalUserSpec) -> Self {
        let initial_message = spec
            .example_messages
            .first()
            .cloned()
            .unwrap_or_else(|| spec.task.clone());
        Self {
            initial_message,
            _marker: PhantomData,
        }
    }
}

#[async_trait]
impl<O, A, U> Participant for ScriptedUserParticipant<O, A, U>
where
    O: Clone + Send + Sync + 'static,
    A: Clone + Send + Sync + 'static,
    U: Clone + Send + Sync + 'static,
{
    type Observation = O;
    type Action = A;
    type Outcome = U;

    fn id(&self) -> ParticipantId {
        ParticipantId::User
    }

    async fn respond(
        &mut self,
        input: ParticipantInput<Self::Observation, Self::Action, Self::Outcome>,
        _output: &mut dyn ParticipantOutput<Self::Action, Self::Outcome>,
    ) -> Result<ParticipantDecision<Self::Action>, ParticipantError> {
        let user_already_spoke = input.trajectory.events.iter().any(|event| {
            matches!(
                event,
                TrajectoryEvent::ParticipantMessage {
                    participant: ParticipantId::User,
                    ..
                }
            )
        }) || input.inbox.iter().any(|event| {
            matches!(
                event,
                ParticipantInboxEvent::Message {
                    from: ParticipantId::User,
                    ..
                }
            )
        });

        if user_already_spoke {
            return Ok(ParticipantDecision::Yield);
        }

        Ok(ParticipantDecision::FinalMessage {
            content: self.initial_message.clone(),
        })
    }
}
