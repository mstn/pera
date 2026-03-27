use std::collections::{BTreeMap, BTreeSet};
use std::time::Instant;

use pera_core::{ActionId, RunId};

use crate::error::EvaluatorError;
use crate::streaming::{NoopParticipantOutput, ParticipantOutput};
use crate::traits::{Environment, Evaluator, NoopEvaluator, Participant};
use crate::types::{
    ActionExecution, EnvironmentEvent, FinishReason, ParticipantDecision, ParticipantId,
    ParticipantInboxEvent, ParticipantTurnInput, RunRequest, RunResult, SubmittedAction,
    TerminationCondition, Trajectory, TrajectoryEvent,
};

type BoxedParticipant<O, A, U> = Box<dyn Participant<Observation = O, Action = A, Outcome = U>>;

pub struct Orchestrator<E, V>
where
    E: Environment,
{
    environment: E,
    participants: Vec<BoxedParticipant<E::Observation, E::Action, E::Outcome>>,
    evaluator: Option<V>,
    next_participant_index: usize,
}

impl<E> Orchestrator<E, NoopEvaluator>
where
    E: Environment,
{
    pub fn new(
        participant: impl Participant<
            Observation = E::Observation,
            Action = E::Action,
            Outcome = E::Outcome,
        > + 'static,
        _environment: E,
    ) -> Self {
        Self::from_participants(vec![Box::new(participant)], _environment)
    }

    pub fn from_participants(
        participants: Vec<BoxedParticipant<E::Observation, E::Action, E::Outcome>>,
        environment: E,
    ) -> Self {
        Self {
            environment,
            participants,
            evaluator: None,
            next_participant_index: 0,
        }
    }
}

impl<E, V> Orchestrator<E, V>
where
    E: Environment,
{
    pub fn with_evaluator(
        participant: impl Participant<
            Observation = E::Observation,
            Action = E::Action,
            Outcome = E::Outcome,
        > + 'static,
        _environment: E,
        evaluator: V,
    ) -> Self {
        Self::with_participants_and_evaluator(vec![Box::new(participant)], _environment, evaluator)
    }

    pub fn with_participants_and_evaluator(
        participants: Vec<BoxedParticipant<E::Observation, E::Action, E::Outcome>>,
        environment: E,
        evaluator: V,
    ) -> Self {
        Self {
            environment,
            participants,
            evaluator: Some(evaluator),
            next_participant_index: 0,
        }
    }
}

impl<E, V> Orchestrator<E, V>
where
    E: Environment,
    V: Evaluator<E::Observation, E::Action, E::Outcome>,
{
    pub async fn run(
        &mut self,
        request: RunRequest,
    ) -> Result<RunResult<E::Observation, E::Action, E::Outcome>, EvaluatorError> {
        let mut output = NoopParticipantOutput;
        self.run_with_output(request, &mut output).await
    }

    pub async fn run_with_output(
        &mut self,
        request: RunRequest,
        output: &mut dyn ParticipantOutput<E::Action>,
    ) -> Result<RunResult<E::Observation, E::Action, E::Outcome>, EvaluatorError> {
        let run_id = RunId::generate();
        let started_at = Instant::now();
        let mut step_count = 0usize;
        let mut action_count = 0usize;
        let mut message_count = 0usize;
        let mut evaluation = None;
        let mut pending_actions = BTreeSet::<ActionId>::new();
        let mut blocked_participants = BTreeMap::<ParticipantId, ActionId>::new();
        let mut finished_participants = BTreeSet::<ParticipantId>::new();
        let mut inboxes =
            BTreeMap::<ParticipantId, Vec<ParticipantInboxEvent<E::Action, E::Outcome>>>::new();

        let mut trajectory = Trajectory::new(run_id);
        trajectory.events.push(TrajectoryEvent::SessionStarted {
            task: request.task.clone(),
        });

        let mut observation = match self.environment.reset(&request.task).await {
            Ok(observation) => observation,
            Err(error) => {
                let reason = FinishReason::EnvironmentError(error.to_string());
                trajectory.events.push(TrajectoryEvent::SessionFinished {
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

            let mut polled_events = self.environment.poll_events().await.map_err(|error| {
                EvaluatorError::new(format!("failed to poll environment events: {error}"))
            })?;
            if !polled_events.is_empty() {
                self.route_environment_events(
                    &mut polled_events,
                    &mut trajectory,
                    &mut inboxes,
                    &mut pending_actions,
                    &mut blocked_participants,
                );
                observation = self.environment.observe().await.map_err(|error| {
                    EvaluatorError::new(format!(
                        "failed to refresh observation after environment events: {error}"
                    ))
                })?;
                trajectory
                    .events
                    .push(TrajectoryEvent::ObservationRecorded {
                        observation: observation.clone(),
                    });
            }

            if let Some(reason) = termination_condition_met(
                &request.termination_condition,
                &finished_participants,
                self.participants.len(),
                None,
            ) {
                break reason;
            }

            let Some(participant_index) =
                self.next_runnable_participant(&finished_participants, &blocked_participants)
            else {
                if pending_actions.is_empty() {
                    break FinishReason::Deadlocked;
                }
                continue;
            };
            let participant = &mut self.participants[participant_index];
            let participant_id = participant.id();
            let inbox = inboxes.remove(&participant_id).unwrap_or_default();
            let input = ParticipantTurnInput {
                run_id,
                participant: participant_id.clone(),
                task: request.task.clone(),
                limits: request.limits,
                observation: observation.clone(),
                inbox,
                trajectory: trajectory.clone(),
            };

            match participant.run_turn(input, output).await {
                Ok(ParticipantDecision::Message { content }) => {
                    step_count += 1;
                    message_count += 1;
                    trajectory.events.push(TrajectoryEvent::ParticipantMessage {
                        participant: participant_id.clone(),
                        content,
                    });
                    if message_count > request.limits.max_messages {
                        break FinishReason::MessageLimitExceeded;
                    }
                }
                Ok(ParticipantDecision::Action { action, execution }) => {
                    step_count += 1;
                    action_count += 1;
                    trajectory.events.push(TrajectoryEvent::ActionRequested {
                        participant: participant_id.clone(),
                        action: action.clone(),
                        execution,
                    });
                    if action_count > request.limits.max_actions {
                        break FinishReason::ActionLimitExceeded;
                    }

                    match execution {
                        ActionExecution::Immediate => {
                            output
                                .action_planned(&participant_id, &action)
                                .await
                                .map_err(|error| {
                                    EvaluatorError::new(format!(
                                        "failed to emit action planning output: {error}"
                                    ))
                                })?;
                            let action_id = ActionId::generate();
                            match self.environment.step(participant_id.clone(), action).await {
                                Ok(outcome) => {
                                    trajectory.events.push(TrajectoryEvent::ActionCompleted {
                                        participant: participant_id,
                                        action_id,
                                        outcome,
                                    });
                                    observation = self.environment.observe().await.map_err(|error| {
                                        EvaluatorError::new(format!(
                                            "failed to refresh observation after immediate action: {error}"
                                        ))
                                    })?;
                                    trajectory
                                        .events
                                        .push(TrajectoryEvent::ObservationRecorded {
                                            observation: observation.clone(),
                                        });
                                }
                                Err(error) => {
                                    trajectory.events.push(TrajectoryEvent::ActionFailed {
                                        participant: participant_id,
                                        action_id,
                                        error: error.to_string(),
                                    });
                                    break FinishReason::EnvironmentError(error.to_string());
                                }
                            }
                        }
                        ActionExecution::DeferredBlocking
                        | ActionExecution::DeferredNonBlocking => {
                            let blocking = execution == ActionExecution::DeferredBlocking;
                            output
                                .action_planned(&participant_id, &action)
                                .await
                                .map_err(|error| {
                                    EvaluatorError::new(format!(
                                        "failed to emit action planning output: {error}"
                                    ))
                                })?;
                            match self
                                .environment
                                .submit(participant_id.clone(), action.clone())
                                .await
                            {
                                Ok(SubmittedAction { action_id }) => {
                                    trajectory.events.push(TrajectoryEvent::ActionSubmitted {
                                        participant: participant_id.clone(),
                                        action_id,
                                        action: action.clone(),
                                        execution,
                                    });
                                    inboxes.entry(participant_id.clone()).or_default().push(
                                        ParticipantInboxEvent::ActionAccepted { action_id, action },
                                    );
                                    pending_actions.insert(action_id);
                                    if blocking {
                                        blocked_participants.insert(participant_id, action_id);
                                    }
                                }
                                Err(error) => {
                                    break FinishReason::EnvironmentError(error.to_string());
                                }
                            }
                        }
                    }
                }
                Ok(ParticipantDecision::Yield) => {
                    step_count += 1;
                    trajectory.events.push(TrajectoryEvent::ParticipantYielded {
                        participant: participant_id,
                    });
                }
                Ok(ParticipantDecision::Finish) => {
                    finished_participants.insert(participant_id.clone());
                    trajectory
                        .events
                        .push(TrajectoryEvent::ParticipantFinished {
                            participant: participant_id.clone(),
                        });
                    if let Some(reason) = termination_condition_met(
                        &request.termination_condition,
                        &finished_participants,
                        self.participants.len(),
                        Some(&participant_id),
                    ) {
                        break reason;
                    }
                }
                Err(error) => {
                    break FinishReason::ParticipantError {
                        participant: participant_id,
                        message: error.to_string(),
                    };
                }
            }
        };

        trajectory.events.push(TrajectoryEvent::SessionFinished {
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

    fn next_runnable_participant(
        &mut self,
        finished_participants: &BTreeSet<ParticipantId>,
        blocked_participants: &BTreeMap<ParticipantId, ActionId>,
    ) -> Option<usize> {
        if self.participants.is_empty() {
            return None;
        }

        for offset in 0..self.participants.len() {
            let index = (self.next_participant_index + offset) % self.participants.len();
            let participant_id = self.participants[index].id();
            if finished_participants.contains(&participant_id)
                || blocked_participants.contains_key(&participant_id)
            {
                continue;
            }

            self.next_participant_index = (index + 1) % self.participants.len();
            return Some(index);
        }

        None
    }

    fn route_environment_events(
        &self,
        events: &mut Vec<EnvironmentEvent<E::Action, E::Outcome>>,
        trajectory: &mut Trajectory<E::Observation, E::Action, E::Outcome>,
        inboxes: &mut BTreeMap<ParticipantId, Vec<ParticipantInboxEvent<E::Action, E::Outcome>>>,
        pending_actions: &mut BTreeSet<ActionId>,
        blocked_participants: &mut BTreeMap<ParticipantId, ActionId>,
    ) {
        for event in events.drain(..) {
            match event {
                EnvironmentEvent::ActionAccepted {
                    participant,
                    action_id,
                    action,
                } => {
                    inboxes
                        .entry(participant)
                        .or_default()
                        .push(ParticipantInboxEvent::ActionAccepted { action_id, action });
                }
                EnvironmentEvent::ActionCompleted {
                    participant,
                    action_id,
                    outcome,
                } => {
                    pending_actions.remove(&action_id);
                    blocked_participants
                        .retain(|_, blocked_action_id| *blocked_action_id != action_id);
                    trajectory.events.push(TrajectoryEvent::ActionCompleted {
                        participant: participant.clone(),
                        action_id,
                        outcome: outcome.clone(),
                    });
                    inboxes
                        .entry(participant)
                        .or_default()
                        .push(ParticipantInboxEvent::ActionCompleted { action_id, outcome });
                }
                EnvironmentEvent::ActionFailed {
                    participant,
                    action_id,
                    error,
                } => {
                    pending_actions.remove(&action_id);
                    blocked_participants
                        .retain(|_, blocked_action_id| *blocked_action_id != action_id);
                    trajectory.events.push(TrajectoryEvent::ActionFailed {
                        participant: participant.clone(),
                        action_id,
                        error: error.clone(),
                    });
                    inboxes
                        .entry(participant)
                        .or_default()
                        .push(ParticipantInboxEvent::ActionFailed { action_id, error });
                }
                EnvironmentEvent::Notification {
                    participant,
                    message,
                } => {
                    inboxes
                        .entry(participant)
                        .or_default()
                        .push(ParticipantInboxEvent::Notification { message });
                }
            }
        }
    }
}

fn termination_condition_met(
    condition: &TerminationCondition,
    finished_participants: &BTreeSet<ParticipantId>,
    participant_count: usize,
    newly_finished_participant: Option<&ParticipantId>,
) -> Option<FinishReason> {
    match condition {
        TerminationCondition::AllParticipantsFinished => {
            if participant_count > 0 && finished_participants.len() == participant_count {
                Some(FinishReason::ParticipantsFinished)
            } else {
                None
            }
        }
        TerminationCondition::AnyParticipantFinished => newly_finished_participant
            .cloned()
            .map(|participant| FinishReason::ParticipantFinished { participant }),
        TerminationCondition::AnyOfParticipantsFinished(participants) => {
            let newly_finished_participant = newly_finished_participant?;
            if participants.contains(newly_finished_participant) {
                Some(FinishReason::ParticipantFinished {
                    participant: newly_finished_participant.clone(),
                })
            } else {
                None
            }
        }
    }
}
