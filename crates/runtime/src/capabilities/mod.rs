use std::error::Error;
use std::fmt::{Display, Formatter};
use std::sync::Arc;

mod sqlite;

pub use sqlite::SqliteCapabilityProvider;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityProviderError {
    message: String,
}

impl CapabilityProviderError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl Display for CapabilityProviderError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl Error for CapabilityProviderError {}

impl From<rusqlite::Error> for CapabilityProviderError {
    fn from(value: rusqlite::Error) -> Self {
        Self::new(value.to_string())
    }
}

impl From<serde_json::Error> for CapabilityProviderError {
    fn from(value: serde_json::Error) -> Self {
        Self::new(value.to_string())
    }
}

pub trait CapabilityProvider: Send + Sync + 'static {
    fn capability_name(&self) -> &'static str;
}

#[derive(Debug, Clone, Default)]
pub struct CapabilityProviderRegistry {
    sqlite: Option<Arc<SqliteCapabilityProvider>>,
}

impl CapabilityProviderRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_sqlite(provider: SqliteCapabilityProvider) -> Self {
        Self {
            sqlite: Some(Arc::new(provider)),
        }
    }

    pub fn sqlite(&self) -> Option<Arc<SqliteCapabilityProvider>> {
        self.sqlite.clone()
    }
}
