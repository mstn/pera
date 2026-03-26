use async_trait::async_trait;

use crate::error::{AgentError, EnvironmentError, EvaluatorError};
use crate::types::{AgentDecision, AgentTurnInput, EvalResult, TaskSpec, Trajectory};

#[async_trait]
pub trait Agent: Send {
    type Observation: Clone + Send + Sync + 'static;
    type Action: Clone + Send + Sync + 'static;
    type Outcome: Clone + Send + Sync + 'static;

    async fn next_decision(
        &mut self,
        input: AgentTurnInput<Self::Observation, Self::Action, Self::Outcome>,
    ) -> Result<AgentDecision<Self::Action>, AgentError>;
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
        action: Self::Action,
    ) -> Result<Self::Outcome, EnvironmentError>;
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
