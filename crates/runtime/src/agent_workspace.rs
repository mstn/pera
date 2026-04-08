use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::error::Error;
use std::fmt::{Display, Formatter};
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use pera_canonical::{CatalogSkill, SkillCatalog};
use pera_canonical::render_python_stubs;
use pera_orchestrator::{
    ActionError, ActionErrorOrigin, ActionRunStatus, Environment, EnvironmentError,
    EnvironmentEvent, ParticipantId, ScheduledAction, TaskSpec,
};
use pera_core::{
    ActionId, CodeArtifact, CodeArtifactId, CodeLanguage, ExecutionEvent, InputValues,
    RunId, ScriptName, SkillManifest, StartExecutionRequest, Value, ExecutionSnapshot,
    ExecutionStatus,
};
use tracing::{debug, info, warn};

use crate::AgentWorkspaceTool;
use crate::code_tools::agent_workspace_tools;
use crate::{EventSubscription, ExecutionEngine, SkillRuntime, WasmtimeComponentActionExecutor};
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

fn action_error_from_workspace_error(error: AgentWorkspaceError) -> ActionError {
    let detail = error.to_string();
    if detail.contains("interpreter error:") {
        ActionError {
            user_message: "The generated code could not be executed.".to_owned(),
            detail,
            origin: ActionErrorOrigin::Interpreter,
        }
    } else {
        ActionError {
            user_message: "The requested action could not be started.".to_owned(),
            detail,
            origin: ActionErrorOrigin::Environment,
        }
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
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentWorkspaceOutcome {
    CodeExecuted {
        language: String,
        result: Option<Value>,
    },
    SkillLoaded {
        skill_name: String,
    },
    SkillUnloaded {
        skill_name: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentWorkspaceEvent {
    ActionScheduled {
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
        error: ActionError,
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
        skill_name: String,
        action_name: String,
    },
    ActionClaimed {
        engine_action_id: ActionId,
        skill_name: String,
        action_name: String,
        worker_id: String,
    },
    ActionCompleted {
        engine_action_id: ActionId,
        skill_name: String,
        action_name: String,
    },
    ActionFailed {
        engine_action_id: ActionId,
        skill_name: String,
        action_name: String,
        message: String,
    },
    RunResumed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScheduledAgentWorkspaceAction {
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

    fn run_status(&self, run_id: RunId) -> Option<ExecutionStatus>;
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

    fn run_status(&self, run_id: RunId) -> Option<ExecutionStatus> {
        ExecutionEngine::run_status(self, run_id)
    }
}

pub struct AgentWorkspace {
    skill_runtime: Arc<SkillRuntime>,
    queued_events: Vec<AgentWorkspaceEvent>,
    pending_execution_runs: BTreeMap<ActionId, PendingExecutionRun>,
    execution_runs_by_id: BTreeMap<RunId, ActionId>,
    execution_engine: Arc<dyn AgentWorkspaceExecutionEngineHandle>,
    execution_events: EventSubscription,
    active_skill_names: BTreeSet<String>,
    repl_states_by_actor: BTreeMap<String, ExecutionSnapshot>,
}

impl std::fmt::Debug for AgentWorkspace {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentWorkspace")
            .field("pending_execution_runs", &self.pending_execution_runs.len())
            .finish()
    }
}

impl AgentWorkspace {
    pub async fn from_root(root: impl Into<PathBuf>) -> Result<Self, AgentWorkspaceError> {
        Self::from_root_with_catalog_entries(root, None).await
    }

    pub async fn from_root_with_catalog_entries(
        root: impl Into<PathBuf>,
        allowed_catalog_entries: Option<&[(String, String)]>,
    ) -> Result<Self, AgentWorkspaceError> {
        let root = root.into();
        let mut skill_runtime = FileSystemSkillRuntimeLoader::new(&root)
            .load()
            .map_err(|error| AgentWorkspaceError::new(error.to_string()))?;
        if let Some(allowed_catalog_entries) = allowed_catalog_entries {
            skill_runtime = filtered_skill_runtime(&root, &skill_runtime, allowed_catalog_entries)?;
        }
        let event_hub = EventHub::new();
        let event_log = FileSystemEventLog::new(&root)
            .map_err(|error| AgentWorkspaceError::new(error.to_string()))?;
        let recovery_events = event_log
            .read_events()
            .map_err(|error| AgentWorkspaceError::new(error.to_string()))?;
        let publisher = TeeEventPublisher::new(event_log, event_hub.publisher());
        let execution_engine = Arc::new(ExecutionEngine::new(
            crate::RunExecutor::with_skill_catalog(
                crate::interpreter::MontyInterpreter::new(),
                skill_runtime.catalog().clone(),
            ),
            FileSystemRunStore::new(&root)
                .map_err(|error| AgentWorkspaceError::new(error.to_string()))?,
            publisher,
            WasmtimeComponentActionExecutor::new(skill_runtime.clone())
                .map_err(|error| AgentWorkspaceError::new(error.to_string()))?,
            event_hub,
        ));
        let execution_events = execution_engine.subscribe();
        execution_engine
            .recover_from_events(recovery_events)
            .await
            .map_err(|error| AgentWorkspaceError::new(error.to_string()))?;

        Self::new(
            skill_runtime,
            execution_engine,
            execution_events,
        )
    }

    pub fn new(
        skill_runtime: SkillRuntime,
        execution_engine: Arc<dyn AgentWorkspaceExecutionEngineHandle>,
        execution_events: EventSubscription,
    ) -> Result<Self, AgentWorkspaceError> {
        Ok(Self {
            skill_runtime: Arc::new(skill_runtime),
            queued_events: Vec::new(),
            pending_execution_runs: BTreeMap::new(),
            execution_runs_by_id: BTreeMap::new(),
            execution_engine,
            execution_events,
            active_skill_names: BTreeSet::new(),
            repl_states_by_actor: BTreeMap::new(),
        })
    }

    pub fn activate_skill(&mut self, skill_name: impl Into<String>) {
        self.active_skill_names.insert(skill_name.into());
    }

    pub async fn warm_catalog_skills(&self) -> Result<(), AgentWorkspaceError> {
        self.skill_runtime
            .warm_components()
            .await
            .map_err(|error| AgentWorkspaceError::new(error.to_string()))
    }

    pub fn deactivate_skill(&mut self, skill_name: &str) {
        self.active_skill_names.remove(skill_name);
    }

    async fn describe_workspace(&self) -> Result<AgentWorkspaceObservation, AgentWorkspaceError> {
        let runtime = &self.skill_runtime;
        let mut available_skills = Vec::new();
        let mut active_skills = Vec::new();

        for catalog_skill in runtime.catalog().skills() {
            let skill_name = catalog_skill.metadata.skill_name.clone();
            if self.active_skill_names.contains(&skill_name) {
                let instructions = active_skill_instructions(runtime.root(), catalog_skill).await?;
                active_skills.push(AgentWorkspaceActiveSkill {
                    skill_name,
                    instructions,
                    python_stub: render_python_stubs(&catalog_skill.world),
                });
            } else {
                let description = available_skill_description(runtime.root(), catalog_skill);
                available_skills.push(AgentWorkspaceAvailableSkill {
                    skill_name,
                    description,
                });
            }
        }

        debug!(
            skill_runtime_root = runtime.root().display().to_string(),
            catalog_skills_root = catalog_skills_root(runtime.root()).display().to_string(),
            available_skill_count = available_skills.len(),
            active_skill_count = active_skills.len(),
            active_skill_names = ?self.active_skill_names,
            "code environment observation prepared",
        );

        let available_skill_names = available_skills
            .iter()
            .map(|skill| skill.skill_name.clone())
            .collect::<Vec<_>>();
        let active_skill_names = active_skills
            .iter()
            .map(|skill| skill.skill_name.clone())
            .collect::<Vec<_>>();

        Ok(AgentWorkspaceObservation {
            available_tools: agent_workspace_tools(
                &available_skill_names,
                &active_skill_names,
            ),
            available_skills,
            active_skills,
        })
    }

    async fn collect_pending_events(
        &mut self,
    ) -> Result<Vec<AgentWorkspaceEvent>, AgentWorkspaceError> {
        let mut events = Vec::new();

        events.append(&mut self.queued_events);

        let mut execution_events = Vec::new();
        loop {
            let Some(event) = self
                .execution_events
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

        Ok(events)
    }

    async fn run_action(
        &mut self,
        action: AgentWorkspaceAction,
    ) -> Result<AgentWorkspaceOutcome, AgentWorkspaceError> {
        match action {
            AgentWorkspaceAction::ExecuteCode { .. } => Err(AgentWorkspaceError::new(
                "execute_code must be scheduled through the execution engine",
            )),
            AgentWorkspaceAction::LoadSkill { skill_name } => self.load_skill(skill_name),
            AgentWorkspaceAction::UnloadSkill { skill_name } => Ok(self.unload_skill(skill_name)),
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
        self.skill_runtime
            .catalog()
            .skills()
            .any(|skill| skill.metadata.skill_name == skill_name)
    }

    async fn submit_execute_code(
        &mut self,
        actor: String,
        language: String,
        source: String,
    ) -> Result<ScheduledAgentWorkspaceAction, AgentWorkspaceError> {
        info!(
            actor,
            language,
            source_len = source.len(),
            "agent workspace submitting execute_code"
        );
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
            repl_state: self.repl_states_by_actor.get(&actor).cloned(),
        };
        let action_id = ActionId::generate();
        let run_id = self
            .execution_engine
            .submit(request)
            .await
            .map_err(|error| {
                warn!(
                    actor,
                    language,
                    action_id = %action_id,
                    error = %error,
                    "agent workspace failed to submit execute_code to execution engine"
                );
                AgentWorkspaceError::new(error.to_string())
            })?;
        info!(
            actor,
            language,
            action_id = %action_id,
            run_id = %run_id,
            "agent workspace submitted execute_code to execution engine"
        );
        self.execution_runs_by_id.insert(run_id, action_id);
        self.pending_execution_runs
            .insert(action_id, PendingExecutionRun { actor, language });
        self.queued_events
            .push(AgentWorkspaceEvent::ActionRunStatus {
                actor: self
                    .pending_execution_runs
                    .get(&action_id)
                    .expect("pending execution run must exist after insertion")
                    .actor
                    .clone(),
                action_id,
                run_id,
                status: AgentWorkspaceActionRunStatus::RunSubmitted,
            });
        Ok(ScheduledAgentWorkspaceAction { action_id })
    }

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
                skill_name,
                action_name,
                ..
            } => Ok(AgentWorkspaceEvent::ActionRunStatus {
                actor: pending.actor,
                action_id,
                run_id,
                status: AgentWorkspaceActionRunStatus::ActionEnqueued {
                    engine_action_id,
                    skill_name,
                    action_name,
                },
            }),
            ExecutionEvent::ActionClaimed {
                action_id: engine_action_id,
                skill_name,
                action_name,
                worker_id,
                ..
            } => Ok(AgentWorkspaceEvent::ActionRunStatus {
                actor: pending.actor,
                action_id,
                run_id,
                status: AgentWorkspaceActionRunStatus::ActionClaimed {
                    engine_action_id,
                    skill_name,
                    action_name,
                    worker_id,
                },
            }),
            ExecutionEvent::ActionCompleted {
                action_id: engine_action_id,
                skill_name,
                action_name,
                ..
            } => Ok(AgentWorkspaceEvent::ActionRunStatus {
                actor: pending.actor,
                action_id,
                run_id,
                status: AgentWorkspaceActionRunStatus::ActionCompleted {
                    engine_action_id,
                    skill_name,
                    action_name,
                },
            }),
            ExecutionEvent::RunResumed { .. } => Ok(AgentWorkspaceEvent::ActionRunStatus {
                actor: pending.actor,
                action_id,
                run_id,
                status: AgentWorkspaceActionRunStatus::RunResumed,
            }),
            ExecutionEvent::RunCompleted { value, .. } => {
                let repl_state = match self.execution_engine.run_status(run_id) {
                    Some(ExecutionStatus::Completed(output)) => output.repl_state,
                    _ => None,
                };
                if let Some(repl_state) = repl_state {
                    self.repl_states_by_actor
                        .insert(pending.actor.clone(), repl_state);
                }
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
                    error: ActionError {
                        user_message: "The generated code could not be executed.".to_owned(),
                        detail: message,
                        origin: ActionErrorOrigin::Interpreter,
                    },
                })
            }
            ExecutionEvent::ActionFailed {
                action_id: engine_action_id,
                skill_name,
                action_name,
                message,
                ..
            } => Ok(AgentWorkspaceEvent::ActionRunStatus {
                actor: pending.actor,
                action_id,
                run_id,
                status: AgentWorkspaceActionRunStatus::ActionFailed {
                    engine_action_id,
                    skill_name,
                    action_name,
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
        self.queued_events.clear();
        self.pending_execution_runs.clear();
        self.execution_runs_by_id.clear();
        self.repl_states_by_actor.clear();
        self.describe_workspace()
            .await
            .map_err(|error| EnvironmentError::new(error.to_string()))
    }

    async fn observe(&self) -> Result<Self::Observation, EnvironmentError> {
        self.describe_workspace()
            .await
            .map_err(|error| EnvironmentError::new(error.to_string()))
    }

    async fn perform_now(
        &mut self,
        _actor: ParticipantId,
        action: Self::Action,
    ) -> Result<Self::Outcome, EnvironmentError> {
        self.run_action(action)
            .await
            .map_err(|error| EnvironmentError::new(error.to_string()))
    }

    async fn schedule(
        &mut self,
        actor: ParticipantId,
        action: Self::Action,
    ) -> Result<ScheduledAction, ActionError> {
        let actor = format_participant_id(&actor);
        let scheduled = match action {
            AgentWorkspaceAction::ExecuteCode { language, source } => {
                debug!(
                    actor,
                    language,
                    source_len = source.len(),
                    "agent workspace received deferred execute_code action"
                );
                self.submit_execute_code(actor, language, source).await
            }
            _ => Err(AgentWorkspaceError::new(
                "only deferred actions can be scheduled; use perform_now for immediate actions",
            )),
        };

        scheduled.map(|scheduled| ScheduledAction {
            action_id: scheduled.action_id,
        }).map_err(action_error_from_workspace_error)
    }

    async fn poll_events(
        &mut self,
    ) -> Result<Vec<EnvironmentEvent<Self::Action, Self::Outcome>>, EnvironmentError> {
        let events = self
            .collect_pending_events()
            .await
            .map_err(|error| EnvironmentError::new(error.to_string()))?;
        Ok(events
            .into_iter()
            .map(agent_workspace_event_to_environment_event)
            .collect())
    }

    async fn snapshot(&self) -> Result<Self::Snapshot, EnvironmentError> {
        Ok(AgentWorkspaceSnapshot)
    }

    async fn restore(&mut self, snapshot: &Self::Snapshot) -> Result<(), EnvironmentError> {
        let _ = snapshot;
        Ok(())
    }

    async fn terminal_status(&self) -> Result<Option<String>, EnvironmentError> {
        Ok(None)
    }
}

fn agent_workspace_event_to_environment_event(
    event: AgentWorkspaceEvent,
) -> EnvironmentEvent<AgentWorkspaceAction, AgentWorkspaceOutcome> {
    match event {
        AgentWorkspaceEvent::ActionScheduled {
            actor,
            action_id,
            action,
        } => EnvironmentEvent::ActionScheduled {
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
        AgentWorkspaceActionRunStatus::ActionEnqueued {
            engine_action_id,
            skill_name,
            action_name,
        } => {
            ActionRunStatus::ActionEnqueued {
                engine_action_id,
                skill_name,
                action_name,
            }
        }
        AgentWorkspaceActionRunStatus::ActionClaimed {
            engine_action_id,
            skill_name,
            action_name,
            worker_id,
        } => ActionRunStatus::ActionClaimed {
            engine_action_id,
            skill_name,
            action_name,
            worker_id,
        },
        AgentWorkspaceActionRunStatus::ActionCompleted {
            engine_action_id,
            skill_name,
            action_name,
        } => {
            ActionRunStatus::ActionCompleted {
                engine_action_id,
                skill_name,
                action_name,
            }
        }
        AgentWorkspaceActionRunStatus::ActionFailed {
            engine_action_id,
            skill_name,
            action_name,
            message,
        } => ActionRunStatus::ActionFailed {
            engine_action_id,
            skill_name,
            action_name,
            message,
        },
        AgentWorkspaceActionRunStatus::RunResumed => ActionRunStatus::RunResumed,
    }
}

fn filtered_skill_runtime(
    root: &Path,
    runtime: &SkillRuntime,
    allowed_catalog_entries: &[(String, String)],
) -> Result<SkillRuntime, AgentWorkspaceError> {
    let filtered_skills = runtime
        .catalog()
        .skills()
        .filter(|skill| {
            let profile_name = skill.metadata.profile_name.as_deref().unwrap_or_default();
            allowed_catalog_entries
                .iter()
                .any(|(skill_name, allowed_profile)| {
                    skill.metadata.skill_name == *skill_name && profile_name == allowed_profile
                })
        })
        .cloned()
        .collect::<Vec<CatalogSkill>>();
    let catalog =
        SkillCatalog::from_skills(filtered_skills).map_err(|error| AgentWorkspaceError::new(error.to_string()))?;
    SkillRuntime::new(root, catalog).map_err(|error| AgentWorkspaceError::new(error.to_string()))
}

fn parse_code_language(language: &str) -> Result<CodeLanguage, AgentWorkspaceError> {
    match language {
        "python" => Ok(CodeLanguage::Python),
        other => Err(AgentWorkspaceError::new(format!(
            "unsupported execute_code language '{other}'"
        ))),
    }
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
    let source = tokio::fs::read_to_string(&instructions_path)
        .await
        .map_err(|error| AgentWorkspaceError::new(error.to_string()))?;
    Ok(strip_markdown_frontmatter(&source).to_owned())
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

fn strip_markdown_frontmatter(source: &str) -> &str {
    let Some(rest) = source.strip_prefix("---\n").or_else(|| source.strip_prefix("---\r\n")) else {
        return source;
    };

    if let Some(index) = rest.find("\n---\n") {
        return rest[index + 5..].trim_start_matches(['\n', '\r']);
    }
    if let Some(index) = rest.find("\n---\r\n") {
        return rest[index + 6..].trim_start_matches(['\n', '\r']);
    }

    source
}
