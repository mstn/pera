use std::collections::{BTreeMap, BTreeSet};
use std::time::Instant;

use pera_core::{ActionId, RunId, WorkItemId};

use crate::error::EvaluatorError;
use crate::streaming::{NoopParticipantOutput, ParticipantOutput};
use crate::traits::{Environment, Evaluator, NoopEvaluator, Participant};
use crate::types::{
    ActionExecution, ActionRunStatus, EnvironmentEvent, FinishReason, InitialInboxMessage,
    ParticipantDecision, ParticipantId, ParticipantInboxEvent, ParticipantInput, RunRequest,
    RunResult, ScheduledAction, TerminationCondition, Trajectory, TrajectoryEvent,
};

type BoxedParticipant<O, A, U> = Box<dyn Participant<Observation = O, Action = A, Outcome = U>>;

#[derive(Debug, Default)]
struct RunCounters {
    step_count: usize,
    action_count: usize,
    message_count: usize,
}

#[derive(Debug, Clone)]
struct InitialMessage {
    from: ParticipantId,
    content: String,
}

#[derive(Debug, Clone)]
struct WorkItem {
    id: WorkItemId,
    initial_message: InitialMessage,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AgentLoopStatus {
    Ready,
    WaitingOnEnvironment { action_id: ActionId },
}

#[derive(Debug, Clone)]
struct AgentLoopInput<O, A, U> {
    run_id: RunId,
    task: crate::types::TaskSpec,
    limits: crate::types::RunLimits,
    observation: O,
    trajectory: Trajectory<O, A, U>,
}

struct AgentLoop<O, A, U> {
    participant: BoxedParticipant<O, A, U>,
    inbox: Vec<ParticipantInboxEvent<A, U>>,
    work_item: WorkItem,
    status: AgentLoopStatus,
    step_count: usize,
}

impl<O, A, U> AgentLoop<O, A, U>
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
                initial_message,
            },
            status: AgentLoopStatus::Ready,
            step_count: 0,
        }
    }

    fn participant_id(&self) -> ParticipantId {
        self.participant.id()
    }

    fn has_mail(&self) -> bool {
        !self.inbox.is_empty()
    }

    fn can_continue(&self) -> bool {
        matches!(self.status, AgentLoopStatus::Ready)
    }

    fn deliver(&mut self, event: ParticipantInboxEvent<A, U>) {
        self.inbox.push(event);
    }

    fn on_mailbox_updated(&mut self) {
        if self.has_mail() && matches!(self.status, AgentLoopStatus::WaitingOnEnvironment { .. }) {
            self.status = AgentLoopStatus::Ready;
        }
    }

    fn block_on_action(&mut self, action_id: ActionId) {
        self.status = AgentLoopStatus::WaitingOnEnvironment { action_id };
    }

    fn step_count(&self) -> usize {
        self.step_count
    }

    fn record_step(&mut self) {
        self.step_count += 1;
    }

    fn into_parts(self) -> (BoxedParticipant<O, A, U>, Vec<ParticipantInboxEvent<A, U>>) {
        (self.participant, self.inbox)
    }

    async fn continue_with(
        &mut self,
        input: AgentLoopInput<O, A, U>,
        output: &mut dyn ParticipantOutput<A, U>,
    ) -> Result<ParticipantDecision<A>, crate::error::ParticipantError> {
        self.on_mailbox_updated();
        let _ = (
            &self.work_item.initial_message.from,
            &self.work_item.initial_message.content,
        );
        let input = ParticipantInput {
            run_id: input.run_id,
            agent_loop_id: self.work_item.id,
            agent_loop_iteration: self.step_count + 1,
            participant: self.participant.id(),
            task: input.task,
            limits: input.limits,
            observation: input.observation,
            inbox: std::mem::take(&mut self.inbox),
            trajectory: input.trajectory,
        };
        self.participant.respond(input, output).await
    }
}

struct ParticipantState<O, A, U> {
    participant: Option<BoxedParticipant<O, A, U>>,
    pending_inbox: Vec<ParticipantInboxEvent<A, U>>,
    agent_loop: Option<AgentLoop<O, A, U>>,
    finished: bool,
}

impl<O, A, U> ParticipantState<O, A, U>
where
    O: Clone + Send + Sync + 'static,
    A: Clone + Send + Sync + 'static,
    U: Clone + Send + Sync + 'static,
{
    fn new(participant: BoxedParticipant<O, A, U>) -> Self {
        Self {
            participant: Some(participant),
            pending_inbox: Vec::new(),
            agent_loop: None,
            finished: false,
        }
    }

    fn id(&self) -> ParticipantId {
        if let Some(agent_loop) = &self.agent_loop {
            agent_loop.participant_id()
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
        if let Some(agent_loop) = &mut self.agent_loop {
            agent_loop.deliver(event);
        } else {
            self.pending_inbox.push(event);
        }
    }

    fn is_runnable(&self) -> bool {
        if self.finished {
            return false;
        }
        match &self.agent_loop {
            Some(agent_loop) => agent_loop.can_continue() || agent_loop.has_mail(),
            None => self.has_startable_message(),
        }
    }

    fn start_loop(&mut self) -> Option<&mut AgentLoop<O, A, U>> {
        let initial_message = startable_message(&self.pending_inbox)?;
        let participant = self
            .participant
            .take()
            .expect("idle participant must be available when starting loop");
        let inbox = std::mem::take(&mut self.pending_inbox);
        self.agent_loop = Some(AgentLoop::start(participant, inbox, initial_message));
        self.agent_loop.as_mut()
    }

    fn get_or_start_loop(&mut self) -> Option<&mut AgentLoop<O, A, U>> {
        if self.agent_loop.is_none() {
            self.start_loop()?;
        }
        self.agent_loop.as_mut()
    }

    fn complete_loop(&mut self) {
        if let Some(agent_loop) = self.agent_loop.take() {
            let (participant, inbox) = agent_loop.into_parts();
            self.pending_inbox.extend(inbox);
            self.participant = Some(participant);
        }
    }

    fn finish(&mut self) {
        self.finished = true;
        self.pending_inbox.clear();
        self.complete_loop();
    }

    fn into_participant(self) -> BoxedParticipant<O, A, U> {
        match (self.participant, self.agent_loop) {
            (Some(participant), None) => participant,
            (None, Some(agent_loop)) => agent_loop.into_parts().0,
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
    participants: Vec<ParticipantState<O, A, U>>,
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
                .map(ParticipantState::new)
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
            .map(ParticipantState::into_participant)
            .collect();
        (participants, self.trajectory)
    }

    fn finished_participants(&self) -> BTreeSet<ParticipantId> {
        self.participants
            .iter()
            .filter(|participant| participant.finished)
            .map(ParticipantState::id)
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
    ) -> Option<&mut ParticipantState<O, A, U>> {
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

            if let Some(reason) = self.poll_environment_phase(&mut state, output).await? {
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
            let Some(agent_loop) = participant.get_or_start_loop() else {
                continue;
            };
            if agent_loop.step_count() >= request.limits.max_steps_per_agent_loop {
                break FinishReason::AgentLoopStepLimitExceeded {
                    participant: participant_id,
                };
            }

            let decision = agent_loop
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
                            .status_update(
                                participant,
                                &format_action_run_status(action, status),
                            )
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
                EnvironmentEvent::ActionScheduled { .. } | EnvironmentEvent::Notification { .. } => {}
            }
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
                if let Some(participant) = state.participant_mut(&participant_id)
                    && let Some(agent_loop) = participant.agent_loop.as_mut()
                {
                    agent_loop.record_step();
                }
                state.trajectory.push(TrajectoryEvent::ParticipantMessage {
                    participant: participant_id.clone(),
                    content: content.clone(),
                });
                state.route_participant_message(&participant_id, &content);
                if counters.message_count > request.limits.max_messages {
                    return Ok(Some(FinishReason::MessageLimitExceeded));
                }
            }
            ParticipantDecision::FinalMessage { content } => {
                counters.step_count += 1;
                counters.message_count += 1;
                if let Some(participant) = state.participant_mut(&participant_id)
                    && let Some(agent_loop) = participant.agent_loop.as_mut()
                {
                    agent_loop.record_step();
                }
                state.trajectory.push(TrajectoryEvent::ParticipantMessage {
                    participant: participant_id.clone(),
                    content: content.clone(),
                });
                state.route_participant_message(&participant_id, &content);
                if let Some(participant) = state.participant_mut(&participant_id) {
                    participant.complete_loop();
                }
                if counters.message_count > request.limits.max_messages {
                    return Ok(Some(FinishReason::MessageLimitExceeded));
                }
            }
            ParticipantDecision::Action { action, execution } => {
                counters.step_count += 1;
                counters.action_count += 1;
                if let Some(participant) = state.participant_mut(&participant_id)
                    && let Some(agent_loop) = participant.agent_loop.as_mut()
                {
                    agent_loop.record_step();
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
                                state.submitted_actions.insert(
                                    action_id,
                                    (participant_id.clone(), action.clone()),
                                );
                                state.queue_environment_event(
                                    EnvironmentEvent::ActionCompleted {
                                        action_id,
                                        participant: participant_id.clone(),
                                        outcome,
                                    },
                                );
                            }
                            Err(error) => {
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
                            .schedule(participant_id.clone(), action.clone())
                            .await
                        {
                            Ok(ScheduledAction { action_id }) => {
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
                                            .agent_loop
                                            .as_mut()
                                            .expect("agent loop must exist before blocking")
                                            .block_on_action(action_id);
                                    }
                                }
                                state.pending_actions.insert(action_id);
                                state.submitted_actions.insert(
                                    action_id,
                                    (participant_id.clone(), action.clone()),
                                );
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
                if let Some(participant) = state.participant_mut(&participant_id)
                    && let Some(agent_loop) = participant.agent_loop.as_mut()
                {
                    agent_loop.record_step();
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
    }
}
