use std::error::Error;
use std::fmt::{Display, Formatter};
use std::collections::BTreeMap;
use std::sync::Arc;

mod sqlite;

pub use sqlite::SqliteCapabilityProvider;
pub(crate) use sqlite::{
    build_provider as build_sqlite_provider, matches_import as matches_sqlite_import,
    resolve_database_path as resolve_sqlite_database_path,
};

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

#[derive(Debug, Clone)]
pub enum CapabilityProviderHandle {
    Sqlite(Arc<SqliteCapabilityProvider>),
}

impl CapabilityProviderHandle {
    pub fn capability_name(&self) -> &'static str {
        match self {
            Self::Sqlite(provider) => provider.capability_name(),
        }
    }

    pub fn sqlite(&self) -> Option<Arc<SqliteCapabilityProvider>> {
        match self {
            Self::Sqlite(provider) => Some(Arc::clone(provider)),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct CapabilityProviderRegistry {
    providers: BTreeMap<String, CapabilityProviderHandle>,
}

impl CapabilityProviderRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_sqlite(provider: SqliteCapabilityProvider) -> Self {
        let mut registry = Self::new();
        registry.insert(CapabilityProviderHandle::Sqlite(Arc::new(provider)));
        registry
    }

    pub fn insert(&mut self, provider: CapabilityProviderHandle) {
        self.providers
            .insert(provider.capability_name().to_owned(), provider);
    }

    pub fn get(&self, capability_name: &str) -> Option<&CapabilityProviderHandle> {
        self.providers.get(capability_name)
    }

    pub fn sqlite(&self) -> Option<Arc<SqliteCapabilityProvider>> {
        self.get("sqlite").and_then(CapabilityProviderHandle::sqlite)
    }
}
