use async_trait::async_trait;

use crate::error::{EnvironmentError, EvaluatorError, ParticipantError};
use crate::streaming::ParticipantOutput;
use crate::types::{
    EnvironmentEvent, EvalResult, ParticipantDecision, ParticipantId, ParticipantTurnInput,
    SubmittedAction, TaskSpec, Trajectory,
};

#[async_trait]
pub trait Participant: Send {
    type Observation: Clone + Send + Sync + 'static;
    type Action: Clone + Send + Sync + 'static;
    type Outcome: Clone + Send + Sync + 'static;

    fn id(&self) -> ParticipantId;

    async fn run_turn(
        &mut self,
        input: ParticipantTurnInput<Self::Observation, Self::Action, Self::Outcome>,
        output: &mut dyn ParticipantOutput<Self::Action>,
    ) -> Result<ParticipantDecision<Self::Action>, ParticipantError>;
}

#[async_trait]
pub trait Environment: Send {
    type Observation: Clone + Send + Sync + 'static;
    type Action: Clone + Send + Sync + 'static;
    type Outcome: Clone + Send + Sync + 'static;
    type Snapshot: Clone + Send + Sync + 'static;

    async fn reset(&mut self, task: &TaskSpec) -> Result<Self::Observation, EnvironmentError>;
    async fn observe(&self) -> Result<Self::Observation, EnvironmentError>;
    async fn step(
        &mut self,
        actor: ParticipantId,
        action: Self::Action,
    ) -> Result<Self::Outcome, EnvironmentError>;
    async fn submit(
        &mut self,
        actor: ParticipantId,
        action: Self::Action,
    ) -> Result<SubmittedAction, EnvironmentError>;
    async fn poll_events(
        &mut self,
    ) -> Result<Vec<EnvironmentEvent<Self::Action, Self::Outcome>>, EnvironmentError>;
    async fn snapshot(&self) -> Result<Self::Snapshot, EnvironmentError>;
    async fn restore(&mut self, snapshot: &Self::Snapshot) -> Result<(), EnvironmentError>;
    async fn terminal_status(&self) -> Result<Option<String>, EnvironmentError>;
}

#[async_trait]
pub trait Evaluator<O, A, U>: Send + Sync {
    async fn evaluate(
        &self,
        task: &TaskSpec,
        trajectory: &Trajectory<O, A, U>,
    ) -> Result<EvalResult, EvaluatorError>;
}

#[derive(Debug, Clone, Copy)]
pub struct NoopEvaluator;

#[async_trait]
impl<O, A, U> Evaluator<O, A, U> for NoopEvaluator
where
    O: Clone + Send + Sync + 'static,
    A: Clone + Send + Sync + 'static,
    U: Clone + Send + Sync + 'static,
{
    async fn evaluate(
        &self,
        _task: &TaskSpec,
        _trajectory: &Trajectory<O, A, U>,
    ) -> Result<EvalResult, EvaluatorError> {
        Ok(EvalResult {
            passed: true,
            score: None,
            summary: None,
        })
    }
}
