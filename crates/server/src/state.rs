mod session;
mod todo;

use std::collections::BTreeMap;
use std::sync::Arc;

use pera_ui::UiSpec;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::{Mutex, RwLock, broadcast};
use uuid::Uuid;

use self::session::UiSessionRunner;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UiSessionSnapshot {
    pub session_id: String,
    pub spec: UiSpec,
    pub state: Value,
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UiEventRequest {
    pub event_type: String,
    #[serde(default)]
    pub payload: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateSessionRequest {
    pub spec: UiSpec,
    #[serde(default = "default_session_state")]
    pub state: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UiServerEvent {
    pub event_type: String,
    pub payload: Value,
}

struct UiSession {
    runner: Mutex<UiSessionRunner>,
    updates: broadcast::Sender<UiServerEvent>,
}

#[derive(Default)]
pub struct ServerState {
    sessions: RwLock<BTreeMap<String, Arc<UiSession>>>,
}

impl ServerState {
    pub async fn create_session(
        &self,
        request: CreateSessionRequest,
    ) -> Result<UiSessionSnapshot, String> {
        let session_id = Uuid::new_v4().to_string();
        let (updates, _) = broadcast::channel(64);
        let runner = UiSessionRunner::new(session_id.clone(), request.spec, request.state).await?;
        let snapshot = runner.snapshot();
        let session = Arc::new(UiSession {
            runner: Mutex::new(runner),
            updates,
        });

        self.sessions.write().await.insert(session_id, session);
        Ok(snapshot)
    }

    pub async fn get_session(&self, session_id: &str) -> Option<UiSessionSnapshot> {
        let session = self.sessions.read().await.get(session_id).cloned()?;
        let runner = session.runner.lock().await;
        Some(runner.snapshot())
    }

    pub async fn post_event(
        &self,
        session_id: &str,
        event: UiEventRequest,
    ) -> Result<Option<UiSessionSnapshot>, String> {
        let session = self.sessions.read().await.get(session_id).cloned();
        let Some(session) = session else {
            return Ok(None);
        };

        let mut runner = session.runner.lock().await;
        let snapshot = runner.handle_event(event).await?;
        let _ = session.updates.send(UiServerEvent {
            event_type: "snapshot".to_owned(),
            payload: serde_json::to_value(&snapshot)
                .map_err(|error| format!("failed to serialize session snapshot: {error}"))?,
        });
        Ok(Some(snapshot))
    }

    pub async fn subscribe(
        &self,
        session_id: &str,
    ) -> Option<(UiSessionSnapshot, broadcast::Receiver<UiServerEvent>)> {
        let session = self.sessions.read().await.get(session_id).cloned()?;
        let runner = session.runner.lock().await;
        Some((runner.snapshot(), session.updates.subscribe()))
    }
}

fn default_session_state() -> Value {
    json!({})
}
