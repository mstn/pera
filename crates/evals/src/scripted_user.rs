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
        let EvalUserSpec::Scripted {
            known_info,
            initial_message,
            ..
        } = spec
        else {
            panic!("scripted user participant requires a scripted user spec");
        };
        let initial_message = if known_info.trim().is_empty() {
            initial_message.clone()
        } else {
            format!(
                "{initial_message}\n\nKnown information:\n{}",
                known_info.trim()
            )
        };
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
            return Ok(ParticipantDecision::Finish);
        }

        Ok(ParticipantDecision::FinalMessage {
            content: self.initial_message.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::ScriptedUserParticipant;
    use crate::spec::EvalUserSpec;

    #[test]
    fn scripted_user_initial_message_includes_known_info() {
        let participant =
            ScriptedUserParticipant::<(), (), ()>::from_spec(&EvalUserSpec::Scripted {
                task: "Plan the trip".to_owned(),
                known_info:
                    "The cost center is DELTA-EU. The planning week starts on 2026-04-06."
                        .to_owned(),
                initial_message: "Plan Berlin travel for Alice and Bruno.".to_owned(),
            });

        assert_eq!(
            participant.initial_message,
            "Plan Berlin travel for Alice and Bruno.\n\nKnown information:\nThe cost center is DELTA-EU. The planning week starts on 2026-04-06."
        );
    }
}
