use pera_core::{EventPublisher, ExecutionEvent, StoreError};
use tokio::sync::broadcast;

#[derive(Debug, Clone)]
pub struct TeeEventPublisher<A, B> {
    left: A,
    right: B,
}

impl<A, B> TeeEventPublisher<A, B> {
    pub fn new(left: A, right: B) -> Self {
        Self { left, right }
    }
}

impl<A, B> EventPublisher for TeeEventPublisher<A, B>
where
    A: EventPublisher,
    B: EventPublisher,
{
    fn publish(&mut self, event: ExecutionEvent) -> Result<(), StoreError> {
        self.left.publish(event.clone())?;
        self.right.publish(event)
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct StdoutEventPublisher;

impl StdoutEventPublisher {
    pub fn new() -> Self {
        Self
    }
}

impl EventPublisher for StdoutEventPublisher {
    fn publish(&mut self, event: ExecutionEvent) -> Result<(), StoreError> {
        let line = serde_json::to_string(&event)
            .map_err(|error| StoreError::new(format!("failed to serialize event: {error}")))?;
        println!("{line}");
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct EventHub {
    sender: broadcast::Sender<ExecutionEvent>,
}

impl Default for EventHub {
    fn default() -> Self {
        Self::new()
    }
}

impl EventHub {
    pub fn new() -> Self {
        let (sender, _) = broadcast::channel(1024);
        Self { sender }
    }

    pub fn subscribe(&self) -> EventSubscription {
        EventSubscription {
            receiver: self.sender.subscribe(),
        }
    }

    pub fn publisher(&self) -> EventHubPublisher {
        EventHubPublisher {
            sender: self.sender.clone(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct EventHubPublisher {
    sender: broadcast::Sender<ExecutionEvent>,
}

impl EventPublisher for EventHubPublisher {
    fn publish(&mut self, event: ExecutionEvent) -> Result<(), StoreError> {
        match self.sender.send(event) {
            Ok(_) | Err(broadcast::error::SendError(_)) => Ok(()),
        }
    }
}

#[derive(Debug)]
pub struct EventSubscription {
    receiver: broadcast::Receiver<ExecutionEvent>,
}

impl EventSubscription {
    pub async fn recv(&mut self) -> Result<ExecutionEvent, StoreError> {
        self.receiver.recv().await.map_err(broadcast_error)
    }

    pub fn try_recv(&mut self) -> Result<Option<ExecutionEvent>, StoreError> {
        match self.receiver.try_recv() {
            Ok(event) => Ok(Some(event)),
            Err(broadcast::error::TryRecvError::Empty) => Ok(None),
            Err(error) => Err(broadcast_try_error(error)),
        }
    }
}

fn broadcast_error(error: broadcast::error::RecvError) -> StoreError {
    StoreError::new(format!("failed to receive event: {error}"))
}

fn broadcast_try_error(error: broadcast::error::TryRecvError) -> StoreError {
    StoreError::new(format!("failed to receive event: {error}"))
}
