use std::collections::{BTreeMap, BTreeSet};
use std::time::Instant;

use pera_core::{ActionId, RunId, WorkItemId};

use crate::error::EvaluatorError;
use crate::streaming::{NoopParticipantOutput, ParticipantOutput};
use crate::traits::{Environment, Evaluator, NoopEvaluator, Participant};
use crate::types::{
    ActionExecution, EnvironmentEvent, FinishReason, ParticipantDecision, ParticipantId,
    ParticipantInboxEvent, RunRequest, RunResult, SubmittedAction,
    TerminationCondition, Trajectory, TrajectoryEvent, WorkItem, WorkItemStatus,
    WorkItemContinuationInput,
};

type BoxedParticipant<O, A, U> = Box<dyn Participant<Observation = O, Action = A, Outcome = U>>;

#[derive(Debug, Default)]
struct RunCounters {
    step_count: usize,
    action_count: usize,
    message_count: usize,
}

#[derive(Debug, Clone)]
struct ParticipantRuntimeState<A, U> {
    inbox: Vec<ParticipantInboxEvent<A, U>>,
    current_work_item: Option<WorkItem>,
    blocked_on_action: Option<ActionId>,
    finished: bool,
}

impl<A, U> Default for ParticipantRuntimeState<A, U> {
    fn default() -> Self {
        Self {
            inbox: Vec::new(),
            current_work_item: None,
            blocked_on_action: None,
            finished: false,
        }
    }
}

#[derive(Debug, Clone)]
struct WorkItemContinuation {
    participant_id: ParticipantId,
    participant_index: usize,
    work_item: Option<WorkItem>,
}

struct RunState<O, A, U> {
    observation: O,
    trajectory: Trajectory<O, A, U>,
    pending_actions: BTreeSet<ActionId>,
    action_work_items: BTreeMap<ActionId, WorkItem>,
    participants: BTreeMap<ParticipantId, ParticipantRuntimeState<A, U>>,
}

impl<O, A, U> RunState<O, A, U>
where
    O: Clone,
    A: Clone,
    U: Clone,
{
    fn new(
        run_id: RunId,
        task: &crate::types::TaskSpec,
        observation: O,
        participants: &[ParticipantId],
    ) -> Self {
        let mut trajectory = Trajectory::new(run_id);
        trajectory.events.push(TrajectoryEvent::SessionStarted {
            task: task.clone(),
        });
        trajectory.events.push(TrajectoryEvent::ObservationRecorded {
            observation: observation.clone(),
        });

        let participants = participants
            .iter()
            .cloned()
            .map(|participant| (participant, ParticipantRuntimeState::default()))
            .collect();

        Self {
            observation,
            trajectory,
            pending_actions: BTreeSet::new(),
            action_work_items: BTreeMap::new(),
            participants,
        }
    }

    fn record_observation(&mut self, observation: O) {
        self.observation = observation.clone();
        self.trajectory
            .events
            .push(TrajectoryEvent::ObservationRecorded { observation });
    }

    fn continuation_input(
        &mut self,
        run_id: RunId,
        participant: &ParticipantId,
        request: &RunRequest,
    ) -> WorkItemContinuationInput<O, A, U> {
        let state = self
            .participants
            .get_mut(participant)
            .expect("participant runtime state must exist");
        WorkItemContinuationInput {
            run_id,
            participant: participant.clone(),
            current_work_item: state.current_work_item.clone(),
            task: request.task.clone(),
            limits: request.limits,
            observation: self.observation.clone(),
            inbox: std::mem::take(&mut state.inbox),
            trajectory: self.trajectory.clone(),
        }
    }

    fn work_item_for_message(&mut self, participant: &ParticipantId) -> WorkItem {
        if let Some(work_item) = self
            .participants
            .get(participant)
            .and_then(|state| state.current_work_item.clone())
        {
            return work_item;
        }

        let work_item = WorkItem {
            id: WorkItemId::generate(),
            created_by: participant.clone(),
            status: WorkItemStatus::Active,
        };
        self.trajectory.push(TrajectoryEvent::WorkItemCreated {
            work_item: work_item.clone(),
        });
        self.participants
            .get_mut(participant)
            .expect("participant runtime state must exist")
            .current_work_item = Some(work_item.clone());
        work_item
    }

    fn work_item_for_action(&self, participant: &ParticipantId) -> Option<WorkItem> {
        self.participants
            .get(participant)
            .and_then(|state| state.current_work_item.clone())
    }

    fn complete_work_item(&mut self, work_item_id: WorkItemId) {
        for participant_state in self.participants.values_mut() {
            if participant_state
                .current_work_item
                .as_ref()
                .is_some_and(|work_item| work_item.id == work_item_id)
            {
                participant_state.current_work_item = None;
            }

            for event in &mut participant_state.inbox {
                match event {
                    ParticipantInboxEvent::Message { work_item, .. }
                        if work_item.id == work_item_id =>
                    {
                        work_item.status = WorkItemStatus::Completed;
                    }
                    ParticipantInboxEvent::Message { .. } => {}
                    ParticipantInboxEvent::ActionAccepted { work_item, .. }
                    | ParticipantInboxEvent::ActionCompleted { work_item, .. }
                    | ParticipantInboxEvent::ActionFailed { work_item, .. }
                    | ParticipantInboxEvent::Notification { work_item, .. } => {
                        if let Some(work_item) = work_item.as_mut() {
                            if work_item.id == work_item_id {
                                work_item.status = WorkItemStatus::Completed;
                            }
                        }
                    }
                }
            }
        }
    }

    fn route_participant_message(
        &mut self,
        from: &ParticipantId,
        work_item: &WorkItem,
        content: &str,
    ) {
        for (participant_id, participant_state) in &mut self.participants {
            if participant_id == from || participant_state.finished {
                continue;
            }

            participant_state.inbox.push(ParticipantInboxEvent::Message {
                from: from.clone(),
                work_item: work_item.clone(),
                content: content.to_owned(),
            });
            participant_state.current_work_item = Some(work_item.clone());
        }
    }

    fn mark_participant_finished(&mut self, participant: &ParticipantId) {
        if let Some(state) = self.participants.get_mut(participant) {
            state.finished = true;
            state.current_work_item = None;
            state.blocked_on_action = None;
        }
    }

    fn apply_environment_event(&mut self, event: EnvironmentEvent<A, U>) {
        match event {
            EnvironmentEvent::ActionAccepted {
                participant,
                action_id,
                action,
            } => {
                let work_item = self.action_work_items.get(&action_id).cloned();
                if let Some(work_item) = &work_item {
                    self.participants
                        .get_mut(&participant)
                        .expect("participant runtime state must exist")
                        .current_work_item = Some(work_item.clone());
                }
                self.participants
                    .get_mut(&participant)
                    .expect("participant runtime state must exist")
                    .inbox
                    .push(ParticipantInboxEvent::ActionAccepted {
                        work_item,
                        action_id,
                        action,
                    });
            }
            EnvironmentEvent::ActionCompleted {
                participant,
                action_id,
                outcome,
            } => {
                self.pending_actions.remove(&action_id);
                let work_item = self.action_work_items.remove(&action_id).map(|work_item| WorkItem {
                    status: WorkItemStatus::Active,
                    ..work_item
                });
                if let Some(work_item) = &work_item {
                    self.participants
                        .get_mut(&participant)
                        .expect("participant runtime state must exist")
                        .current_work_item = Some(work_item.clone());
                }
                if let Some(state) = self.participants.get_mut(&participant) {
                    if state.blocked_on_action == Some(action_id) {
                        state.blocked_on_action = None;
                    }
                    state.inbox.push(ParticipantInboxEvent::ActionCompleted {
                        work_item: work_item.clone(),
                        action_id,
                        outcome: outcome.clone(),
                    });
                }
                self.trajectory.push(TrajectoryEvent::ActionCompleted {
                    work_item,
                    participant,
                    action_id,
                    outcome,
                });
            }
            EnvironmentEvent::ActionFailed {
                participant,
                action_id,
                error,
            } => {
                self.pending_actions.remove(&action_id);
                let work_item = self.action_work_items.remove(&action_id).map(|work_item| WorkItem {
                    status: WorkItemStatus::Active,
                    ..work_item
                });
                if let Some(work_item) = &work_item {
                    self.participants
                        .get_mut(&participant)
                        .expect("participant runtime state must exist")
                        .current_work_item = Some(work_item.clone());
                }
                if let Some(state) = self.participants.get_mut(&participant) {
                    if state.blocked_on_action == Some(action_id) {
                        state.blocked_on_action = None;
                    }
                    state.inbox.push(ParticipantInboxEvent::ActionFailed {
                        work_item: work_item.clone(),
                        action_id,
                        error: error.clone(),
                    });
                }
                self.trajectory.push(TrajectoryEvent::ActionFailed {
                    work_item,
                    participant,
                    action_id,
                    error,
                });
            }
            EnvironmentEvent::Notification {
                participant,
                message,
            } => {
                let work_item = self
                    .participants
                    .get(&participant)
                    .and_then(|state| state.current_work_item.clone());
                self.participants
                    .get_mut(&participant)
                    .expect("participant runtime state must exist")
                    .inbox
                    .push(ParticipantInboxEvent::Notification { work_item, message });
            }
        }
    }

    fn finished_participants(&self) -> BTreeSet<ParticipantId> {
        self.participants
            .iter()
            .filter_map(|(participant, state)| state.finished.then_some(participant.clone()))
            .collect()
    }
}

impl<O, A, U> Trajectory<O, A, U> {
    fn push(&mut self, event: TrajectoryEvent<O, A, U>) {
        self.events.push(event);
    }
}

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
        environment: E,
    ) -> Self {
        Self::from_participants(vec![Box::new(participant)], environment)
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
        environment: E,
        evaluator: V,
    ) -> Self {
        Self::with_participants_and_evaluator(vec![Box::new(participant)], environment, evaluator)
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
        let mut counters = RunCounters::default();
        let mut evaluation = None;

        let initial_observation = match self.environment.reset(&request.task).await {
            Ok(observation) => observation,
            Err(error) => {
                let reason = FinishReason::EnvironmentError(error.to_string());
                let mut trajectory = Trajectory::new(run_id);
                trajectory.push(TrajectoryEvent::SessionStarted {
                    task: request.task.clone(),
                });
                trajectory.push(TrajectoryEvent::SessionFinished {
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

        let participant_ids = self.participants.iter().map(|p| p.id()).collect::<Vec<_>>();
        let mut state = RunState::new(run_id, &request.task, initial_observation, &participant_ids);

        let finish_reason = loop {
            if let Some(reason) = self.enforce_run_limits(&request, &started_at, &counters) {
                break reason;
            }

            if let Some(reason) = self.poll_environment_phase(&mut state).await? {
                break reason;
            }

            let finished_participants = state.finished_participants();
            if let Some(reason) = termination_condition_met(
                &request.termination_condition,
                &finished_participants,
                self.participants.len(),
                None,
            ) {
                break reason;
            }

            let Some(continuation) = self.next_work_item_continuation(&state) else {
                if state.pending_actions.is_empty() {
                    break FinishReason::Deadlocked;
                }
                continue;
            };

            let decision = {
                let input = state.continuation_input(run_id, &continuation.participant_id, &request);
                let participant = &mut self.participants[continuation.participant_index];
                participant.continue_work_item(input, output).await
            };

            match decision {
                Ok(decision) => {
                    if let Some(reason) = self.apply_participant_decision(
                        &request,
                        continuation,
                        decision,
                        &mut state,
                        &mut counters,
                        output,
                    )
                    .await?
                    {
                        break reason;
                    }
                }
                Err(error) => {
                    break FinishReason::ParticipantError {
                        participant: continuation.participant_id,
                        message: error.to_string(),
                    };
                }
            }
        };

        state
            .trajectory
            .push(TrajectoryEvent::SessionFinished {
                reason: finish_reason.clone(),
            });

        if let Some(evaluator) = &self.evaluator {
            let result = evaluator.evaluate(&request.task, &state.trajectory).await?;
            state
                .trajectory
                .push(TrajectoryEvent::EvaluationCompleted {
                    result: result.clone(),
                });
            evaluation = Some(result);
        }

        Ok(RunResult {
            run_id,
            finish_reason,
            trajectory: state.trajectory,
            evaluation,
        })
    }

    fn enforce_run_limits(
        &self,
        request: &RunRequest,
        started_at: &Instant,
        counters: &RunCounters,
    ) -> Option<FinishReason> {
        if counters.step_count >= request.limits.max_steps {
            return Some(FinishReason::StepLimitExceeded);
        }
        if let Some(max_duration) = request.limits.max_duration {
            if started_at.elapsed() >= max_duration {
                return Some(FinishReason::TimeLimitExceeded);
            }
        }
        None
    }

    async fn poll_environment_phase(
        &mut self,
        state: &mut RunState<E::Observation, E::Action, E::Outcome>,
    ) -> Result<Option<FinishReason>, EvaluatorError> {
        if let Some(reason) = self.environment.terminal_status().await.map_err(|error| {
            EvaluatorError::new(format!("failed to query terminal status: {error}"))
        })? {
            return Ok(Some(FinishReason::EnvironmentTerminated(reason)));
        }

        let events = self.environment.poll_events().await.map_err(|error| {
            EvaluatorError::new(format!("failed to poll environment events: {error}"))
        })?;
        if events.is_empty() {
            return Ok(None);
        }

        for event in events {
            state.apply_environment_event(event);
        }
        let observation = self.environment.observe().await.map_err(|error| {
            EvaluatorError::new(format!(
                "failed to refresh observation after environment events: {error}"
            ))
        })?;
        state.record_observation(observation);
        Ok(None)
    }

    fn next_work_item_continuation(
        &mut self,
        state: &RunState<E::Observation, E::Action, E::Outcome>,
    ) -> Option<WorkItemContinuation> {
        if self.participants.is_empty() {
            return None;
        }

        for offset in 0..self.participants.len() {
            let index = (self.next_participant_index + offset) % self.participants.len();
            let participant_id = self.participants[index].id();
            let runtime_state = state
                .participants
                .get(&participant_id)
                .expect("participant runtime state must exist");
            if runtime_state.finished || runtime_state.blocked_on_action.is_some() {
                continue;
            }

            self.next_participant_index = (index + 1) % self.participants.len();
            return Some(WorkItemContinuation {
                participant_id,
                participant_index: index,
                work_item: runtime_state.current_work_item.clone(),
            });
        }

        None
    }

    async fn apply_participant_decision(
        &mut self,
        request: &RunRequest,
        continuation: WorkItemContinuation,
        decision: ParticipantDecision<E::Action>,
        state: &mut RunState<E::Observation, E::Action, E::Outcome>,
        counters: &mut RunCounters,
        output: &mut dyn ParticipantOutput<E::Action>,
    ) -> Result<Option<FinishReason>, EvaluatorError> {
        let participant_id = continuation.participant_id;
        match decision {
            ParticipantDecision::Message { content } => {
                counters.step_count += 1;
                counters.message_count += 1;

                let work_item = state.work_item_for_message(&participant_id);
                state.trajectory.push(TrajectoryEvent::ParticipantMessage {
                    work_item: work_item.clone(),
                    participant: participant_id.clone(),
                    content: content.clone(),
                });
                state.route_participant_message(&participant_id, &work_item, &content);

                if counters.message_count > request.limits.max_messages {
                    return Ok(Some(FinishReason::MessageLimitExceeded));
                }
            }
            ParticipantDecision::FinalMessage { content } => {
                counters.step_count += 1;
                counters.message_count += 1;

                let work_item = state.work_item_for_message(&participant_id);
                let completed_work_item = WorkItem {
                    status: WorkItemStatus::Completed,
                    ..work_item
                };
                state.trajectory.push(TrajectoryEvent::ParticipantMessage {
                    work_item: completed_work_item.clone(),
                    participant: participant_id.clone(),
                    content: content.clone(),
                });
                state.route_participant_message(&participant_id, &completed_work_item, &content);
                state.trajectory.push(TrajectoryEvent::WorkItemCompleted {
                    work_item: completed_work_item.clone(),
                });
                state.complete_work_item(completed_work_item.id);

                if counters.message_count > request.limits.max_messages {
                    return Ok(Some(FinishReason::MessageLimitExceeded));
                }
            }
            ParticipantDecision::Action { action, execution } => {
                counters.step_count += 1;
                counters.action_count += 1;

                let work_item = continuation.work_item.or_else(|| state.work_item_for_action(&participant_id));
                state.trajectory.push(TrajectoryEvent::ActionRequested {
                    work_item: work_item.clone(),
                    participant: participant_id.clone(),
                    action: action.clone(),
                    execution,
                });
                if counters.action_count > request.limits.max_actions {
                    return Ok(Some(FinishReason::ActionLimitExceeded));
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
                        if let Some(work_item) = &work_item {
                            state.action_work_items.insert(
                                action_id,
                                WorkItem {
                                    status: WorkItemStatus::WaitingOnEnvironment,
                                    ..work_item.clone()
                                },
                            );
                        }

                        match self.environment.step(participant_id.clone(), action).await {
                            Ok(outcome) => {
                                let work_item = state.action_work_items.remove(&action_id).map(|work_item| {
                                    WorkItem {
                                        status: WorkItemStatus::Active,
                                        ..work_item
                                    }
                                });
                                if let Some(work_item) = &work_item {
                                    state
                                        .participants
                                        .get_mut(&participant_id)
                                        .expect("participant runtime state must exist")
                                        .current_work_item = Some(work_item.clone());
                                }
                                state.trajectory.push(TrajectoryEvent::ActionCompleted {
                                    work_item,
                                    participant: participant_id,
                                    action_id,
                                    outcome,
                                });
                                let observation = self.environment.observe().await.map_err(|error| {
                                    EvaluatorError::new(format!(
                                        "failed to refresh observation after immediate action: {error}"
                                    ))
                                })?;
                                state.record_observation(observation);
                            }
                            Err(error) => {
                                state.trajectory.push(TrajectoryEvent::ActionFailed {
                                    work_item: state.action_work_items.remove(&action_id),
                                    participant: participant_id,
                                    action_id,
                                    error: error.to_string(),
                                });
                                return Ok(Some(FinishReason::EnvironmentError(error.to_string())));
                            }
                        }
                    }
                    ActionExecution::DeferredBlocking | ActionExecution::DeferredNonBlocking => {
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
                                if let Some(work_item) = &work_item {
                                    state.action_work_items.insert(
                                        action_id,
                                        WorkItem {
                                            status: WorkItemStatus::WaitingOnEnvironment,
                                            ..work_item.clone()
                                        },
                                    );
                                }
                                state.trajectory.push(TrajectoryEvent::ActionSubmitted {
                                    work_item: work_item.clone(),
                                    participant: participant_id.clone(),
                                    action_id,
                                    action: action.clone(),
                                    execution,
                                });
                                let participant_state = state
                                    .participants
                                    .get_mut(&participant_id)
                                    .expect("participant runtime state must exist");
                                participant_state.inbox.push(
                                    ParticipantInboxEvent::ActionAccepted {
                                        work_item,
                                        action_id,
                                        action,
                                    },
                                );
                                state.pending_actions.insert(action_id);
                                if blocking {
                                    participant_state.blocked_on_action = Some(action_id);
                                }
                            }
                            Err(error) => {
                                return Ok(Some(FinishReason::EnvironmentError(error.to_string())));
                            }
                        }
                    }
                }
            }
            ParticipantDecision::Yield => {
                counters.step_count += 1;
                state.trajectory.push(TrajectoryEvent::ParticipantYielded {
                    participant: participant_id,
                });
            }
            ParticipantDecision::Finish => {
                state.mark_participant_finished(&participant_id);
                state.trajectory.push(TrajectoryEvent::ParticipantFinished {
                    participant: participant_id.clone(),
                });
                let finished = state.finished_participants();
                if let Some(reason) = termination_condition_met(
                    &request.termination_condition,
                    &finished,
                    self.participants.len(),
                    Some(&participant_id),
                ) {
                    return Ok(Some(reason));
                }
            }
        }

        Ok(None)
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
