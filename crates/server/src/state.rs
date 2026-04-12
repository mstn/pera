use std::collections::BTreeMap;
use std::sync::Arc;

use pera_ui::UiSpec;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::{RwLock, broadcast};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UiSessionSnapshot {
    pub session_id: String,
    pub spec: Option<UiSpec>,
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
    pub spec: Option<UiSpec>,
    #[serde(default = "default_session_state")]
    pub state: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UiServerEvent {
    pub event_type: String,
    pub payload: Value,
}

#[derive(Debug)]
struct UiSession {
    snapshot: UiSessionSnapshot,
    updates: broadcast::Sender<UiServerEvent>,
}

#[derive(Debug, Default)]
pub struct ServerState {
    sessions: RwLock<BTreeMap<String, Arc<RwLock<UiSession>>>>,
}

impl ServerState {
    pub async fn create_session(&self, request: CreateSessionRequest) -> UiSessionSnapshot {
        let session_id = Uuid::new_v4().to_string();
        let snapshot = UiSessionSnapshot {
            session_id: session_id.clone(),
            spec: request.spec,
            state: request.state,
            status: "ready".to_owned(),
        };
        let (updates, _) = broadcast::channel(64);
        let session = Arc::new(RwLock::new(UiSession {
            snapshot: snapshot.clone(),
            updates,
        }));

        self.sessions.write().await.insert(session_id, session);
        snapshot
    }

    pub async fn get_session(&self, session_id: &str) -> Option<UiSessionSnapshot> {
        let session = self.sessions.read().await.get(session_id).cloned()?;
        Some(session.read().await.snapshot.clone())
    }

    pub async fn post_event(
        &self,
        session_id: &str,
        event: UiEventRequest,
    ) -> Option<UiSessionSnapshot> {
        let session = self.sessions.read().await.get(session_id).cloned()?;
        let mut session = session.write().await;
        session.snapshot.status = "event_received".to_owned();
        let update = UiServerEvent {
            event_type: "ui_event_received".to_owned(),
            payload: json!({
                "session_id": session.snapshot.session_id,
                "event_type": event.event_type,
                "payload": event.payload,
            }),
        };
        let _ = session.updates.send(update);
        Some(session.snapshot.clone())
    }

    pub async fn subscribe(
        &self,
        session_id: &str,
    ) -> Option<(UiSessionSnapshot, broadcast::Receiver<UiServerEvent>)> {
        let session = self.sessions.read().await.get(session_id).cloned()?;
        let session = session.read().await;
        Some((session.snapshot.clone(), session.updates.subscribe()))
    }
}

fn default_session_state() -> Value {
    json!({})
}
