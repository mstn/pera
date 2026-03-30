use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::error::Error;
use std::fmt::{Display, Formatter};
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use pera_canonical::CatalogSkill;
use pera_canonical::render_python_stubs;
use pera_orchestrator::{
    ActionRunStatus, Environment, EnvironmentError, EnvironmentEvent, ParticipantId,
    SubmittedAction, TaskSpec,
};
#[cfg(feature = "monty")]
use pera_core::{
    ActionId, ActionRequest, ActionResult, ActionSkillRef, CanonicalInvocation, CanonicalValue,
    CodeArtifact, CodeArtifactId, CodeLanguage, ExecutionEvent, InputValues, RunId, ScriptName, SkillManifest,
    StartExecutionRequest, Value,
};
#[cfg(not(feature = "monty"))]
use pera_core::{
    ActionId, ActionRequest, ActionSkillRef, CanonicalInvocation, CanonicalValue, RunId,
    SkillManifest, StartExecutionRequest, Value,
};
use tokio::task::JoinHandle;
use tracing::debug;

use crate::AgentWorkspaceTool;
use crate::code_tools::default_agent_workspace_tools;
use crate::{
    ActionExecutionUpdate, ActionExecutor, EventSubscription, ExecutionEngine, SkillRuntime,
    WasmtimeComponentActionExecutor,
};
#[cfg(feature = "monty")]
use crate::{RunExecutor, interpreter::MontyInterpreter};
#[cfg(feature = "monty")]
use crate::{
    EventHub, FileSystemEventLog, FileSystemRunStore, FileSystemSkillRuntimeLoader,
    TeeEventPublisher,
};

fn catalog_skills_root(runtime_root: &std::path::Path) -> PathBuf {
    runtime_root.join("catalog").join("skills")
}

fn format_participant_id(participant: &ParticipantId) -> String {
    match participant {
        ParticipantId::Agent => "agent".to_owned(),
        ParticipantId::User => "user".to_owned(),
        ParticipantId::Custom(value) => value.clone(),
    }
}

fn parse_participant_id(value: String) -> ParticipantId {
    match value.as_str() {
        "agent" => ParticipantId::Agent,
        "user" => ParticipantId::User,
        _ => ParticipantId::Custom(value),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentWorkspaceObservation {
    pub available_tools: Vec<AgentWorkspaceTool>,
    pub available_skills: Vec<AgentWorkspaceAvailableSkill>,
    pub active_skills: Vec<AgentWorkspaceActiveSkill>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentWorkspaceAvailableSkill {
    pub skill_name: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentWorkspaceActiveSkill {
    pub skill_name: String,
    pub instructions: String,
    pub python_stub: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentWorkspaceAction {
    ExecuteCode {
        language: String,
        source: String,
    },
    LoadSkill {
        skill_name: String,
    },
    UnloadSkill {
        skill_name: String,
    },
    CallTool {
        skill: ActionSkillRef,
        invocation: CanonicalInvocation,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentWorkspaceOutcome {
    CodeExecuted {
        language: String,
        result: Value,
    },
    SkillLoaded {
        skill_name: String,
    },
    SkillUnloaded {
        skill_name: String,
    },
    ToolCall {
        skill: ActionSkillRef,
        invocation: CanonicalInvocation,
        value: CanonicalValue,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentWorkspaceEvent {
    ActionAccepted {
        actor: String,
        action_id: ActionId,
        action: AgentWorkspaceAction,
    },
    ActionRunStatus {
        actor: String,
        action_id: ActionId,
        run_id: RunId,
        status: AgentWorkspaceActionRunStatus,
    },
    ActionCompleted {
        actor: String,
        action_id: ActionId,
        outcome: AgentWorkspaceOutcome,
    },
    ActionFailed {
        actor: String,
        action_id: ActionId,
        error: String,
    },
    Notification {
        actor: String,
        message: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentWorkspaceActionRunStatus {
    RunSubmitted,
    RunStarted,
    ActionEnqueued {
        engine_action_id: ActionId,
    },
    ActionClaimed {
        engine_action_id: ActionId,
        worker_id: String,
    },
    ActionCompleted {
        engine_action_id: ActionId,
    },
    ActionFailed {
        engine_action_id: ActionId,
        message: String,
    },
    RunResumed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubmittedAgentWorkspaceAction {
    pub action_id: ActionId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentWorkspaceSnapshot;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentWorkspaceError {
    message: String,
}

impl AgentWorkspaceError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl Display for AgentWorkspaceError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl Error for AgentWorkspaceError {}

#[async_trait]
pub trait AgentWorkspaceToolExecutor: Send + Sync {
    async fn execute_tool(
        &self,
        request: ActionRequest,
    ) -> Result<CanonicalValue, AgentWorkspaceError>;
}

#[async_trait]
impl AgentWorkspaceToolExecutor for WasmtimeComponentActionExecutor {
    async fn execute_tool(
        &self,
        request: ActionRequest,
    ) -> Result<CanonicalValue, AgentWorkspaceError> {
        match self.execute(request).await {
            ActionExecutionUpdate::Completed(result) => Ok(result.value),
            ActionExecutionUpdate::Failed { message, .. } => {
                Err(AgentWorkspaceError::new(message))
            }
            ActionExecutionUpdate::Claimed { .. } => Err(AgentWorkspaceError::new(
                "unexpected claimed update returned by tool executor",
            )),
        }
    }
}

struct PendingCodeAction {
    actor: String,
    handle: JoinHandle<Result<AgentWorkspaceOutcome, AgentWorkspaceError>>,
}

#[derive(Debug, Clone)]
struct PendingExecutionRun {
    actor: String,
    language: String,
}

#[async_trait]
pub trait AgentWorkspaceExecutionEngineHandle: Send + Sync {
    async fn submit(
        &self,
        request: StartExecutionRequest,
    ) -> Result<RunId, AgentWorkspaceError>;
}

#[async_trait]
impl<I, S, P, A> AgentWorkspaceExecutionEngineHandle for ExecutionEngine<I, S, P, A>
where
    I: pera_core::Interpreter + Send + Sync + 'static,
    S: pera_core::RunStore + Send + Sync + 'static,
    P: pera_core::EventPublisher + Send + Sync + 'static,
    A: crate::ActionExecutor + Sync,
{
    async fn submit(
        &self,
        request: StartExecutionRequest,
    ) -> Result<RunId, AgentWorkspaceError> {
        ExecutionEngine::submit(self, request)
            .await
            .map_err(|error| AgentWorkspaceError::new(error.to_string()))
    }
}

pub struct AgentWorkspace {
    skill_runtime: Option<Arc<SkillRuntime>>,
    tool_executor: Option<Arc<dyn AgentWorkspaceToolExecutor>>,
    pending_actions: BTreeMap<ActionId, PendingCodeAction>,
    pending_execution_runs: BTreeMap<ActionId, PendingExecutionRun>,
    execution_runs_by_id: BTreeMap<RunId, ActionId>,
    execution_engine: Option<Arc<dyn AgentWorkspaceExecutionEngineHandle>>,
    execution_events: Option<EventSubscription>,
    active_skill_names: BTreeSet<String>,
}

impl std::fmt::Debug for AgentWorkspace {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentWorkspace")
            .field("has_skill_runtime", &self.skill_runtime.is_some())
            .field("has_tool_executor", &self.tool_executor.is_some())
            .field("pending_actions", &self.pending_actions.len())
            .field("pending_execution_runs", &self.pending_execution_runs.len())
            .finish()
    }
}

impl AgentWorkspace {
    #[cfg(feature = "monty")]
    pub async fn from_root(root: impl Into<PathBuf>) -> Result<Self, AgentWorkspaceError> {
        let root = root.into();
        let skill_runtime = FileSystemSkillRuntimeLoader::new(&root)
            .load()
            .map_err(|error| AgentWorkspaceError::new(error.to_string()))?;
        let action_executor = WasmtimeComponentActionExecutor::new(skill_runtime.clone())
            .map_err(|error| AgentWorkspaceError::new(error.to_string()))?;
        let event_hub = EventHub::new();
        let event_log = FileSystemEventLog::new(&root)
            .map_err(|error| AgentWorkspaceError::new(error.to_string()))?;
        let recovery_events = event_log
            .read_events()
            .map_err(|error| AgentWorkspaceError::new(error.to_string()))?;
        let publisher = TeeEventPublisher::new(event_log, event_hub.publisher());
        let run_executor =
            RunExecutor::with_skill_catalog(MontyInterpreter::new(), skill_runtime.catalog().clone());
        let execution_engine = Arc::new(ExecutionEngine::new(
            run_executor,
            FileSystemRunStore::new(&root)
                .map_err(|error| AgentWorkspaceError::new(error.to_string()))?,
            publisher,
            action_executor,
            event_hub,
        ));
        let execution_events = execution_engine.subscribe();
        execution_engine
            .recover_from_events(recovery_events)
            .await
            .map_err(|error| AgentWorkspaceError::new(error.to_string()))?;

        Self::new(
            Some(skill_runtime),
            None,
            Some(execution_engine),
            Some(execution_events),
        )
    }

    #[cfg(not(feature = "monty"))]
    pub async fn from_root(_root: impl Into<PathBuf>) -> Result<Self, AgentWorkspaceError> {
        Err(AgentWorkspaceError::new(
            "AgentWorkspace::from_root requires the 'monty' feature",
        ))
    }

    pub fn new(
        skill_runtime: Option<SkillRuntime>,
        tool_executor: Option<Arc<dyn AgentWorkspaceToolExecutor>>,
        execution_engine: Option<Arc<dyn AgentWorkspaceExecutionEngineHandle>>,
        execution_events: Option<EventSubscription>,
    ) -> Result<Self, AgentWorkspaceError> {
        let tool_executor = match tool_executor {
            Some(tool_executor) => Some(tool_executor),
            None => skill_runtime
                .clone()
                .map(WasmtimeComponentActionExecutor::new)
                .transpose()
                .map_err(|error| AgentWorkspaceError::new(error.to_string()))?
                .map(|executor| Arc::new(executor) as Arc<dyn AgentWorkspaceToolExecutor>),
        };

        Ok(Self {
            skill_runtime: skill_runtime.map(Arc::new),
            tool_executor,
            pending_actions: BTreeMap::new(),
            pending_execution_runs: BTreeMap::new(),
            execution_runs_by_id: BTreeMap::new(),
            execution_engine,
            execution_events,
            active_skill_names: BTreeSet::new(),
        })
    }

    pub fn with_tool_executor(
        skill_runtime: Option<SkillRuntime>,
        tool_executor: Arc<dyn AgentWorkspaceToolExecutor>,
        execution_engine: Option<Arc<dyn AgentWorkspaceExecutionEngineHandle>>,
        execution_events: Option<EventSubscription>,
    ) -> Result<Self, AgentWorkspaceError> {
        Self::new(
            skill_runtime,
            Some(tool_executor),
            execution_engine,
            execution_events,
        )
    }

    async fn reset_workspace(&mut self) -> Result<AgentWorkspaceObservation, AgentWorkspaceError> {
        self.pending_actions.clear();
        self.pending_execution_runs.clear();
        self.execution_runs_by_id.clear();
        self.observe_workspace().await
    }

    pub fn activate_skill(&mut self, skill_name: impl Into<String>) {
        self.active_skill_names.insert(skill_name.into());
    }

    pub fn deactivate_skill(&mut self, skill_name: &str) {
        self.active_skill_names.remove(skill_name);
    }

    async fn observe_workspace(&self) -> Result<AgentWorkspaceObservation, AgentWorkspaceError> {
        let (available_skills, active_skills) = match &self.skill_runtime {
            Some(runtime) => {
                let mut available_skills = Vec::new();
                let mut active_skills = Vec::new();

                for catalog_skill in runtime.catalog().skills() {
                    let skill_name = catalog_skill.metadata.skill_name.clone();
                    if self.active_skill_names.contains(&skill_name) {
                        let instructions =
                            active_skill_instructions(runtime.root(), catalog_skill).await?;
                        active_skills.push(AgentWorkspaceActiveSkill {
                            skill_name,
                            instructions,
                            python_stub: render_python_stubs(&catalog_skill.world),
                        });
                    } else {
                        let description =
                            available_skill_description(runtime.root(), catalog_skill);
                        available_skills.push(AgentWorkspaceAvailableSkill {
                            skill_name,
                            description,
                        });
                    }
                }

                (available_skills, active_skills)
            }
            None => (Vec::new(), Vec::new()),
        };

        debug!(
            skill_runtime_root = self
                .skill_runtime
                .as_ref()
                .map(|runtime| runtime.root().display().to_string())
                .unwrap_or_else(|| "<none>".to_owned()),
            catalog_skills_root = self
                .skill_runtime
                .as_ref()
                .map(|runtime| catalog_skills_root(runtime.root()).display().to_string())
                .unwrap_or_else(|| "<none>".to_owned()),
            available_skill_count = available_skills.len(),
            active_skill_count = active_skills.len(),
            active_skill_names = ?self.active_skill_names,
            "code environment observation prepared",
        );

        Ok(AgentWorkspaceObservation {
            available_tools: default_agent_workspace_tools(),
            available_skills,
            active_skills,
        })
    }

    async fn snapshot_workspace(&self) -> Result<AgentWorkspaceSnapshot, AgentWorkspaceError> {
        Ok(AgentWorkspaceSnapshot)
    }

    async fn restore_workspace(
        &mut self,
        _snapshot: &AgentWorkspaceSnapshot,
    ) -> Result<(), AgentWorkspaceError> {
        Ok(())
    }

    async fn step_workspace(
        &mut self,
        action: AgentWorkspaceAction,
    ) -> Result<AgentWorkspaceOutcome, AgentWorkspaceError> {
        self.run_action(action).await
    }

    async fn submit_workspace(
        &mut self,
        actor: String,
        action: AgentWorkspaceAction,
    ) -> Result<SubmittedAgentWorkspaceAction, AgentWorkspaceError> {
        if let AgentWorkspaceAction::ExecuteCode { language, source } = action {
            return self.submit_execute_code(actor, language, source).await;
        }
        let action_id = ActionId::generate();
        let outcome = self.run_action(action).await?;
        let handle = tokio::spawn(async move { Ok(outcome) });
        self.pending_actions
            .insert(action_id, PendingCodeAction { actor, handle });

        Ok(SubmittedAgentWorkspaceAction { action_id })
    }

    async fn poll_workspace_events(
        &mut self,
    ) -> Result<Vec<AgentWorkspaceEvent>, AgentWorkspaceError> {
        let ready_action_ids = self
            .pending_actions
            .iter()
            .filter_map(|(action_id, pending)| pending.handle.is_finished().then_some(*action_id))
            .collect::<Vec<_>>();
        let mut events = Vec::new();

        for action_id in ready_action_ids {
            let pending = self.pending_actions.remove(&action_id).ok_or_else(|| {
                AgentWorkspaceError::new(format!("missing pending action '{action_id}'"))
            })?;
            let actor = pending.actor;
            match pending.handle.await {
                Ok(Ok(outcome)) => {
                    events.push(AgentWorkspaceEvent::ActionCompleted {
                        actor,
                        action_id,
                        outcome,
                    });
                }
                Ok(Err(error)) => {
                    events.push(AgentWorkspaceEvent::ActionFailed {
                        actor,
                        action_id,
                        error: error.to_string(),
                    });
                }
                Err(error) => {
                    events.push(AgentWorkspaceEvent::ActionFailed {
                        actor,
                        action_id,
                        error: error.to_string(),
                    });
                }
            }
        }

        #[cfg(feature = "monty")]
        if let Some(subscription) = &mut self.execution_events {
            let mut execution_events = Vec::new();
            loop {
                let Some(event) = subscription
                    .try_recv()
                    .map_err(|error| AgentWorkspaceError::new(error.to_string()))?
                else {
                    break;
                };
                execution_events.push(event);
            }

            for event in execution_events {
                if let Some(mapped) = self.translate_execution_event(event) {
                    events.push(mapped?);
                }
            }
        }

        Ok(events)
    }

    async fn run_action(
        &mut self,
        action: AgentWorkspaceAction,
    ) -> Result<AgentWorkspaceOutcome, AgentWorkspaceError> {
        match action {
            AgentWorkspaceAction::ExecuteCode { language, source } => {
                self.execute_code(language, source).await
            }
            AgentWorkspaceAction::LoadSkill { skill_name } => self.load_skill(skill_name),
            AgentWorkspaceAction::UnloadSkill { skill_name } => Ok(self.unload_skill(skill_name)),
            AgentWorkspaceAction::CallTool { skill, invocation } => {
                call_tool(self.tool_executor.clone(), skill, invocation).await
            }
        }
    }

    fn load_skill(
        &mut self,
        skill_name: String,
    ) -> Result<AgentWorkspaceOutcome, AgentWorkspaceError> {
        if !self.skill_exists(&skill_name) {
            return Err(AgentWorkspaceError::new(format!(
                "skill '{skill_name}' does not exist in the catalog"
            )));
        }
        self.active_skill_names.insert(skill_name.clone());
        Ok(AgentWorkspaceOutcome::SkillLoaded { skill_name })
    }

    fn unload_skill(&mut self, skill_name: String) -> AgentWorkspaceOutcome {
        self.active_skill_names.remove(&skill_name);
        AgentWorkspaceOutcome::SkillUnloaded { skill_name }
    }

    fn skill_exists(&self, skill_name: &str) -> bool {
        self.skill_runtime.as_ref().is_some_and(|runtime| {
            runtime
                .catalog()
                .skills()
                .any(|skill| skill.metadata.skill_name == skill_name)
        })
    }

    #[cfg(feature = "monty")]
    async fn execute_code(
        &self,
        language: String,
        source: String,
    ) -> Result<AgentWorkspaceOutcome, AgentWorkspaceError> {
        let runtime = self
            .skill_runtime
            .as_ref()
            .ok_or_else(|| AgentWorkspaceError::new("no skill runtime is configured"))?;
        let tool_executor = self
            .tool_executor
            .as_ref()
            .ok_or_else(|| AgentWorkspaceError::new("no tool executor is configured"))?;

        let code_language = match language.as_str() {
            "python" => CodeLanguage::Python,
            other => {
                return Err(AgentWorkspaceError::new(format!(
                    "unsupported execute_code language '{other}'"
                )));
            }
        };

        let executor =
            RunExecutor::with_skill_catalog(MontyInterpreter::new(), runtime.catalog().clone());
        let mut transition = executor
            .start_run(
                pera_core::StartExecutionRequest {
                    code: CodeArtifact {
                        id: CodeArtifactId::generate(),
                        language: code_language,
                        script_name: ScriptName::new("execute_code"),
                        source,
                        inputs: Vec::new(),
                    },
                    inputs: InputValues::new(),
                },
                RunId::generate(),
                CodeArtifactId::generate(),
                ActionId::generate,
            )
            .map_err(|error| AgentWorkspaceError::new(error.to_string()))?;

        loop {
            if let Some(action_request) = transition.action_to_enqueue.clone() {
                let action_id = action_request.id;
                let value = tool_executor.execute_tool(action_request.clone()).await?;
                transition = executor
                    .complete_action(
                        transition.session,
                        action_request,
                        ActionResult { action_id, value },
                        ActionId::generate,
                    )
                    .map_err(|error| AgentWorkspaceError::new(error.to_string()))?;
                continue;
            }

            return match transition.session.status {
                pera_core::ExecutionStatus::Completed(output) => {
                    Ok(AgentWorkspaceOutcome::CodeExecuted {
                        language,
                        result: output.value,
                    })
                }
                pera_core::ExecutionStatus::Failed(message) => {
                    Err(AgentWorkspaceError::new(message))
                }
                other => Err(AgentWorkspaceError::new(format!(
                    "unexpected execution status after execute_code: {other:?}"
                ))),
            };
        }
    }

    #[cfg(not(feature = "monty"))]
    async fn execute_code(
        &self,
        _language: String,
        _source: String,
    ) -> Result<AgentWorkspaceOutcome, AgentWorkspaceError> {
        Err(AgentWorkspaceError::new(
            "execute_code requires the 'monty' feature",
        ))
    }

    #[cfg(feature = "monty")]
    async fn submit_execute_code(
        &mut self,
        actor: String,
        language: String,
        source: String,
    ) -> Result<SubmittedAgentWorkspaceAction, AgentWorkspaceError> {
        let engine = self
            .execution_engine
            .as_ref()
            .ok_or_else(|| AgentWorkspaceError::new("no execution engine is configured"))?;
        let code_language = parse_code_language(&language)?;
        let request = pera_core::StartExecutionRequest {
            code: CodeArtifact {
                id: CodeArtifactId::generate(),
                language: code_language,
                script_name: ScriptName::new("execute_code"),
                source,
                inputs: Vec::new(),
            },
            inputs: InputValues::new(),
        };
        let action_id = ActionId::generate();
        let run_id = engine
            .submit(request)
            .await
            .map_err(|error| AgentWorkspaceError::new(error.to_string()))?;
        self.execution_runs_by_id.insert(run_id, action_id);
        self.pending_execution_runs
            .insert(action_id, PendingExecutionRun { actor, language });
        Ok(SubmittedAgentWorkspaceAction { action_id })
    }

    #[cfg(not(feature = "monty"))]
    async fn submit_execute_code(
        &mut self,
        _actor: String,
        _language: String,
        _source: String,
    ) -> Result<SubmittedAgentWorkspaceAction, AgentWorkspaceError> {
        Err(AgentWorkspaceError::new(
            "execute_code requires the 'monty' feature",
        ))
    }

    #[cfg(feature = "monty")]
    fn translate_execution_event(
        &mut self,
        event: ExecutionEvent,
    ) -> Option<Result<AgentWorkspaceEvent, AgentWorkspaceError>> {
        let run_id = event.run_id();
        let action_id = *self.execution_runs_by_id.get(&run_id)?;
        let pending = self.pending_execution_runs.get(&action_id)?.clone();

        Some(match event {
            ExecutionEvent::RunSubmitted { .. } => Ok(AgentWorkspaceEvent::ActionRunStatus {
                actor: pending.actor,
                action_id,
                run_id,
                status: AgentWorkspaceActionRunStatus::RunSubmitted,
            }),
            ExecutionEvent::RunStarted { .. } => Ok(AgentWorkspaceEvent::ActionRunStatus {
                actor: pending.actor,
                action_id,
                run_id,
                status: AgentWorkspaceActionRunStatus::RunStarted,
            }),
            ExecutionEvent::ActionEnqueued {
                action_id: engine_action_id,
                ..
            } => Ok(AgentWorkspaceEvent::ActionRunStatus {
                actor: pending.actor,
                action_id,
                run_id,
                status: AgentWorkspaceActionRunStatus::ActionEnqueued { engine_action_id },
            }),
            ExecutionEvent::ActionClaimed {
                action_id: engine_action_id,
                worker_id,
                ..
            } => Ok(AgentWorkspaceEvent::ActionRunStatus {
                actor: pending.actor,
                action_id,
                run_id,
                status: AgentWorkspaceActionRunStatus::ActionClaimed {
                    engine_action_id,
                    worker_id,
                },
            }),
            ExecutionEvent::ActionCompleted {
                action_id: engine_action_id,
                ..
            } => Ok(AgentWorkspaceEvent::ActionRunStatus {
                actor: pending.actor,
                action_id,
                run_id,
                status: AgentWorkspaceActionRunStatus::ActionCompleted { engine_action_id },
            }),
            ExecutionEvent::RunResumed { .. } => Ok(AgentWorkspaceEvent::ActionRunStatus {
                actor: pending.actor,
                action_id,
                run_id,
                status: AgentWorkspaceActionRunStatus::RunResumed,
            }),
            ExecutionEvent::RunCompleted { value, .. } => {
                self.execution_runs_by_id.remove(&run_id);
                self.pending_execution_runs.remove(&action_id);
                Ok(AgentWorkspaceEvent::ActionCompleted {
                    actor: pending.actor,
                    action_id,
                    outcome: AgentWorkspaceOutcome::CodeExecuted {
                        language: pending.language,
                        result: value,
                    },
                })
            }
            ExecutionEvent::RunFailed { message, .. } => {
                self.execution_runs_by_id.remove(&run_id);
                self.pending_execution_runs.remove(&action_id);
                Ok(AgentWorkspaceEvent::ActionFailed {
                    actor: pending.actor,
                    action_id,
                    error: message,
                })
            }
            ExecutionEvent::ActionFailed {
                action_id: engine_action_id,
                message,
                ..
            } => Ok(AgentWorkspaceEvent::ActionRunStatus {
                actor: pending.actor,
                action_id,
                run_id,
                status: AgentWorkspaceActionRunStatus::ActionFailed {
                    engine_action_id,
                    message,
                },
            }),
        })
    }
}

#[async_trait]
impl Environment for AgentWorkspace {
    type Observation = AgentWorkspaceObservation;
    type Action = AgentWorkspaceAction;
    type Outcome = AgentWorkspaceOutcome;
    type Snapshot = AgentWorkspaceSnapshot;

    async fn reset(&mut self, _task: &TaskSpec) -> Result<Self::Observation, EnvironmentError> {
        AgentWorkspace::reset_workspace(self)
            .await
            .map_err(|error| EnvironmentError::new(error.to_string()))
    }

    async fn observe(&self) -> Result<Self::Observation, EnvironmentError> {
        AgentWorkspace::observe_workspace(self)
            .await
            .map_err(|error| EnvironmentError::new(error.to_string()))
    }

    async fn step(
        &mut self,
        _actor: ParticipantId,
        action: Self::Action,
    ) -> Result<Self::Outcome, EnvironmentError> {
        AgentWorkspace::step_workspace(self, action)
            .await
            .map_err(|error| EnvironmentError::new(error.to_string()))
    }

    async fn submit(
        &mut self,
        actor: ParticipantId,
        action: Self::Action,
    ) -> Result<SubmittedAction, EnvironmentError> {
        AgentWorkspace::submit_workspace(self, format_participant_id(&actor), action)
            .await
            .map(|submitted| SubmittedAction {
                action_id: submitted.action_id,
            })
            .map_err(|error| EnvironmentError::new(error.to_string()))
    }

    async fn poll_events(
        &mut self,
    ) -> Result<Vec<EnvironmentEvent<Self::Action, Self::Outcome>>, EnvironmentError> {
        let events = AgentWorkspace::poll_workspace_events(self)
            .await
            .map_err(|error| EnvironmentError::new(error.to_string()))?;
        Ok(events
            .into_iter()
            .map(agent_workspace_event_to_environment_event)
            .collect())
    }

    async fn snapshot(&self) -> Result<Self::Snapshot, EnvironmentError> {
        AgentWorkspace::snapshot_workspace(self)
            .await
            .map_err(|error| EnvironmentError::new(error.to_string()))
    }

    async fn restore(&mut self, snapshot: &Self::Snapshot) -> Result<(), EnvironmentError> {
        AgentWorkspace::restore_workspace(self, snapshot)
            .await
            .map_err(|error| EnvironmentError::new(error.to_string()))
    }

    async fn terminal_status(&self) -> Result<Option<String>, EnvironmentError> {
        Ok(None)
    }
}

fn agent_workspace_event_to_environment_event(
    event: AgentWorkspaceEvent,
) -> EnvironmentEvent<AgentWorkspaceAction, AgentWorkspaceOutcome> {
    match event {
        AgentWorkspaceEvent::ActionAccepted {
            actor,
            action_id,
            action,
        } => EnvironmentEvent::ActionAccepted {
            participant: parse_participant_id(actor),
            action_id,
            action,
        },
        AgentWorkspaceEvent::ActionRunStatus {
            actor,
            action_id,
            run_id,
            status,
        } => EnvironmentEvent::ActionRunStatus {
            participant: parse_participant_id(actor),
            action_id,
            run_id,
            status: workspace_status_to_action_run_status(status),
        },
        AgentWorkspaceEvent::ActionCompleted {
            actor,
            action_id,
            outcome,
        } => EnvironmentEvent::ActionCompleted {
            participant: parse_participant_id(actor),
            action_id,
            outcome,
        },
        AgentWorkspaceEvent::ActionFailed {
            actor,
            action_id,
            error,
        } => EnvironmentEvent::ActionFailed {
            participant: parse_participant_id(actor),
            action_id,
            error,
        },
        AgentWorkspaceEvent::Notification { actor, message } => EnvironmentEvent::Notification {
            participant: parse_participant_id(actor),
            message,
        },
    }
}

fn workspace_status_to_action_run_status(status: AgentWorkspaceActionRunStatus) -> ActionRunStatus {
    match status {
        AgentWorkspaceActionRunStatus::RunSubmitted => ActionRunStatus::RunSubmitted,
        AgentWorkspaceActionRunStatus::RunStarted => ActionRunStatus::RunStarted,
        AgentWorkspaceActionRunStatus::ActionEnqueued { engine_action_id } => {
            ActionRunStatus::ActionEnqueued { engine_action_id }
        }
        AgentWorkspaceActionRunStatus::ActionClaimed {
            engine_action_id,
            worker_id,
        } => ActionRunStatus::ActionClaimed {
            engine_action_id,
            worker_id,
        },
        AgentWorkspaceActionRunStatus::ActionCompleted { engine_action_id } => {
            ActionRunStatus::ActionCompleted { engine_action_id }
        }
        AgentWorkspaceActionRunStatus::ActionFailed {
            engine_action_id,
            message,
        } => ActionRunStatus::ActionFailed {
            engine_action_id,
            message,
        },
        AgentWorkspaceActionRunStatus::RunResumed => ActionRunStatus::RunResumed,
    }
}

#[cfg(feature = "monty")]
fn parse_code_language(language: &str) -> Result<CodeLanguage, AgentWorkspaceError> {
    match language {
        "python" => Ok(CodeLanguage::Python),
        other => Err(AgentWorkspaceError::new(format!(
            "unsupported execute_code language '{other}'"
        ))),
    }
}

async fn call_tool(
    tool_executor: Option<Arc<dyn AgentWorkspaceToolExecutor>>,
    skill: ActionSkillRef,
    invocation: CanonicalInvocation,
) -> Result<AgentWorkspaceOutcome, AgentWorkspaceError> {
    let tool_executor = tool_executor
        .as_ref()
        .ok_or_else(|| AgentWorkspaceError::new("no tool executor is configured"))?;

    let value = tool_executor
        .execute_tool(ActionRequest {
            id: ActionId::generate(),
            run_id: RunId::generate(),
            skill: skill.clone(),
            invocation: invocation.clone(),
        })
        .await?;

    Ok(AgentWorkspaceOutcome::ToolCall {
        skill,
        invocation,
        value,
    })
}

async fn active_skill_instructions(
    runtime_root: &Path,
    catalog_skill: &CatalogSkill,
) -> Result<String, AgentWorkspaceError> {
    let (profile_dir, manifest) = compiled_catalog_profile(runtime_root, catalog_skill)?;
    let Some(instructions) = manifest.defaults.instructions.as_ref() else {
        return Ok(String::new());
    };
    let instructions_path = profile_dir.join(&instructions.source);
    tokio::fs::read_to_string(&instructions_path)
        .await
        .map_err(|error| AgentWorkspaceError::new(error.to_string()))
}

fn available_skill_description(runtime_root: &Path, catalog_skill: &CatalogSkill) -> String {
    let Ok((profile_dir, manifest)) = compiled_catalog_profile(runtime_root, catalog_skill) else {
        return String::new();
    };
    let Some(instructions) = manifest.defaults.instructions.as_ref() else {
        return manifest.skill.description;
    };
    let instructions_path = profile_dir.join(&instructions.source);
    let Ok(source) = std::fs::read_to_string(&instructions_path) else {
        return manifest.skill.description;
    };

    frontmatter_when_to_use(&source)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(manifest.skill.description)
}

fn compiled_catalog_profile(
    runtime_root: &Path,
    catalog_skill: &CatalogSkill,
) -> Result<(PathBuf, SkillManifest), AgentWorkspaceError> {
    let skill_version = catalog_skill
        .metadata
        .skill_version
        .as_deref()
        .ok_or_else(|| AgentWorkspaceError::new("catalog skill is missing skill_version"))?;
    let profile_name = catalog_skill
        .metadata
        .profile_name
        .as_deref()
        .ok_or_else(|| AgentWorkspaceError::new("catalog skill is missing profile_name"))?;
    let profile_dir = catalog_skills_root(runtime_root)
        .join(&catalog_skill.metadata.skill_name)
        .join(skill_version)
        .join(profile_name);
    let manifest_path = resolve_manifest_path(&profile_dir)?;
    let manifest_source = std::fs::read_to_string(&manifest_path)
        .map_err(|error| AgentWorkspaceError::new(error.to_string()))?;
    let manifest = serde_yaml::from_str(&manifest_source)
        .map_err(|error| AgentWorkspaceError::new(error.to_string()))?;
    Ok((profile_dir, manifest))
}

fn resolve_manifest_path(profile_dir: &Path) -> Result<PathBuf, AgentWorkspaceError> {
    for candidate in ["manifest.yaml", "skill.yaml", "skill.yml"] {
        let path = profile_dir.join(candidate);
        if path.exists() {
            return Ok(path);
        }
    }

    Err(AgentWorkspaceError::new(format!(
        "no manifest found in {}",
        profile_dir.display()
    )))
}

fn frontmatter_when_to_use(source: &str) -> Option<String> {
    let mut lines = source.lines();
    if lines.next()?.trim() != "---" {
        return None;
    }

    let mut frontmatter = String::new();
    for line in lines {
        if line.trim() == "---" {
            break;
        }
        frontmatter.push_str(line);
        frontmatter.push('\n');
    }

    let value: serde_yaml::Value = serde_yaml::from_str(&frontmatter).ok()?;
    value
        .get("when_to_use")
        .and_then(serde_yaml::Value::as_str)
        .map(ToOwned::to_owned)
}
