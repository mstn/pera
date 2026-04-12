use std::convert::Infallible;
use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::routing::{get, post};
use axum::Router;
use futures_util::stream::{self, StreamExt};
use serde_json::json;
use tokio_stream::wrappers::BroadcastStream;

use crate::state::{
    CreateSessionRequest, ServerState, UiEventRequest, UiServerEvent, UiSessionSnapshot,
};

pub fn router(state: Arc<ServerState>) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/ui/sessions", post(create_session))
        .route("/ui/sessions/{session_id}", get(get_session))
        .route("/ui/sessions/{session_id}/events", post(post_event))
        .route("/ui/sessions/{session_id}/stream", get(stream_session))
        .with_state(state)
}

async fn health() -> impl IntoResponse {
    Json(json!({ "status": "ok" }))
}

async fn create_session(
    State(state): State<Arc<ServerState>>,
    Json(request): Json<CreateSessionRequest>,
) -> impl IntoResponse {
    let snapshot = state.create_session(request).await;
    (StatusCode::CREATED, Json(snapshot))
}

async fn get_session(
    State(state): State<Arc<ServerState>>,
    Path(session_id): Path<String>,
) -> impl IntoResponse {
    match state.get_session(&session_id).await {
        Some(snapshot) => (StatusCode::OK, Json(snapshot)).into_response(),
        None => not_found("ui session was not found"),
    }
}

async fn post_event(
    State(state): State<Arc<ServerState>>,
    Path(session_id): Path<String>,
    Json(event): Json<UiEventRequest>,
) -> impl IntoResponse {
    match state.post_event(&session_id, event).await {
        Some(snapshot) => (StatusCode::ACCEPTED, Json(snapshot)).into_response(),
        None => not_found("ui session was not found"),
    }
}

async fn stream_session(
    State(state): State<Arc<ServerState>>,
    Path(session_id): Path<String>,
) -> impl IntoResponse {
    let Some((snapshot, updates)) = state.subscribe(&session_id).await else {
        return not_found("ui session was not found");
    };

    let initial = stream::once(async move { Ok::<Event, Infallible>(snapshot_event(snapshot)) });
    let updates = BroadcastStream::new(updates).filter_map(|result| async move {
        match result {
            Ok(event) => Some(Ok::<Event, Infallible>(server_event(event))),
            Err(_) => None,
        }
    });
    let stream = initial.chain(updates);

    Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response()
}

fn snapshot_event(snapshot: UiSessionSnapshot) -> Event {
    Event::default()
        .event("snapshot")
        .json_data(snapshot)
        .expect("snapshot should serialize")
}

fn server_event(event: UiServerEvent) -> Event {
    Event::default()
        .event(event.event_type)
        .json_data(event.payload)
        .expect("server event should serialize")
}

fn not_found(message: &'static str) -> axum::response::Response {
    (
        StatusCode::NOT_FOUND,
        Json(json!({
            "error": message,
        })),
    )
        .into_response()
}
