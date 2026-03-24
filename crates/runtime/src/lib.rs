//! Runtime orchestration layer for Pera.

mod action;
mod catalog;
mod capabilities;
mod engine;
mod events;
mod fs;
mod in_memory;
mod run_executor;
mod skills;
pub mod interpreter;

pub use action::{
    ActionExecutionUpdate, ActionExecutor, ActionHandler, ActionProcessorError,
    InProcessActionExecutor, RejectingActionHandler, WasmtimeComponentActionExecutor,
};
pub use catalog::{FileSystemSkillCatalogLoader, FileSystemSkillRuntimeLoader, SkillRuntime};
pub use capabilities::{
    CapabilityProvider, CapabilityProviderError, CapabilityProviderRegistry,
    SqliteCapabilityProvider,
};

pub(crate) use action::ActionWorker;
pub use engine::{ExecutionEngine, ExecutionEngineError};
pub use events::{
    EventHub, EventHubPublisher, EventSubscription, StdoutEventPublisher, TeeEventPublisher,
};
pub use fs::{FileSystemEventLog, FileSystemRunStore};
pub use in_memory::{InMemoryRunStore, RecordingEventPublisher};
pub use run_executor::{RunExecutor, RunExecutorError, RunTransition, RunTransitionTrigger};
pub use skills::{
    FileSystemSkillRegistry, LoadedSkillProfile, LoadedWasmSkillRuntime, SkillBundle,
    SkillRegistry, SkillRegistryError,
};

#[cfg(test)]
mod tests;
