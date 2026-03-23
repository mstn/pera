use std::error::Error;
use std::fmt::{Display, Formatter};

use async_trait::async_trait;
use pera_core::{ActionId, ActionRequest, ActionResult, RunId, Value};
use tokio::sync::mpsc;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionProcessorError {
    message: String,
}

impl ActionProcessorError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl Display for ActionProcessorError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl Error for ActionProcessorError {}

#[async_trait]
pub trait ActionHandler: Send + Sync + 'static {
    async fn handle(&self, action: &ActionRequest) -> Result<Value, ActionProcessorError>;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct RejectingActionHandler;

impl RejectingActionHandler {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl ActionHandler for RejectingActionHandler {
    async fn handle(&self, action: &ActionRequest) -> Result<Value, ActionProcessorError> {
        Err(ActionProcessorError::new(format!(
            "no action processor is configured for '{}'",
            action.action_name.as_str()
        )))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActionExecutionUpdate {
    Claimed {
        run_id: RunId,
        action_id: ActionId,
        worker_id: String,
    },
    Completed(ActionResult),
    Failed {
        run_id: RunId,
        action_id: ActionId,
        message: String,
    },
}

#[async_trait]
pub trait ActionExecutor: Clone + Send + Sync + 'static {
    async fn execute(&self, action: ActionRequest) -> ActionExecutionUpdate;
}

#[derive(Debug)]
pub(crate) struct ActionWorker<A> {
    worker_id: String,
    action_executor: A,
    action_rx: mpsc::UnboundedReceiver<ActionRequest>,
    update_tx: mpsc::UnboundedSender<ActionExecutionUpdate>,
}

impl<A> ActionWorker<A>
where
    A: ActionExecutor,
{
    pub(crate) fn new(
        worker_id: impl Into<String>,
        action_executor: A,
        action_rx: mpsc::UnboundedReceiver<ActionRequest>,
        update_tx: mpsc::UnboundedSender<ActionExecutionUpdate>,
    ) -> Self {
        Self {
            worker_id: worker_id.into(),
            action_executor,
            action_rx,
            update_tx,
        }
    }

    pub(crate) async fn run(mut self) {
        while let Some(action) = self.action_rx.recv().await {
            let _ = self.update_tx.send(ActionExecutionUpdate::Claimed {
                run_id: action.run_id,
                action_id: action.id,
                worker_id: self.worker_id.clone(),
            });

            let update = self.action_executor.execute(action).await;
            let _ = self.update_tx.send(update);
        }
    }
}

#[derive(Debug, Clone)]
pub struct InProcessActionExecutor<H> {
    handler: H,
}

impl<H> InProcessActionExecutor<H> {
    pub fn new(handler: H) -> Self {
        Self { handler }
    }
}

#[async_trait]
impl<H> ActionExecutor for InProcessActionExecutor<H>
where
    H: ActionHandler + Clone,
{
    async fn execute(&self, action: ActionRequest) -> ActionExecutionUpdate {
        match self.handler.handle(&action).await {
            Ok(value) => ActionExecutionUpdate::Completed(ActionResult {
                action_id: action.id,
                value,
            }),
            Err(error) => ActionExecutionUpdate::Failed {
                run_id: action.run_id,
                action_id: action.id,
                message: error.to_string(),
            },
        }
    }
}
