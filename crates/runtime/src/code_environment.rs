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
use pera_core::{
    ActionId, ActionRequest, ActionSkillRef, CanonicalInvocation, CanonicalValue, RunId,
    SkillManifest,
};
use tokio::task::JoinHandle;
use tracing::debug;

use crate::{ActionExecutionUpdate, ActionExecutor, SkillRuntime, WasmtimeComponentActionExecutor};
use crate::code_tools::default_code_environment_tools;
use crate::CodeEnvironmentTool;

fn catalog_skills_root(runtime_root: &std::path::Path) -> PathBuf {
    runtime_root.join("catalog").join("skills")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeEnvironmentObservation {
    pub available_tools: Vec<CodeEnvironmentTool>,
    pub available_skills: Vec<CodeEnvironmentAvailableSkill>,
    pub active_skills: Vec<CodeEnvironmentActiveSkill>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeEnvironmentAvailableSkill {
    pub skill_name: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeEnvironmentActiveSkill {
    pub skill_name: String,
    pub instructions: String,
    pub python_stub: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodeEnvironmentAction {
    CallTool {
        skill: ActionSkillRef,
        invocation: CanonicalInvocation,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodeEnvironmentOutcome {
    ToolCall {
        skill: ActionSkillRef,
        invocation: CanonicalInvocation,
        value: CanonicalValue,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodeEnvironmentEvent {
    ActionAccepted {
        actor: String,
        action_id: ActionId,
        action: CodeEnvironmentAction,
    },
    ActionCompleted {
        actor: String,
        action_id: ActionId,
        outcome: CodeEnvironmentOutcome,
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
pub struct SubmittedCodeAction {
    pub action_id: ActionId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeEnvironmentSnapshot;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeEnvironmentError {
    message: String,
}

impl CodeEnvironmentError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl Display for CodeEnvironmentError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl Error for CodeEnvironmentError {}

#[async_trait]
pub trait CodeToolExecutor: Send + Sync {
    async fn execute_tool(
        &self,
        request: ActionRequest,
    ) -> Result<CanonicalValue, CodeEnvironmentError>;
}

#[async_trait]
impl CodeToolExecutor for WasmtimeComponentActionExecutor {
    async fn execute_tool(
        &self,
        request: ActionRequest,
    ) -> Result<CanonicalValue, CodeEnvironmentError> {
        match self.execute(request).await {
            ActionExecutionUpdate::Completed(result) => Ok(result.value),
            ActionExecutionUpdate::Failed { message, .. } => {
                Err(CodeEnvironmentError::new(message))
            }
            ActionExecutionUpdate::Claimed { .. } => Err(CodeEnvironmentError::new(
                "unexpected claimed update returned by tool executor",
            )),
        }
    }
}

struct PendingCodeAction {
    actor: String,
    handle: JoinHandle<Result<CodeEnvironmentOutcome, CodeEnvironmentError>>,
}

pub struct CodeEnvironment {
    skill_runtime: Option<Arc<SkillRuntime>>,
    tool_executor: Option<Arc<dyn CodeToolExecutor>>,
    pending_actions: BTreeMap<ActionId, PendingCodeAction>,
    active_skill_names: BTreeSet<String>,
}

impl std::fmt::Debug for CodeEnvironment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CodeEnvironment")
            .field("has_skill_runtime", &self.skill_runtime.is_some())
            .field("has_tool_executor", &self.tool_executor.is_some())
            .field("pending_actions", &self.pending_actions.len())
            .finish()
    }
}

impl CodeEnvironment {
    pub fn new(
        _workspace_root: impl Into<PathBuf>,
        skill_runtime: Option<SkillRuntime>,
    ) -> Result<Self, CodeEnvironmentError> {
        let tool_executor = skill_runtime
            .clone()
            .map(WasmtimeComponentActionExecutor::new)
            .transpose()
            .map_err(|error| CodeEnvironmentError::new(error.to_string()))?
            .map(|executor| Arc::new(executor) as Arc<dyn CodeToolExecutor>);

        Ok(Self {
            skill_runtime: skill_runtime.map(Arc::new),
            tool_executor,
            pending_actions: BTreeMap::new(),
            active_skill_names: BTreeSet::new(),
        })
    }

    pub fn with_tool_executor(
        _workspace_root: impl Into<PathBuf>,
        skill_runtime: Option<SkillRuntime>,
        tool_executor: Arc<dyn CodeToolExecutor>,
    ) -> Self {
        Self {
            skill_runtime: skill_runtime.map(Arc::new),
            tool_executor: Some(tool_executor),
            pending_actions: BTreeMap::new(),
            active_skill_names: BTreeSet::new(),
        }
    }

    pub async fn reset(&mut self) -> Result<CodeEnvironmentObservation, CodeEnvironmentError> {
        self.pending_actions.clear();
        self.observe().await
    }

    pub fn activate_skill(&mut self, skill_name: impl Into<String>) {
        self.active_skill_names.insert(skill_name.into());
    }

    pub fn deactivate_skill(&mut self, skill_name: &str) {
        self.active_skill_names.remove(skill_name);
    }

    pub async fn observe(&self) -> Result<CodeEnvironmentObservation, CodeEnvironmentError> {
        let (available_skills, active_skills) = match &self.skill_runtime {
            Some(runtime) => {
                let mut available_skills = Vec::new();
                let mut active_skills = Vec::new();

                for catalog_skill in runtime.catalog().skills() {
                    let skill_name = catalog_skill.metadata.skill_name.clone();
                    if self.active_skill_names.contains(&skill_name) {
                        let instructions = active_skill_instructions(runtime.root(), catalog_skill)
                            .await?;
                        active_skills.push(CodeEnvironmentActiveSkill {
                            skill_name,
                            instructions,
                            python_stub: render_python_stubs(&catalog_skill.world),
                        });
                    } else {
                        let description = available_skill_description(runtime.root(), catalog_skill);
                        available_skills.push(CodeEnvironmentAvailableSkill {
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

        Ok(CodeEnvironmentObservation {
            available_tools: default_code_environment_tools(),
            available_skills,
            active_skills,
        })
    }

    pub async fn snapshot(&self) -> Result<CodeEnvironmentSnapshot, CodeEnvironmentError> {
        Ok(CodeEnvironmentSnapshot)
    }

    pub async fn restore(
        &mut self,
        _snapshot: &CodeEnvironmentSnapshot,
    ) -> Result<(), CodeEnvironmentError> {
        Ok(())
    }

    pub async fn step(
        &mut self,
        action: CodeEnvironmentAction,
    ) -> Result<CodeEnvironmentOutcome, CodeEnvironmentError> {
        run_action(self.tool_executor.clone(), action).await
    }

    pub async fn submit(
        &mut self,
        actor: String,
        action: CodeEnvironmentAction,
    ) -> Result<SubmittedCodeAction, CodeEnvironmentError> {
        let action_id = ActionId::generate();
        let tool_executor = self.tool_executor.clone();
        let handle = tokio::spawn(async move { run_action(tool_executor, action).await });
        self.pending_actions.insert(
            action_id,
            PendingCodeAction {
                actor,
                handle,
            },
        );

        Ok(SubmittedCodeAction { action_id })
    }

    pub async fn poll_events(&mut self) -> Result<Vec<CodeEnvironmentEvent>, CodeEnvironmentError> {
        let ready_action_ids = self
            .pending_actions
            .iter()
            .filter_map(|(action_id, pending)| pending.handle.is_finished().then_some(*action_id))
            .collect::<Vec<_>>();
        let mut events = Vec::new();

        for action_id in ready_action_ids {
            let pending = self.pending_actions.remove(&action_id).ok_or_else(|| {
                CodeEnvironmentError::new(format!("missing pending action '{action_id}'"))
            })?;
            let actor = pending.actor;
            match pending.handle.await {
                Ok(Ok(outcome)) => {
                    events.push(CodeEnvironmentEvent::ActionCompleted {
                        actor,
                        action_id,
                        outcome,
                    });
                }
                Ok(Err(error)) => {
                    events.push(CodeEnvironmentEvent::ActionFailed {
                        actor,
                        action_id,
                        error: error.to_string(),
                    });
                }
                Err(error) => {
                    events.push(CodeEnvironmentEvent::ActionFailed {
                        actor,
                        action_id,
                        error: error.to_string(),
                    });
                }
            }
        }

        Ok(events)
    }
}

async fn run_action(
    tool_executor: Option<Arc<dyn CodeToolExecutor>>,
    action: CodeEnvironmentAction,
) -> Result<CodeEnvironmentOutcome, CodeEnvironmentError> {
    match action {
        CodeEnvironmentAction::CallTool { skill, invocation } => {
            call_tool(tool_executor, skill, invocation).await
        }
    }
}

async fn call_tool(
    tool_executor: Option<Arc<dyn CodeToolExecutor>>,
    skill: ActionSkillRef,
    invocation: CanonicalInvocation,
) -> Result<CodeEnvironmentOutcome, CodeEnvironmentError> {
    let tool_executor = tool_executor
        .as_ref()
        .ok_or_else(|| CodeEnvironmentError::new("no tool executor is configured"))?;

    let value = tool_executor
        .execute_tool(ActionRequest {
            id: ActionId::generate(),
            run_id: RunId::generate(),
            skill: skill.clone(),
            invocation: invocation.clone(),
        })
        .await?;

    Ok(CodeEnvironmentOutcome::ToolCall {
        skill,
        invocation,
        value,
    })
}

async fn active_skill_instructions(
    runtime_root: &Path,
    catalog_skill: &CatalogSkill,
) -> Result<String, CodeEnvironmentError> {
    let (profile_dir, manifest) = compiled_catalog_profile(runtime_root, catalog_skill)?;
    let Some(instructions) = manifest.defaults.instructions.as_ref() else {
        return Ok(String::new());
    };
    let instructions_path = profile_dir.join(&instructions.source);
    tokio::fs::read_to_string(&instructions_path)
        .await
        .map_err(|error| CodeEnvironmentError::new(error.to_string()))
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
) -> Result<(PathBuf, SkillManifest), CodeEnvironmentError> {
    let skill_version = catalog_skill
        .metadata
        .skill_version
        .as_deref()
        .ok_or_else(|| CodeEnvironmentError::new("catalog skill is missing skill_version"))?;
    let profile_name = catalog_skill
        .metadata
        .profile_name
        .as_deref()
        .ok_or_else(|| CodeEnvironmentError::new("catalog skill is missing profile_name"))?;
    let profile_dir = catalog_skills_root(runtime_root)
        .join(&catalog_skill.metadata.skill_name)
        .join(skill_version)
        .join(profile_name);
    let manifest_path = resolve_manifest_path(&profile_dir)?;
    let manifest_source = std::fs::read_to_string(&manifest_path)
        .map_err(|error| CodeEnvironmentError::new(error.to_string()))?;
    let manifest = serde_yaml::from_str(&manifest_source)
        .map_err(|error| CodeEnvironmentError::new(error.to_string()))?;
    Ok((profile_dir, manifest))
}

fn resolve_manifest_path(profile_dir: &Path) -> Result<PathBuf, CodeEnvironmentError> {
    for candidate in ["manifest.yaml", "skill.yaml", "skill.yml"] {
        let path = profile_dir.join(candidate);
        if path.exists() {
            return Ok(path);
        }
    }

    Err(CodeEnvironmentError::new(format!(
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
