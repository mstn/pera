mod artifacts;

use std::{env, path::PathBuf, sync::Arc};

use async_trait::async_trait;
use clap::{Args, Subcommand};
use pera_agents::{
    LlmAgentParticipant, LlmProvider, LlmRequest, OpenAiConfig as OpenAiProviderConfig,
    OpenAiProvider, PromptMessage, ProviderBackedPromptBuilder,
};
use pera_evals::{
    EvalActionAdapter, EvalEngine, EvalJudge, EvalJudgeRequest, EvalJudgeResultPayload,
    EvalMode as EngineEvalMode, EvalRequest, EvalRunner, EvalSpec, OverrideSet,
    ScriptedUserParticipant, SerializedAction, SerializedOutcome, parse_judge_verdict,
};
use pera_orchestrator::Participant;
use pera_runtime::{AgentWorkspace, WorkspaceAction, WorkspaceObservation, WorkspaceOutcome};
use serde_yaml::{Mapping, Value};

use self::artifacts::{RunArtifacts, create_run_artifacts, write_run_failed, write_run_result};
use crate::error::CliError;
use crate::repl::prompt_debug::FilePromptDebugSink;

struct OpenAiEvalJudge {
    api_key: String,
    default_model: String,
}

#[async_trait]
impl EvalJudge for OpenAiEvalJudge {
    async fn evaluate(
        &self,
        requests: Vec<EvalJudgeRequest>,
    ) -> Vec<pera_evals::EvalJudgeResult> {
        let mut results = Vec::new();
        for request in requests {
            let model_name = request
                .model
                .clone()
                .unwrap_or_else(|| self.default_model.clone());
            let provider = match OpenAiProvider::new(OpenAiProviderConfig {
                api_key: self.api_key.clone(),
                model: model_name.clone(),
            }) {
                Ok(provider) => provider,
                Err(error) => {
                    results.push(pera_evals::EvalJudgeResult {
                        criterion_index: request.criterion_index,
                        model: Some(model_name),
                        passed: false,
                        score: Some(0.0),
                        summary: format!("llm_judge setup failed: {error}"),
                        rubric: request.rubric,
                        response: String::new(),
                    });
                    continue;
                }
            };
            let llm_request = LlmRequest {
                system_prompt: request.system_prompt.clone(),
                messages: vec![PromptMessage {
                    role: "user".to_owned(),
                    content: request.user_message.clone(),
                    metadata: None,
                }],
                tools: vec![],
            };
            match provider.complete(llm_request).await {
                Ok(response) => {
                    let content = response.content.trim().to_owned();
                    match parse_judge_verdict(&content) {
                        Ok(EvalJudgeResultPayload {
                            passed,
                            score,
                            reason,
                        }) => results.push(pera_evals::EvalJudgeResult {
                            criterion_index: request.criterion_index,
                            model: Some(model_name),
                            passed,
                            score,
                            summary: reason,
                            rubric: request.rubric,
                            response: content,
                        }),
                        Err(error) => results.push(pera_evals::EvalJudgeResult {
                            criterion_index: request.criterion_index,
                            model: Some(model_name),
                            passed: false,
                            score: Some(0.0),
                            summary: format!("llm_judge returned invalid JSON: {error}"),
                            rubric: request.rubric,
                            response: content,
                        }),
                    }
                }
                Err(error) => results.push(pera_evals::EvalJudgeResult {
                    criterion_index: request.criterion_index,
                    model: Some(model_name),
                    passed: false,
                    score: Some(0.0),
                    summary: format!("llm_judge request failed: {error}"),
                    rubric: request.rubric,
                    response: String::new(),
                }),
            }
        }
        results
    }
}

#[derive(Debug, Args)]
pub struct EvalCommand {
    #[command(subcommand)]
    command: EvalSubcommand,
}

impl EvalCommand {
    pub async fn execute(&self) -> Result<(), CliError> {
        match &self.command {
            EvalSubcommand::Run(command) => command.execute(EvalMode::Run).await,
            EvalSubcommand::Optimize(command) => command.execute(EvalMode::Optimize).await,
        }
    }
}

#[derive(Debug, Subcommand)]
enum EvalSubcommand {
    Run(EvalModeCommand),
    Optimize(EvalModeCommand),
}

#[derive(Debug, Clone, Copy)]
enum EvalMode {
    Run,
    Optimize,
}

impl EvalMode {
    fn as_str(self) -> &'static str {
        match self {
            Self::Run => "run",
            Self::Optimize => "optimize",
        }
    }
}

#[derive(Debug, Args)]
struct EvalModeCommand {
    pub spec: PathBuf,
    #[arg(long)]
    pub output_folder: Option<PathBuf>,
    #[arg(long)]
    pub name: Option<String>,
    #[arg(long = "set", value_name = "PATH=VALUE")]
    pub set_values: Vec<String>,
    #[arg(long = "set-json", value_name = "PATH=JSON")]
    pub set_json_values: Vec<String>,
}

impl EvalModeCommand {
    async fn execute(&self, mode: EvalMode) -> Result<(), CliError> {
        if matches!(mode, EvalMode::Optimize) {
            return Err(CliError::UnexpectedStateOwned(
                "eval optimize is not implemented yet".to_owned(),
            ));
        }
        let overrides = OverrideSet::from_cli(&self.set_values, &self.set_json_values)?;
        let engine = EvalEngine;
        let mut session = engine
            .resolve(
                mode.into(),
                EvalRequest {
                    spec_path: self.spec.clone(),
                    output_folder: self.output_folder.clone(),
                    overrides: overrides.clone(),
                },
            )
            .map_err(CliError::from)?;
        engine.prepare(&mut session).await.map_err(CliError::from)?;
        let output_root =
            resolved_output_folder(&session.loaded_spec.spec, self.output_folder.as_ref())?;
        let artifacts = create_run_artifacts(
            &output_root,
            self.name
                .as_deref()
                .unwrap_or(&session.loaded_spec.spec.id),
            mode.as_str(),
            &self.spec,
            &session.loaded_spec,
            &overrides,
        )?;
        let workspace_root = EvalRunner::new()
            .prepare_run_workspace(
                session.preparation.as_ref().ok_or(CliError::UnexpectedStateOwned(
                    "eval session must be prepared before execution".to_owned(),
                ))?,
                &artifacts.run_dir,
            )
            .map_err(CliError::from)?;
        let allowed_catalog_entries = session
            .preparation
            .as_ref()
            .ok_or(CliError::UnexpectedStateOwned(
                "eval session must be prepared before execution".to_owned(),
            ))?
            .skills
            .iter()
            .map(|skill| (skill.skill_name.clone(), skill.profile_name.clone()))
            .collect::<Vec<_>>();
        let mut environment = AgentWorkspace::from_root_with_catalog_entries(
            &workspace_root,
            Some(&allowed_catalog_entries),
        )
            .await
            .map_err(|error| CliError::UnexpectedStateOwned(error.to_string()))?;
        environment
            .warm_catalog_skills()
            .await
            .map_err(|error| CliError::UnexpectedStateOwned(error.to_string()))?;
        for skill_name in &session.loaded_spec.spec.runtime.active_skills {
            environment.activate_skill(skill_name.clone());
        }
        let openai_model = required_env_var("OPENAI_MODEL")?;
        let openai_api_key = required_env_var("OPENAI_API_KEY")?;
        let judge = OpenAiEvalJudge {
            api_key: openai_api_key.clone(),
            default_model: openai_model.clone(),
        };
        let agent = LlmAgentParticipant::with_debug_sink(
            OpenAiProvider::new(OpenAiProviderConfig {
                api_key: openai_api_key.clone(),
                model: openai_model.clone(),
            })
            .map_err(|error| CliError::UnexpectedStateOwned(error.to_string()))?,
            ProviderBackedPromptBuilder,
            Arc::new(FilePromptDebugSink::new(
                workspace_root.clone(),
                Some(openai_model.clone()),
            )),
        );
        let user = ScriptedUserParticipant::<
            WorkspaceObservation,
            WorkspaceAction,
            WorkspaceOutcome,
        >::from_spec(&session.loaded_spec.spec.scenario.user);
        let participants: Vec<
            Box<dyn Participant<Observation = WorkspaceObservation, Action = WorkspaceAction, Outcome = WorkspaceOutcome>>,
        > = vec![Box::new(user), Box::new(agent)];
        let result = engine
            .run_with(
                &session,
                artifacts.run_dir.clone(),
                environment,
                participants,
                WorkspaceEvalAdapter,
                Some(&judge),
            )
            .await
            .map_err(|error| {
                let _ = write_run_failed(&artifacts);
                CliError::from(error)
            })?;
        write_run_result(&artifacts, &result)?;

        print_summary(mode, &artifacts);
        println!("passed: {}", result.passed);
        if let Some(message) = &result.final_agent_message {
            println!("final_agent_message: {}", message);
        }
        Ok(())
    }
}

fn resolved_output_folder(
    spec: &EvalSpec,
    cli_output_folder: Option<&PathBuf>,
) -> Result<PathBuf, CliError> {
    if let Some(path) = cli_output_folder {
        return Ok(path.clone());
    }

    if spec.runtime.output_folder.as_os_str().is_empty() {
        return Err(CliError::InvalidArguments(
            "spec runtime.output_folder cannot be empty",
        ));
    }

    Ok(spec.runtime.output_folder.clone())
}

fn required_env_var(name: &'static str) -> Result<String, CliError> {
    env::var(name)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .ok_or(CliError::InvalidArguments(match name {
            "OPENAI_API_KEY" => "OPENAI_API_KEY is required for eval run",
            "OPENAI_MODEL" => "OPENAI_MODEL is required for eval run",
            _ => "required environment variable is missing",
        }))
}

impl From<EvalMode> for EngineEvalMode {
    fn from(value: EvalMode) -> Self {
        match value {
            EvalMode::Run => Self::Run,
            EvalMode::Optimize => Self::Optimize,
        }
    }
}

fn print_summary(mode: EvalMode, artifacts: &RunArtifacts) {
    println!("mode: {}", mode.as_str());
    println!("run_dir: {}", artifacts.run_dir.display());
    println!("resolved_spec: {}", artifacts.resolved_spec_path.display());
    println!("manifest: {}", artifacts.manifest_path.display());
}

#[derive(Debug, Clone, Copy)]
struct WorkspaceEvalAdapter;

impl EvalActionAdapter<WorkspaceAction, WorkspaceOutcome> for WorkspaceEvalAdapter {
    fn serialize_action(&self, action: &WorkspaceAction) -> SerializedAction {
        match action {
            WorkspaceAction::LoadSkill { skill_name } => SerializedAction {
                name: "load_skill".to_owned(),
                arguments: Some(mapping([(
                    "skill_name",
                    Value::String(skill_name.clone()),
                )])),
            },
            WorkspaceAction::UnloadSkill { skill_name } => SerializedAction {
                name: "unload_skill".to_owned(),
                arguments: Some(mapping([(
                    "skill_name",
                    Value::String(skill_name.clone()),
                )])),
            },
            WorkspaceAction::ExecuteCode { language, source } => SerializedAction {
                name: "execute_code".to_owned(),
                arguments: Some(mapping([
                    ("language", Value::String(language.clone())),
                    ("source", Value::String(source.clone())),
                ])),
            },
        }
    }

    fn serialize_outcome(&self, outcome: &WorkspaceOutcome) -> SerializedOutcome {
        match outcome {
            WorkspaceOutcome::SkillLoaded { skill_name } => SerializedOutcome {
                name: "skill_loaded".to_owned(),
                payload: Some(mapping([(
                    "skill_name",
                    Value::String(skill_name.clone()),
                )])),
            },
            WorkspaceOutcome::SkillUnloaded { skill_name } => SerializedOutcome {
                name: "skill_unloaded".to_owned(),
                payload: Some(mapping([(
                    "skill_name",
                    Value::String(skill_name.clone()),
                )])),
            },
            WorkspaceOutcome::CodeExecuted { language, result } => SerializedOutcome {
                name: "code_executed".to_owned(),
                payload: Some({
                    let mut payload = Mapping::new();
                    payload.insert(
                        Value::String("language".to_owned()),
                        Value::String(language.clone()),
                    );
                    if let Some(result) = result {
                        payload.insert(
                            Value::String("result".to_owned()),
                            serde_yaml::to_value(result).unwrap_or(Value::Null),
                        );
                    }
                    Value::Mapping(payload)
                }),
            },
        }
    }
}

fn mapping(entries: impl IntoIterator<Item = (&'static str, Value)>) -> Value {
    let mut map = Mapping::new();
    for (key, value) in entries {
        map.insert(Value::String(key.to_owned()), value);
    }
    Value::Mapping(map)
}
