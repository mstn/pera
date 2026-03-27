//! Core domain types and trusted abstractions for Pera.

mod action;
mod code;
mod event;
mod ids;
mod interpreter;
mod run;
mod skill;
mod value;

pub use action::{CanonicalInvocation, ActionRecord, ActionRequest, ActionResult, ActionSkillRef, ActionStatus};
pub use code::{CodeArtifact, CodeLanguage, ScriptName};
pub use event::ExecutionEvent;
pub use ids::{ActionId, ActionName, CodeArtifactId, InputName, RunId, WorkItemId};
pub use interpreter::{
    CompiledProgram, ExecutionOutput, ExecutionSnapshot, ExternalCall, InputValues, Interpreter,
    InterpreterError, InterpreterKind, InterpreterStep, Suspension,
};
pub use run::{EventPublisher, ExecutionSession, ExecutionStatus, RunStore, StartExecutionRequest, StoreError};
pub use skill::{
    SkillBuildSpec, SkillDatabaseMigrationsSpec, SkillDatabaseSeedsSpec, SkillDatabaseSpec,
    SkillDefaults, SkillDescription, SkillInstructionsSpec, SkillManifest, SkillMetadata,
    SkillProfileManifest, SkillRuntimeArtifactSpec, SkillRuntimeKind, SkillRuntimeManifest,
    SkillVersion, WasmSkillBuildSpec, WasmSkillInterfaceSpec, WasmSkillRuntimeSpec,
};
pub use value::{CanonicalValue, Value};
