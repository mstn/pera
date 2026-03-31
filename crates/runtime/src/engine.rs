use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt::{Display, Formatter};
use std::sync::{Arc, RwLock};

use pera_core::{
    ActionRequest, CodeArtifactId, EventPublisher, ExecutionEvent, ExecutionSession,
    ExecutionStatus, RunId, RunStore, StartExecutionRequest, StoreError,
};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tracing::error;

use crate::{
    ActionExecutionUpdate, ActionExecutor, ActionWorker, EventHub, EventSubscription, RunExecutor,
    RunExecutorError, RunTransition, RunTransitionTrigger,
};

#[derive(Debug)]
pub enum ExecutionEngineError {
    RunExecutor(RunExecutorError),
    Store(StoreError),
    InvalidState(&'static str),
    UnknownRun(RunId),
    EngineClosed,
}

impl Display for ExecutionEngineError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RunExecutor(error) => write!(f, "run executor error: {error}"),
            Self::Store(error) => write!(f, "store error: {error}"),
            Self::InvalidState(message) => f.write_str(message),
            Self::UnknownRun(run_id) => write!(f, "unknown run {run_id}"),
            Self::EngineClosed => f.write_str("execution engine is closed"),
        }
    }
}

impl Error for ExecutionEngineError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::RunExecutor(error) => Some(error),
            Self::Store(error) => Some(error),
            Self::InvalidState(_) | Self::UnknownRun(_) | Self::EngineClosed => None,
        }
    }
}

impl From<RunExecutorError> for ExecutionEngineError {
    fn from(value: RunExecutorError) -> Self {
        Self::RunExecutor(value)
    }
}

impl From<StoreError> for ExecutionEngineError {
    fn from(value: StoreError) -> Self {
        Self::Store(value)
    }
}

#[derive(Debug)]
enum EngineCommand {
    Recover {
        events: Vec<ExecutionEvent>,
        reply: oneshot::Sender<Result<(), ExecutionEngineError>>,
    },
    Submit {
        request: StartExecutionRequest,
        reply: oneshot::Sender<Result<RunId, ExecutionEngineError>>,
    },
}

#[derive(Debug)]
struct EngineState {
    run_statuses: BTreeMap<RunId, ExecutionStatus>,
    active_runs: BTreeSet<RunId>,
}

impl EngineState {
    fn new() -> Self {
        Self {
            run_statuses: BTreeMap::new(),
            active_runs: BTreeSet::new(),
        }
    }

    fn record_status(&mut self, session: &ExecutionSession) {
        self.run_statuses.insert(session.id, session.status.clone());

        match session.status {
            ExecutionStatus::Completed(_) | ExecutionStatus::Failed(_) => {
                self.active_runs.remove(&session.id);
            }
            ExecutionStatus::Running | ExecutionStatus::WaitingForAction(_) => {
                self.active_runs.insert(session.id);
            }
        }
    }
}

#[derive(Debug)]
struct ExecutionWorker<I, S, P> {
    run_executor: RunExecutor<I>,
    store: S,
    publisher: P,
    action_tx: mpsc::UnboundedSender<ActionRequest>,
    state: Arc<RwLock<EngineState>>,
    command_rx: mpsc::UnboundedReceiver<EngineCommand>,
    update_tx: mpsc::UnboundedSender<ActionExecutionUpdate>,
    update_rx: mpsc::UnboundedReceiver<ActionExecutionUpdate>,
}

#[derive(Debug)]
pub struct ExecutionEngine<I, S, P, A> {
    command_tx: mpsc::UnboundedSender<EngineCommand>,
    event_hub: EventHub,
    state: Arc<RwLock<EngineState>>,
    _task: JoinHandle<()>,
    _action_task: JoinHandle<()>,
    _marker: std::marker::PhantomData<(I, S, P, A)>,
}

impl<I, S, P, A> ExecutionEngine<I, S, P, A>
where
    I: pera_core::Interpreter + Send + 'static,
    S: pera_core::RunStore + Send + 'static,
    P: pera_core::EventPublisher + Send + 'static,
    A: ActionExecutor,
{
    pub fn new(
        run_executor: RunExecutor<I>,
        store: S,
        publisher: P,
        action_executor: A,
        event_hub: EventHub,
    ) -> Self {
        let (command_tx, command_rx) = mpsc::unbounded_channel();
        let (action_tx, action_rx) = mpsc::unbounded_channel();
        let (update_tx, update_rx) = mpsc::unbounded_channel();
        let state = Arc::new(RwLock::new(EngineState::new()));
        let worker = ExecutionWorker {
            run_executor,
            store,
            publisher,
            action_tx,
            state: Arc::clone(&state),
            command_rx,
            update_tx,
            update_rx,
        };
        let update_tx = worker.update_tx.clone();
        let action_worker =
            ActionWorker::new("action-worker-1", action_executor, action_rx, update_tx);
        let action_task = tokio::spawn(action_worker.run());
        // Recovery/supervision is not implemented yet. If the background worker task
        // or the action worker task exits or panics, the public engine handle remains
        // but no longer has a fully live runtime behind it.
        let task = tokio::spawn(worker.run());

        Self {
            command_tx,
            event_hub,
            state,
            _task: task,
            _action_task: action_task,
            _marker: std::marker::PhantomData,
        }
    }

    pub fn subscribe(&self) -> EventSubscription {
        self.event_hub.subscribe()
    }

    pub async fn recover_from_events(
        &self,
        events: Vec<ExecutionEvent>,
    ) -> Result<(), ExecutionEngineError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.command_tx
            .send(EngineCommand::Recover {
                events,
                reply: reply_tx,
            })
            .map_err(|_| ExecutionEngineError::EngineClosed)?;

        reply_rx
            .await
            .map_err(|_| ExecutionEngineError::EngineClosed)?
    }

    pub async fn submit(
        &self,
        request: StartExecutionRequest,
    ) -> Result<RunId, ExecutionEngineError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.command_tx
            .send(EngineCommand::Submit {
                request,
                reply: reply_tx,
            })
            .map_err(|_| ExecutionEngineError::EngineClosed)?;

        reply_rx
            .await
            .map_err(|_| ExecutionEngineError::EngineClosed)?
    }

    pub fn run_status(&self, run_id: RunId) -> Option<ExecutionStatus> {
        self.state
            .read()
            .ok()
            .and_then(|state| state.run_statuses.get(&run_id).cloned())
    }

    pub fn has_active_runs(&self) -> bool {
        self.state
            .read()
            .map(|state| !state.active_runs.is_empty())
            .unwrap_or(false)
    }
}

impl<I, S, P> ExecutionWorker<I, S, P>
where
    I: pera_core::Interpreter + Send + 'static,
    S: RunStore + Send + 'static,
    P: EventPublisher + Send + 'static,
{
    async fn run(mut self) {
        loop {
            tokio::select! {
                Some(command) = self.command_rx.recv() => {
                    match command {
                        EngineCommand::Recover { events, reply } => {
                            let result = self.handle_recover(events).await;
                            let _ = reply.send(result);
                        }
                        EngineCommand::Submit { request, reply } => {
                            let result = self.handle_submit(request).await;
                            let _ = reply.send(result);
                        }
                    }
                }
                Some(update) = self.update_rx.recv() => {
                    if let Err(update_error) = self.handle_update(update.clone()).await {
                        error!(error = %update_error, "engine failed to process action update");
                        let _ = self.handle_update_error(update, update_error).await;
                    }
                }
                else => break,
            }
        }
    }

    async fn handle_submit(
        &mut self,
        mut request: StartExecutionRequest,
    ) -> Result<RunId, ExecutionEngineError> {
        let run_id = self.allocate_run_id();
        let code_id = self.allocate_code_id();
        request.code.id = code_id;
        let code_artifact = request.code.clone();
        let transition =
            self.run_executor
                .start_run(request, run_id, code_id, pera_core::ActionId::generate)?;
        self.store
            .save_code_artifact(run_id, &code_artifact)?;
        self.apply_transition(transition).await?;
        Ok(run_id)
    }

    async fn handle_recover(
        &mut self,
        events: Vec<ExecutionEvent>,
    ) -> Result<(), ExecutionEngineError> {
        let recovered = RecoveredRuntime::from_events(&events);

        for run_id in self.store.list_runs()? {
            let session = self.store.load_run(run_id)?;
            self.record_status(&session);

            if recovered.terminal_runs.contains(&run_id) {
                continue;
            }

            if let ExecutionStatus::WaitingForAction(action_id) = session.status {
                let action = self.store.load_action(action_id)?;
                self.action_tx
                    .send(action.request)
                    .map_err(|_| ExecutionEngineError::EngineClosed)?;
            }
        }

        Ok(())
    }

    async fn handle_update(
        &mut self,
        update: ActionExecutionUpdate,
    ) -> Result<(), ExecutionEngineError> {
        match update {
            ActionExecutionUpdate::Claimed {
                run_id,
                action_id,
                skill_name,
                action_name,
                worker_id,
            } => {
                self.publisher.publish(ExecutionEvent::ActionClaimed {
                    run_id,
                    action_id,
                    skill_name,
                    action_name,
                    worker_id,
                })?;
            }
            ActionExecutionUpdate::Completed(result) => {
                let action = self.store.load_action(result.action_id)?;
                let session = self.store.load_run(action.request.run_id)?;
                let transition = self.run_executor.complete_action(
                    session,
                    action.request,
                    result,
                    pera_core::ActionId::generate,
                )?;
                self.apply_transition(transition).await?;
            }
            ActionExecutionUpdate::Failed {
                run_id,
                action_id,
                skill_name,
                action_name,
                message,
            } => {
                self.publisher.publish(ExecutionEvent::ActionFailed {
                    run_id,
                    action_id,
                    skill_name,
                    action_name,
                    message: message.clone(),
                })?;
                let session = self.store.load_run(run_id)?;
                let transition = self.run_executor.fail_run(session, message);
                self.apply_transition(transition).await?;
            }
        }

        Ok(())
    }

    async fn handle_update_error(
        &mut self,
        update: ActionExecutionUpdate,
        error: ExecutionEngineError,
    ) -> Result<(), ExecutionEngineError> {
        let message = error.to_string();

        match update {
            ActionExecutionUpdate::Claimed {
                run_id,
                action_id,
                skill_name,
                action_name,
                ..
            } => {
                self.publisher.publish(ExecutionEvent::ActionFailed {
                    run_id,
                    action_id,
                    skill_name,
                    action_name,
                    message: message.clone(),
                })?;
                let session = self.store.load_run(run_id)?;
                let transition = self.run_executor.fail_run(session, message);
                self.apply_transition(transition).await?;
            }
            ActionExecutionUpdate::Completed(result) => {
                let action = self.store.load_action(result.action_id)?;
                let run_id = action.request.run_id;
                self.publisher.publish(ExecutionEvent::ActionFailed {
                    run_id,
                    action_id: result.action_id,
                    skill_name: action.request.skill.skill_name.clone(),
                    action_name: action.request.invocation.action_name.as_str().to_owned(),
                    message: message.clone(),
                })?;
                let session = self.store.load_run(run_id)?;
                let transition = self.run_executor.fail_run(session, message);
                self.apply_transition(transition).await?;
            }
            ActionExecutionUpdate::Failed {
                run_id,
                action_id,
                skill_name,
                action_name,
                message: action_message,
            } => {
                self.publisher.publish(ExecutionEvent::ActionFailed {
                    run_id,
                    action_id,
                    skill_name,
                    action_name,
                    message: action_message,
                })?;
                let session = self.store.load_run(run_id)?;
                let transition = self.run_executor.fail_run(session, message);
                self.apply_transition(transition).await?;
            }
        }

        Ok(())
    }

    fn record_status(&self, session: &ExecutionSession) {
        if let Ok(mut state) = self.state.write() {
            state.record_status(session);
        }
    }

    async fn apply_transition(
        &mut self,
        transition: RunTransition,
    ) -> Result<(), ExecutionEngineError> {
        let events = self.transition_events(&transition);
        self.store.save_run(transition.session.clone())?;

        for action_record in transition.action_records {
            self.store.save_action(action_record)?;
        }

        if let Some(action) = transition.action_to_enqueue {
            self.action_tx
                .send(action)
                .map_err(|_| ExecutionEngineError::EngineClosed)?;
        }

        for event in events {
            self.publisher.publish(event)?;
        }

        self.record_status(&transition.session);

        Ok(())
    }

    fn transition_events(&self, transition: &RunTransition) -> Vec<ExecutionEvent> {
        let mut events = Vec::new();
        let run_id = transition.session.id;

        match transition.trigger {
            RunTransitionTrigger::Started => {
                events.push(ExecutionEvent::RunSubmitted { run_id });
                events.push(ExecutionEvent::RunStarted { run_id });
            }
            RunTransitionTrigger::Resumed {
                completed_action_id,
            } => {
                if let Some(completed_action) = transition
                    .action_records
                    .iter()
                    .find(|record| record.request.id == completed_action_id)
                {
                    events.push(ExecutionEvent::ActionCompleted {
                        run_id,
                        action_id: completed_action_id,
                        skill_name: completed_action.request.skill.skill_name.clone(),
                        action_name: completed_action
                            .request
                            .invocation
                            .action_name
                            .as_str()
                            .to_owned(),
                    });
                }
                events.push(ExecutionEvent::RunResumed { run_id });
            }
            RunTransitionTrigger::Failed => {}
        }

        if let Some(action) = &transition.action_to_enqueue {
            events.push(ExecutionEvent::ActionEnqueued {
                run_id,
                action_id: action.id,
                skill_name: action.skill.skill_name.clone(),
                action_name: action.invocation.action_name.as_str().to_owned(),
            });
        }

        match &transition.session.status {
            ExecutionStatus::Completed(output) => events.push(ExecutionEvent::RunCompleted {
                run_id,
                value: output.value.clone(),
            }),
            ExecutionStatus::Failed(message) => events.push(ExecutionEvent::RunFailed {
                run_id,
                message: message.clone(),
            }),
            ExecutionStatus::Running | ExecutionStatus::WaitingForAction(_) => {}
        }

        events
    }

    fn allocate_run_id(&mut self) -> RunId {
        RunId::generate()
    }

    fn allocate_code_id(&mut self) -> CodeArtifactId {
        CodeArtifactId::generate()
    }
}

#[derive(Debug, Default)]
struct RecoveredRuntime {
    terminal_runs: BTreeSet<RunId>,
}

impl RecoveredRuntime {
    fn from_events(events: &[ExecutionEvent]) -> Self {
        let mut recovered = Self {
            terminal_runs: BTreeSet::new(),
        };

        for event in events {
            match event {
                ExecutionEvent::RunCompleted { run_id, .. }
                | ExecutionEvent::RunFailed { run_id, .. } => {
                    recovered.terminal_runs.insert(*run_id);
                }
                ExecutionEvent::ActionEnqueued { .. }
                | ExecutionEvent::ActionClaimed { .. }
                | ExecutionEvent::ActionCompleted { .. }
                | ExecutionEvent::ActionFailed { .. }
                | ExecutionEvent::RunSubmitted { .. }
                | ExecutionEvent::RunStarted { .. }
                | ExecutionEvent::RunResumed { .. } => {}
            }
        }

        recovered
    }
}
