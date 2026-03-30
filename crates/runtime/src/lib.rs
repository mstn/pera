//! Runtime orchestration layer for Pera.

mod action;
mod catalog;
mod capabilities;
mod agent_workspace;
mod code_tools;
mod engine;
mod events;
mod fs;
mod in_memory;
mod run_executor;
mod skills;
pub mod interpreter;

pub use action::{
    ActionExecutionUpdate, ActionExecutor, ActionProcessorError, WasmtimeComponentActionExecutor,
};
pub use catalog::{FileSystemSkillCatalogLoader, FileSystemSkillRuntimeLoader, SkillRuntime};
pub use capabilities::{
    CapabilityProvider, CapabilityProviderError, CapabilityProviderRegistry,
    SqliteCapabilityProvider,
};
pub use agent_workspace::{
    AgentWorkspace, AgentWorkspaceAction, AgentWorkspaceActionRunStatus,
    AgentWorkspaceActiveSkill, AgentWorkspaceAvailableSkill, AgentWorkspaceError,
    AgentWorkspaceEvent, AgentWorkspaceExecutionEngineHandle, AgentWorkspaceObservation,
    AgentWorkspaceOutcome, AgentWorkspaceSnapshot, AgentWorkspaceToolExecutor,
    SubmittedAgentWorkspaceAction,
};
pub use code_tools::{AgentWorkspaceTool, default_agent_workspace_tools};
pub type WorkspaceAction = AgentWorkspaceAction;
pub type WorkspaceActionRunStatus = AgentWorkspaceActionRunStatus;
pub type WorkspaceActiveSkill = AgentWorkspaceActiveSkill;
pub type WorkspaceAvailableSkill = AgentWorkspaceAvailableSkill;
pub type WorkspaceObservation = AgentWorkspaceObservation;
pub type WorkspaceOutcome = AgentWorkspaceOutcome;
pub type WorkspaceSnapshot = AgentWorkspaceSnapshot;
pub type WorkspaceToolDefinition = AgentWorkspaceTool;
pub type SubmittedWorkspaceAction = SubmittedAgentWorkspaceAction;
pub type WorkspaceParticipantDyn = dyn pera_orchestrator::Participant<
    Observation = WorkspaceObservation,
    Action = WorkspaceAction,
    Outcome = WorkspaceOutcome,
>;

pub(crate) use action::ActionWorker;
pub use engine::{ExecutionEngine, ExecutionEngineError};
pub use events::{
    EventHub, EventHubPublisher, EventSubscription, StdoutEventPublisher, TeeEventPublisher,
};
pub use fs::{FileSystemEventLog, FileSystemLayout, FileSystemRunStore};
pub use in_memory::{InMemoryRunStore, RecordingEventPublisher};
pub use run_executor::{RunExecutor, RunExecutorError, RunTransition, RunTransitionTrigger};
pub use skills::{
    FileSystemSkillRegistry, LoadedSkillProfile, LoadedWasmSkillRuntime, SkillBundle,
    SkillRegistry, SkillRegistryError,
};

#[cfg(test)]
mod tests;
