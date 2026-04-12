mod api;
mod error;
mod state;

use std::net::SocketAddr;
use std::sync::Arc;

pub use error::ServerError;
use state::ServerState;

#[derive(Debug, Clone)]
pub struct ServeConfig {
    pub addr: SocketAddr,
}

pub async fn serve(config: ServeConfig) -> Result<(), ServerError> {
    let state = Arc::new(ServerState::default());
    let app = api::router(state);
    let listener = tokio::net::TcpListener::bind(config.addr)
        .await
        .map_err(ServerError::Bind)?;

    axum::serve(listener, app).await.map_err(ServerError::Serve)
}
