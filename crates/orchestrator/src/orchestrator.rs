use std::collections::{BTreeMap, BTreeSet};
use std::time::Instant;

use pera_core::{ActionId, RunId, WorkItemId};
use tracing::{debug, warn};

use crate::error::EvaluatorError;
use crate::streaming::{NoopParticipantOutput, ParticipantOutput};
use crate::traits::{Environment, Evaluator, NoopEvaluator, Participant};
use crate::types::{
    ActionExecution, ActionRunStatus, EnvironmentEvent, FinishReason, InitialInboxMessage,
    ParticipantDecision, ParticipantId, ParticipantInboxEvent, ParticipantInput, RunRequest,
    RunResult, ScheduledAction, TerminationCondition, Trajectory, TrajectoryEvent, WorkItem,
};

type BoxedParticipant<O, A, U> = Box<dyn Participant<Observation = O, Action = A, Outcome = U>>;

#[derive(Debug, Default)]
struct RunCounters {
    step_count: usize,
    action_count: usize,
    message_count: usize,
    failed_action_count: usize,
    consecutive_failed_action_count: usize,
}

impl RunCounters {
    fn record_action_completed(&mut self) {
        self.consecutive_failed_action_count = 0;
    }

    fn record_action_failed(&mut self) {
        self.failed_action_count += 1;
        self.consecutive_failed_action_count += 1;
    }
}

#[derive(Debug, Clone)]
struct InitialMessage {
    from: ParticipantId,
    content: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LoopExecutionState {
    ReadyForTurn,
    WaitingForActionCompletion { action_id: ActionId },
}

#[derive(Debug, Clone)]
struct AgentLoopInput<O, A, U> {
    run_id: RunId,
    task: crate::types::TaskSpec,
    limits: crate::types::RunLimits,
    observation: O,
    trajectory: Trajectory<O, A, U>,
}

struct ActiveLoop<O, A, U> {
    participant: BoxedParticipant<O, A, U>,
    inbox: Vec<ParticipantInboxEvent<A, U>>,
    work_item: WorkItem,
    execution_state: LoopExecutionState,
    step_count: usize,
}

impl<O, A, U> ActiveLoop<O, A, U>
where
    O: Clone + Send + Sync + 'static,
    A: Clone + Send + Sync + 'static,
    U: Clone + Send + Sync + 'static,
{
    fn start(
        participant: BoxedParticipant<O, A, U>,
        inbox: Vec<ParticipantInboxEvent<A, U>>,
        initial_message: InitialMessage,
    ) -> Self {
        Self {
            participant,
            inbox,
            work_item: WorkItem {
                id: WorkItemId::generate(),
                from: initial_message.from,
                content: initial_message.content,
            },
            execution_state: LoopExecutionState::ReadyForTurn,
            step_count: 0,
        }
    }

    fn participant_id(&self) -> ParticipantId {
        self.participant.id()
    }

    fn is_ready_for_turn(&self) -> bool {
        matches!(self.execution_state, LoopExecutionState::ReadyForTurn)
    }

    fn awaiting_action_completion(&self) -> Option<ActionId> {
        match self.execution_state {
            LoopExecutionState::WaitingForActionCompletion { action_id } => Some(action_id),
            LoopExecutionState::ReadyForTurn => None,
        }
    }

    fn transition_to_ready_for_turn(&mut self) {
        self.execution_state = LoopExecutionState::ReadyForTurn;
    }

    fn transition_to_waiting_for_action_completion(&mut self, action_id: ActionId) {
        self.execution_state = LoopExecutionState::WaitingForActionCompletion { action_id };
    }

    fn handle_inbox_event(&mut self, event: &ParticipantInboxEvent<A, U>) {
        let should_resume = matches!(
            (self.awaiting_action_completion(), event),
            (
                Some(awaited_action_id),
                ParticipantInboxEvent::ActionCompleted { action_id, .. }
                | ParticipantInboxEvent::ActionFailed { action_id, .. },
            ) if *action_id == awaited_action_id
        );
        if should_resume {
            self.transition_to_ready_for_turn();
        }
    }

    fn deliver(&mut self, event: ParticipantInboxEvent<A, U>) {
        self.handle_inbox_event(&event);
        self.inbox.push(event);
    }

    fn transition_to_blocked_on_action(&mut self, action_id: ActionId) {
        self.transition_to_waiting_for_action_completion(action_id);
    }

    fn step_count(&self) -> usize {
        self.step_count
    }

    fn record_step(&mut self) {
        self.step_count += 1;
    }

    fn release(self) -> (BoxedParticipant<O, A, U>, Vec<ParticipantInboxEvent<A, U>>) {
        (self.participant, self.inbox)
    }

    async fn continue_with(
        &mut self,
        input: AgentLoopInput<O, A, U>,
        output: &mut dyn ParticipantOutput<A, U>,
    ) -> Result<ParticipantDecision<A>, crate::error::ParticipantError> {
        let _ = (
            &self.work_item.from,
            &self.work_item.content,
        );
        let input = ParticipantInput {
            run_id: input.run_id,
            agent_loop_id: self.work_item.id,
            agent_loop_iteration: self.step_count + 1,
            participant: self.participant.id(),
            work_item: Some(self.work_item.clone()),
            task: input.task,
            limits: input.limits,
            observation: input.observation,
            inbox: std::mem::take(&mut self.inbox),
            trajectory: input.trajectory,
        };
        self.participant.respond(input, output).await
    }
}

struct ParticipantRuntimeState<O, A, U> {
    participant: Option<BoxedParticipant<O, A, U>>,
    pending_inbox: Vec<ParticipantInboxEvent<A, U>>,
    active_loop: Option<ActiveLoop<O, A, U>>,
    finished: bool,
}

impl<O, A, U> ParticipantRuntimeState<O, A, U>
where
    O: Clone + Send + Sync + 'static,
    A: Clone + Send + Sync + 'static,
    U: Clone + Send + Sync + 'static,
{
    fn new(participant: BoxedParticipant<O, A, U>) -> Self {
        Self {
            participant: Some(participant),
            pending_inbox: Vec::new(),
            active_loop: None,
            finished: false,
        }
    }

    fn id(&self) -> ParticipantId {
        if let Some(active_loop) = &self.active_loop {
            active_loop.participant_id()
        } else {
            self.participant
                .as_ref()
                .expect("idle participant must be present")
                .id()
        }
    }

    fn has_startable_message(&self) -> bool {
        startable_message(&self.pending_inbox).is_some()
    }

    fn deliver(&mut self, event: ParticipantInboxEvent<A, U>) {
        if let Some(active_loop) = &mut self.active_loop {
            active_loop.deliver(event);
        } else {
            self.pending_inbox.push(event);
        }
    }

    fn is_runnable(&self) -> bool {
        if self.finished {
            return false;
        }
        match &self.active_loop {
            Some(active_loop) => active_loop.is_ready_for_turn(),
            None => self.has_startable_message(),
        }
    }

    fn start_next_loop(&mut self) -> Option<&mut ActiveLoop<O, A, U>> {
        let initial_message = startable_message(&self.pending_inbox)?;
        let participant = self
            .participant
            .take()
            .expect("idle participant must be available when starting loop");
        let inbox = std::mem::take(&mut self.pending_inbox);
        self.active_loop = Some(ActiveLoop::start(participant, inbox, initial_message));
        self.active_loop.as_mut()
    }

    fn active_loop_or_start(&mut self) -> Option<&mut ActiveLoop<O, A, U>> {
        if self.active_loop.is_none() {
            self.start_next_loop()?;
        }
        self.active_loop.as_mut()
    }

    fn record_current_loop_step(&mut self) {
        if let Some(active_loop) = self.active_loop.as_mut() {
            active_loop.record_step();
        }
    }

    fn apply_turn_progress(&mut self) {
        self.record_current_loop_step();
    }

    fn transition_current_loop_to_blocked_on_action(&mut self, action_id: ActionId) {
        self.active_loop
            .as_mut()
            .expect("active loop must exist before blocking")
            .transition_to_blocked_on_action(action_id);
    }

    fn complete_current_loop(&mut self) {
        self.transition_to_idle_after_loop_completion();
    }

    fn apply_loop_completion(&mut self) {
        self.apply_turn_progress();
        self.complete_current_loop();
    }

    fn transition_to_idle_after_loop_completion(&mut self) {
        if let Some(active_loop) = self.active_loop.take() {
            let (participant, inbox) = active_loop.release();
            self.pending_inbox.extend(inbox);
            self.participant = Some(participant);
        }
    }

    fn finish(&mut self) {
        self.finished = true;
        self.pending_inbox.clear();
        self.transition_to_idle_after_loop_completion();
    }

    fn into_participant(self) -> BoxedParticipant<O, A, U> {
        match (self.participant, self.active_loop) {
            (Some(participant), None) => participant,
            (None, Some(active_loop)) => active_loop.release().0,
            (Some(_), Some(_)) => unreachable!("participant and agent loop cannot coexist"),
            (None, None) => unreachable!("participant state must retain a participant"),
        }
    }
}

struct RunState<O, A, U> {
    observation: O,
    trajectory: Trajectory<O, A, U>,
    pending_actions: BTreeSet<ActionId>,
    submitted_actions: BTreeMap<ActionId, (ParticipantId, A)>,
    queued_environment_events: Vec<EnvironmentEvent<A, U>>,
    participants: Vec<ParticipantRuntimeState<O, A, U>>,
}

impl<O, A, U> RunState<O, A, U>
where
    O: Clone + Send + Sync + 'static,
    A: Clone + Send + Sync + 'static,
    U: Clone + Send + Sync + 'static,
{
    fn new(
        run_id: RunId,
        task: &crate::types::TaskSpec,
        observation: O,
        participants: Vec<BoxedParticipant<O, A, U>>,
        initial_messages: &[InitialInboxMessage],
    ) -> Self {
        let mut trajectory = Trajectory::new(run_id);
        trajectory.push(TrajectoryEvent::SessionStarted { task: task.clone() });
        trajectory.push(TrajectoryEvent::ObservationRecorded {
            observation: observation.clone(),
        });

        let mut state = Self {
            observation,
            trajectory,
            pending_actions: BTreeSet::new(),
            submitted_actions: BTreeMap::new(),
            queued_environment_events: Vec::new(),
            participants: participants
                .into_iter()
                .map(ParticipantRuntimeState::new)
                .collect(),
        };

        for message in initial_messages {
            if let Some(participant) = state.participant_mut(&message.to) {
                participant.deliver(ParticipantInboxEvent::Message {
                    from: message.from.clone(),
                    content: message.content.clone(),
                });
            }
        }

        state
    }

    fn into_parts(self) -> (Vec<BoxedParticipant<O, A, U>>, Trajectory<O, A, U>) {
        let participants = self
            .participants
            .into_iter()
            .map(ParticipantRuntimeState::into_participant)
            .collect();
        (participants, self.trajectory)
    }

    fn finished_participants(&self) -> BTreeSet<ParticipantId> {
        self.participants
            .iter()
            .filter(|participant| participant.finished)
            .map(ParticipantRuntimeState::id)
            .collect()
    }

    fn record_observation(&mut self, observation: O) {
        self.observation = observation.clone();
        self.trajectory
            .push(TrajectoryEvent::ObservationRecorded { observation });
    }

    fn participant_mut(
        &mut self,
        participant: &ParticipantId,
    ) -> Option<&mut ParticipantRuntimeState<O, A, U>> {
        self.participants
            .iter_mut()
            .find(|state| state.id() == *participant)
    }

    fn route_participant_message(&mut self, from: &ParticipantId, content: &str) {
        for participant in &mut self.participants {
            if participant.finished || participant.id() == *from {
                continue;
            }
            participant.deliver(ParticipantInboxEvent::Message {
                from: from.clone(),
                content: content.to_owned(),
            });
        }
    }

    fn emit_participant_message(&mut self, participant: &ParticipantId, content: &str) {
        self.trajectory.push(TrajectoryEvent::ParticipantMessage {
            participant: participant.clone(),
            content: content.to_owned(),
        });
        self.route_participant_message(participant, content);
    }

    fn finish_participant(&mut self, participant: &ParticipantId) {
        if let Some(participant) = self.participant_mut(participant) {
            participant.finish();
        }
    }

    fn take_submitted_action(&mut self, action_id: &ActionId) -> Option<(ParticipantId, A)> {
        self.submitted_actions.remove(action_id)
    }

    fn queue_environment_event(&mut self, event: EnvironmentEvent<A, U>) {
        self.queued_environment_events.push(event);
    }

    fn drain_environment_events(&mut self) -> Vec<EnvironmentEvent<A, U>> {
        std::mem::take(&mut self.queued_environment_events)
    }

    fn apply_environment_event(&mut self, event: EnvironmentEvent<A, U>) {
        match event {
            EnvironmentEvent::ActionScheduled {
                participant,
                action_id,
                action,
            } => {
                if let Some(participant) = self.participant_mut(&participant) {
                    participant
                        .deliver(ParticipantInboxEvent::ActionScheduled { action_id, action });
                }
            }
            EnvironmentEvent::ActionRunStatus {
                participant,
                action_id,
                run_id,
                status,
            } => {
                self.trajectory.push(TrajectoryEvent::ActionRunStatus {
                    participant,
                    action_id,
                    run_id,
                    status,
                });
            }
            EnvironmentEvent::ActionCompleted {
                participant,
                action_id,
                outcome,
            } => {
                self.pending_actions.remove(&action_id);
                self.submitted_actions.remove(&action_id);
                self.trajectory.push(TrajectoryEvent::ActionCompleted {
                    participant: participant.clone(),
                    action_id,
                    outcome: outcome.clone(),
                });
                if let Some(participant) = self.participant_mut(&participant) {
                    participant
                        .deliver(ParticipantInboxEvent::ActionCompleted { action_id, outcome });
                }
            }
            EnvironmentEvent::ActionFailed {
                participant,
                action_id,
                error,
            } => {
                self.pending_actions.remove(&action_id);
                self.submitted_actions.remove(&action_id);
                self.trajectory.push(TrajectoryEvent::ActionFailed {
                    participant: participant.clone(),
                    action_id,
                    error: error.clone(),
                });
                if let Some(participant) = self.participant_mut(&participant) {
                    participant.deliver(ParticipantInboxEvent::ActionFailed { action_id, error });
                }
            }
            EnvironmentEvent::Notification {
                participant,
                message,
            } => {
                if let Some(participant) = self.participant_mut(&participant) {
                    participant.deliver(ParticipantInboxEvent::Notification { message });
                }
            }
        }
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
        output: &mut dyn ParticipantOutput<E::Action, E::Outcome>,
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

        let participants = std::mem::take(&mut self.participants);
        let mut state = RunState::new(
            run_id,
            &request.task,
            initial_observation,
            participants,
            &request.initial_messages,
        );

        let finish_reason = loop {
            if let Some(reason) = self.enforce_run_limits(&request, &started_at, &counters) {
                break reason;
            }

            if let Some(reason) = self
                .poll_environment_phase(&mut state, &mut counters, &request, &started_at, output)
                .await?
            {
                break reason;
            }

            let finished_participants = state.finished_participants();
            if let Some(reason) = termination_condition_met(
                &request.termination_condition,
                &finished_participants,
                state.participants.len(),
                None,
            ) {
                break reason;
            }

            let Some(participant_index) = self.pick_next_participant(&state) else {
                if state.pending_actions.is_empty() {
                    break FinishReason::Deadlocked;
                }
                continue;
            };

            let participant = &mut state.participants[participant_index];
            let participant_id = participant.id();
            let Some(active_loop) = participant.active_loop_or_start() else {
                continue;
            };
            if active_loop.step_count() >= request.limits.max_steps_per_agent_loop {
                break FinishReason::AgentLoopStepLimitExceeded {
                    participant: participant_id,
                };
            }

            let decision = active_loop
                .continue_with(
                    AgentLoopInput {
                        run_id,
                        task: request.task.clone(),
                        limits: request.limits,
                        observation: state.observation.clone(),
                        trajectory: state.trajectory.clone(),
                    },
                    output,
                )
                .await;

            match decision {
                Ok(decision) => {
                    if let Some(reason) = self
                        .apply_decision(
                            &request,
                            participant_id,
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
                        participant: participant_id,
                        message: error.to_string(),
                    };
                }
            }
        };

        state.trajectory.push(TrajectoryEvent::SessionFinished {
            reason: finish_reason.clone(),
        });

        if let Some(evaluator) = &self.evaluator {
            let result = evaluator.evaluate(&request.task, &state.trajectory).await?;
            state.trajectory.push(TrajectoryEvent::EvaluationCompleted {
                result: result.clone(),
            });
            evaluation = Some(result);
        }

        let (participants, trajectory) = state.into_parts();
        self.participants = participants;

        Ok(RunResult {
            run_id,
            finish_reason,
            trajectory,
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
        if let Some(max_failed_actions) = request.limits.max_failed_actions
            && counters.failed_action_count >= max_failed_actions
        {
            return Some(FinishReason::FailedActionLimitExceeded {
                total_failures: counters.failed_action_count,
                consecutive_failures: counters.consecutive_failed_action_count,
            });
        }
        if let Some(max_consecutive_failed_actions) = request.limits.max_consecutive_failed_actions
            && counters.consecutive_failed_action_count >= max_consecutive_failed_actions
        {
            return Some(FinishReason::FailedActionLimitExceeded {
                total_failures: counters.failed_action_count,
                consecutive_failures: counters.consecutive_failed_action_count,
            });
        }
        if let Some(max_duration) = request.limits.max_duration
            && started_at.elapsed() >= max_duration
        {
            return Some(FinishReason::TimeLimitExceeded);
        }
        None
    }

    async fn poll_environment_phase(
        &mut self,
        state: &mut RunState<E::Observation, E::Action, E::Outcome>,
        counters: &mut RunCounters,
        request: &RunRequest,
        started_at: &Instant,
        output: &mut dyn ParticipantOutput<E::Action, E::Outcome>,
    ) -> Result<Option<FinishReason>, EvaluatorError> {
        if let Some(reason) = self.environment.terminal_status().await.map_err(|error| {
            EvaluatorError::new(format!("failed to query terminal status: {error}"))
        })? {
            return Ok(Some(FinishReason::EnvironmentTerminated(reason)));
        }

        let mut events = state.drain_environment_events();
        events.extend(self.environment.poll_events().await.map_err(|error| {
            EvaluatorError::new(format!("failed to poll environment events: {error}"))
        })?);
        if events.is_empty() {
            return Ok(None);
        }

        for event in events {
            match &event {
                EnvironmentEvent::ActionCompleted {
                    participant,
                    action_id,
                    outcome,
                } => {
                    counters.record_action_completed();
                    if let Some((_, action)) = state.take_submitted_action(action_id) {
                        output
                            .action_completed(participant, &action, outcome)
                            .await
                            .map_err(|error| {
                                EvaluatorError::new(format!(
                                    "failed to emit deferred action completion output: {error}"
                                ))
                            })?;
                    }
                }
                EnvironmentEvent::ActionRunStatus {
                    participant,
                    action_id,
                    status,
                    ..
                } => {
                    if let Some((_, action)) = state.submitted_actions.get(action_id) {
                        output
                            .status_update(participant, &format_action_run_status(action, status))
                            .await
                            .map_err(|error| {
                                EvaluatorError::new(format!(
                                    "failed to emit deferred action status output: {error}"
                                ))
                            })?;
                    }
                }
                EnvironmentEvent::ActionFailed {
                    participant,
                    action_id,
                    error,
                } => {
                    counters.record_action_failed();
                    if let Some((_, action)) = state.take_submitted_action(action_id) {
                        output
                            .action_failed(participant, &action, error)
                            .await
                            .map_err(|emit_error| {
                                EvaluatorError::new(format!(
                                    "failed to emit deferred action failure output: {emit_error}"
                                ))
                            })?;
                    }
                }
                EnvironmentEvent::ActionScheduled { .. }
                | EnvironmentEvent::Notification { .. } => {}
            }
            state.apply_environment_event(event);
            if let Some(reason) = self.enforce_run_limits(request, started_at, counters) {
                return Ok(Some(reason));
            }
        }
        let observation = self.environment.observe().await.map_err(|error| {
            EvaluatorError::new(format!(
                "failed to refresh observation after environment events: {error}"
            ))
        })?;
        state.record_observation(observation);
        Ok(None)
    }

    fn pick_next_participant(
        &mut self,
        state: &RunState<E::Observation, E::Action, E::Outcome>,
    ) -> Option<usize> {
        if state.participants.is_empty() {
            return None;
        }

        for offset in 0..state.participants.len() {
            let index = (self.next_participant_index + offset) % state.participants.len();
            if !state.participants[index].is_runnable() {
                continue;
            }

            self.next_participant_index = (index + 1) % state.participants.len();
            return Some(index);
        }

        None
    }

    async fn apply_decision(
        &mut self,
        request: &RunRequest,
        participant_id: ParticipantId,
        decision: ParticipantDecision<E::Action>,
        state: &mut RunState<E::Observation, E::Action, E::Outcome>,
        counters: &mut RunCounters,
        output: &mut dyn ParticipantOutput<E::Action, E::Outcome>,
    ) -> Result<Option<FinishReason>, EvaluatorError> {
        match decision {
            ParticipantDecision::Message { content } => {
                counters.step_count += 1;
                counters.message_count += 1;
                if let Some(participant) = state.participant_mut(&participant_id) {
                    participant.apply_turn_progress();
                }
                state.emit_participant_message(&participant_id, &content);
                if counters.message_count > request.limits.max_messages {
                    return Ok(Some(FinishReason::MessageLimitExceeded));
                }
            }
            ParticipantDecision::CompleteLoop { content } => {
                counters.step_count += 1;
                counters.message_count += 1;
                if let Some(participant) = state.participant_mut(&participant_id) {
                    participant.apply_loop_completion();
                }
                state.emit_participant_message(&participant_id, &content);
                state.trajectory.push(TrajectoryEvent::ParticipantLoopCompleted {
                    participant: participant_id.clone(),
                });
                if let Some(reason) = loop_termination_condition_met(
                    &request.termination_condition,
                    Some(&participant_id),
                ) {
                    return Ok(Some(reason));
                }
                if counters.message_count > request.limits.max_messages {
                    return Ok(Some(FinishReason::MessageLimitExceeded));
                }
            }
            ParticipantDecision::Action {
                message,
                action,
                execution,
            } => {
                counters.step_count += 1;
                counters.action_count += 1;
                if let Some(participant) = state.participant_mut(&participant_id) {
                    participant.apply_turn_progress();
                }
                if let Some(message) = message {
                    counters.message_count += 1;
                    state.emit_participant_message(&participant_id, &message);
                    if counters.message_count > request.limits.max_messages {
                        return Ok(Some(FinishReason::MessageLimitExceeded));
                    }
                }
                state.trajectory.push(TrajectoryEvent::ActionRequested {
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
                        match self
                            .environment
                            .perform_now(participant_id.clone(), action.clone())
                            .await
                        {
                            Ok(outcome) => {
                                state
                                    .submitted_actions
                                    .insert(action_id, (participant_id.clone(), action.clone()));
                                state.queue_environment_event(EnvironmentEvent::ActionCompleted {
                                    action_id,
                                    participant: participant_id.clone(),
                                    outcome,
                                });
                            }
                            Err(error) => {
                                return Ok(Some(FinishReason::EnvironmentError(error.to_string())));
                            }
                        }
                    }
                    ActionExecution::DeferredBlocking | ActionExecution::DeferredNonBlocking => {
                        let blocking = execution == ActionExecution::DeferredBlocking;
                        let deferred_action_id = ActionId::generate();
                        debug!(
                            participant = ?participant_id,
                            blocking,
                            "orchestrator scheduling deferred action"
                        );
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
                            .schedule(participant_id.clone(), action.clone())
                            .await
                        {
                            Ok(ScheduledAction { action_id }) => {
                                debug!(
                                    participant = ?participant_id,
                                    action_id = %action_id,
                                    blocking,
                                    "orchestrator scheduled deferred action"
                                );
                                state.trajectory.push(TrajectoryEvent::ActionScheduled {
                                    participant: participant_id.clone(),
                                    action_id,
                                    action: action.clone(),
                                    execution,
                                });
                                if let Some(participant) = state.participant_mut(&participant_id) {
                                    participant.deliver(ParticipantInboxEvent::ActionScheduled {
                                        action_id,
                                        action: action.clone(),
                                    });
                                    if blocking {
                                        participant
                                            .transition_current_loop_to_blocked_on_action(action_id);
                                    }
                                }
                                state.pending_actions.insert(action_id);
                                state
                                    .submitted_actions
                                    .insert(action_id, (participant_id.clone(), action.clone()));
                            }
                            Err(error) => {
                                warn!(
                                    participant = ?participant_id,
                                    blocking,
                                    error = %error.detail,
                                    "orchestrator failed to schedule deferred action"
                                );
                                state.submitted_actions.insert(
                                    deferred_action_id,
                                    (participant_id.clone(), action.clone()),
                                );
                                state.queue_environment_event(EnvironmentEvent::ActionFailed {
                                    participant: participant_id.clone(),
                                    action_id: deferred_action_id,
                                    error,
                                });
                            }
                        }
                    }
                }
            }
            ParticipantDecision::Yield => {
                counters.step_count += 1;
                if let Some(participant) = state.participant_mut(&participant_id) {
                    participant.apply_turn_progress();
                }
                state.trajectory.push(TrajectoryEvent::ParticipantYielded {
                    participant: participant_id,
                });
            }
            ParticipantDecision::Finish => {
                state.finish_participant(&participant_id);
                state.trajectory.push(TrajectoryEvent::ParticipantFinished {
                    participant: participant_id.clone(),
                });
                let finished_participants = state.finished_participants();
                if let Some(reason) = termination_condition_met(
                    &request.termination_condition,
                    &finished_participants,
                    state.participants.len(),
                    Some(&participant_id),
                ) {
                    return Ok(Some(reason));
                }
            }
        }

        Ok(None)
    }
}

fn format_action_run_status<A>(action: &A, status: &ActionRunStatus) -> String {
    let _ = action;
    match status {
        ActionRunStatus::RunSubmitted => "code execution submitted".to_owned(),
        ActionRunStatus::RunStarted => "running code".to_owned(),
        ActionRunStatus::ActionEnqueued {
            skill_name,
            action_name,
            ..
        } if !skill_name.is_empty() && !action_name.is_empty() => {
            format!("querying {skill_name}.{action_name}")
        }
        ActionRunStatus::ActionEnqueued { .. } => "waiting for skill action".to_owned(),
        ActionRunStatus::ActionClaimed {
            skill_name,
            action_name,
            worker_id,
            ..
        } if !skill_name.is_empty() && !action_name.is_empty() => {
            format!("running {skill_name}.{action_name} ({worker_id})")
        }
        ActionRunStatus::ActionClaimed { worker_id, .. } => {
            format!("running skill action ({worker_id})")
        }
        ActionRunStatus::ActionCompleted {
            skill_name,
            action_name,
            ..
        } if !skill_name.is_empty() && !action_name.is_empty() => {
            format!("completed {skill_name}.{action_name}")
        }
        ActionRunStatus::ActionCompleted { .. } => "skill action completed".to_owned(),
        ActionRunStatus::ActionFailed {
            skill_name,
            action_name,
            message,
            ..
        } if !skill_name.is_empty() && !action_name.is_empty() => {
            format!("failed {skill_name}.{action_name}: {message}")
        }
        ActionRunStatus::ActionFailed { message, .. } => {
            format!("skill action failed: {message}")
        }
        ActionRunStatus::RunResumed => "resuming code execution".to_owned(),
    }
}

fn startable_message<A, U>(inbox: &[ParticipantInboxEvent<A, U>]) -> Option<InitialMessage> {
    inbox.iter().find_map(|event| match event {
        ParticipantInboxEvent::Message { from, content } => Some(InitialMessage {
            from: from.clone(),
            content: content.clone(),
        }),
        _ => None,
    })
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
        TerminationCondition::AnyParticipantCompletedLoop
        | TerminationCondition::AnyOfParticipantsCompletedLoop(_) => None,
    }
}

fn loop_termination_condition_met(
    condition: &TerminationCondition,
    newly_completed_loop_participant: Option<&ParticipantId>,
) -> Option<FinishReason> {
    match condition {
        TerminationCondition::AnyParticipantCompletedLoop => newly_completed_loop_participant
            .cloned()
            .map(|participant| FinishReason::ParticipantCompletedLoop { participant }),
        TerminationCondition::AnyOfParticipantsCompletedLoop(participants) => {
            let newly_completed_loop_participant = newly_completed_loop_participant?;
            if participants.contains(newly_completed_loop_participant) {
                Some(FinishReason::ParticipantCompletedLoop {
                    participant: newly_completed_loop_participant.clone(),
                })
            } else {
                None
            }
        }
        TerminationCondition::AllParticipantsFinished
        | TerminationCondition::AnyParticipantFinished
        | TerminationCondition::AnyOfParticipantsFinished(_) => None,
    }
}
