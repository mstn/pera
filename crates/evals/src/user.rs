use async_trait::async_trait;
use pera_llm::LlmProvider;
use pera_orchestrator::{
    Participant, ParticipantDecision, ParticipantError, ParticipantId, ParticipantInput,
    ParticipantOutput,
};

use crate::scripted_user::ScriptedUserParticipant;
use crate::simulated_user::SimulatedUserParticipant;
use crate::spec::{EvalUserMode, EvalUserSpec};

pub enum EvalUserParticipant<P, O, A, U> {
    Scripted(ScriptedUserParticipant<O, A, U>),
    Simulated(SimulatedUserParticipant<P, O, A, U>),
}

impl<P, O, A, U> EvalUserParticipant<P, O, A, U> {
    pub fn from_spec(provider: P, spec: EvalUserSpec) -> Self {
        match spec.mode {
            EvalUserMode::Scripted => Self::Scripted(ScriptedUserParticipant::from_spec(&spec)),
            EvalUserMode::Simulated => Self::Simulated(SimulatedUserParticipant::new(provider, spec)),
        }
    }
}

#[async_trait]
impl<P, O, A, U> Participant for EvalUserParticipant<P, O, A, U>
where
    P: LlmProvider + Send + Sync + 'static,
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
        output: &mut dyn ParticipantOutput<Self::Action, Self::Outcome>,
    ) -> Result<ParticipantDecision<Self::Action>, ParticipantError> {
        match self {
            Self::Scripted(participant) => participant.respond(input, output).await,
            Self::Simulated(participant) => participant.respond(input, output).await,
        }
    }
}
