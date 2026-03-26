use std::time::Instant;

use pera_core::RunId;

use crate::error::EvaluatorError;
use crate::traits::{Agent, Environment, Evaluator, NoopEvaluator};
use crate::types::{
    AgentDecision, AgentTurnInput, FinishReason, RunRequest, RunResult, Trajectory,
    TrajectoryEvent,
};

#[derive(Debug)]
pub struct Orchestrator<A, E, V> {
    agent: A,
    environment: E,
    evaluator: Option<V>,
}

impl<A, E> Orchestrator<A, E, NoopEvaluator> {
    pub fn new(agent: A, environment: E) -> Self {
        Self {
            agent,
            environment,
            evaluator: None,
        }
    }
}

impl<A, E, V> Orchestrator<A, E, V> {
    pub fn with_evaluator(agent: A, environment: E, evaluator: V) -> Self {
        Self {
            agent,
            environment,
            evaluator: Some(evaluator),
        }
    }
}

impl<A, E, V> Orchestrator<A, E, V>
where
    A: Agent<
            Observation = E::Observation,
            Action = E::Action,
            Outcome = E::Outcome,
        >,
    E: Environment,
    V: Evaluator<E::Observation, E::Action, E::Outcome>,
{
    pub async fn run(
        &mut self,
        request: RunRequest,
    ) -> Result<RunResult<E::Observation, E::Action, E::Outcome>, EvaluatorError> {
        let run_id = RunId::generate();
        let started_at = Instant::now();
        let mut step_count = 0usize;
        let mut action_count = 0usize;
        let mut message_count = 0usize;
        let mut evaluation = None;
        let mut trajectory = Trajectory::new(run_id);
        trajectory
            .events
            .push(TrajectoryEvent::SessionStarted {
                task: request.task.clone(),
            });

        let mut observation = match self.environment.reset(&request.task).await {
            Ok(observation) => observation,
            Err(error) => {
                let reason = FinishReason::EnvironmentError(error.to_string());
                trajectory
                    .events
                    .push(TrajectoryEvent::SessionFinished {
                        reason: reason.clone(),
                    });
                return Ok(RunResult {
                    run_id,
                    finish_reason: reason,
                    trajectory,
                    evaluation: None,
                });
            }
        };
        trajectory
            .events
            .push(TrajectoryEvent::ObservationRecorded {
                observation: observation.clone(),
            });

        let finish_reason = loop {
            if step_count >= request.limits.max_steps {
                break FinishReason::StepLimitExceeded;
            }
            if let Some(max_duration) = request.limits.max_duration {
                if started_at.elapsed() >= max_duration {
                    break FinishReason::TimeLimitExceeded;
                }
            }
            if let Some(reason) = self.environment.terminal_status().await.map_err(|error| {
                EvaluatorError::new(format!("failed to query terminal status: {error}"))
            })? {
                break FinishReason::EnvironmentTerminated(reason);
            }

            let input = AgentTurnInput {
                run_id,
                task: request.task.clone(),
                limits: request.limits,
                observation: observation.clone(),
                trajectory: trajectory.clone(),
            };

            match self.agent.next_decision(input).await {
                Ok(AgentDecision::Message { content }) => {
                    step_count += 1;
                    message_count += 1;
                    trajectory
                        .events
                        .push(TrajectoryEvent::AgentMessage { content });
                    if message_count > request.limits.max_messages {
                        break FinishReason::MessageLimitExceeded;
                    }
                }
                Ok(AgentDecision::EnvironmentAction { action }) => {
                    step_count += 1;
                    action_count += 1;
                    trajectory
                        .events
                        .push(TrajectoryEvent::AgentActionRequested {
                            action: action.clone(),
                        });
                    if action_count > request.limits.max_actions {
                        break FinishReason::ActionLimitExceeded;
                    }
                    match self.environment.step(action.clone()).await {
                        Ok(outcome) => {
                            trajectory.events.push(
                                TrajectoryEvent::EnvironmentActionCompleted {
                                    action,
                                    outcome,
                                },
                            );
                            observation = self.environment.observe().await.map_err(|error| {
                                EvaluatorError::new(format!(
                                    "failed to refresh observation after action: {error}"
                                ))
                            })?;
                            trajectory.events.push(TrajectoryEvent::ObservationRecorded {
                                observation: observation.clone(),
                            });
                        }
                        Err(error) => {
                            trajectory
                                .events
                                .push(TrajectoryEvent::EnvironmentActionFailed {
                                    action,
                                    error: error.to_string(),
                                });
                            break FinishReason::EnvironmentError(error.to_string());
                        }
                    }
                }
                Ok(AgentDecision::Finish { reason }) => {
                    break reason;
                }
                Err(error) => {
                    break FinishReason::AgentError(error.to_string());
                }
            }
        };

        trajectory
            .events
            .push(TrajectoryEvent::SessionFinished {
                reason: finish_reason.clone(),
            });

        if let Some(evaluator) = &self.evaluator {
            let result = evaluator.evaluate(&request.task, &trajectory).await?;
            trajectory
                .events
                .push(TrajectoryEvent::EvaluationCompleted {
                    result: result.clone(),
                });
            evaluation = Some(result);
        }

        Ok(RunResult {
            run_id,
            finish_reason,
            trajectory,
            evaluation,
        })
    }
}
