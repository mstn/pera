use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;

use chrono::Local;
use pera_agents::{PromptDebugMetadata, PromptDebugResponseRecord, PromptDebugSink};
use pera_core::{RunId, WorkItemId};
use pera_evals::{
    SimulatedUserDebugMetadata, SimulatedUserDebugResponseRecord, SimulatedUserDebugSink,
};
use pera_llm::{LlmRequest, PromptMessageMetadata};
use pera_orchestrator::ParticipantId;
use pera_runtime::FileSystemLayout;
use serde::Serialize;

use crate::error::CliError;

pub struct FilePromptDebugSink {
    layout: FileSystemLayout,
    model: Option<String>,
    run_directories: Mutex<BTreeMap<RunId, PathBuf>>,
    loop_directories: Mutex<BTreeMap<(RunId, WorkItemId, String), PathBuf>>,
}

impl FilePromptDebugSink {
    pub fn new(root: PathBuf, model: Option<String>) -> Self {
        let layout = FileSystemLayout::new(root)
            .expect("prompt debug layout initialization must succeed");
        Self {
            layout,
            model,
            run_directories: Mutex::new(BTreeMap::new()),
            loop_directories: Mutex::new(BTreeMap::new()),
        }
    }

    fn resolve_run_directory(&self, run_id: RunId) -> Result<PathBuf, CliError> {
        let mut run_directories = self
            .run_directories
            .lock()
            .map_err(|_| CliError::UnexpectedStateOwned("prompt debug lock poisoned".to_owned()))?;
        if let Some(path) = run_directories.get(&run_id) {
            return Ok(path.clone());
        }

        let path = self
            .layout
            .orchestration_runs_dir()
            .join(format!("{}-{}", timestamp_prefix(), run_id.as_hyphenated()));
        std::fs::create_dir_all(&path).map_err(|source| CliError::CreateDir {
            path: path.clone(),
            source,
        })?;
        run_directories.insert(run_id, path.clone());
        Ok(path)
    }

    fn resolve_loop_directory(
        &self,
        run_id: RunId,
        loop_id: WorkItemId,
        participant: &ParticipantId,
    ) -> Result<PathBuf, CliError> {
        let participant_suffix = participant_suffix(participant);
        let key = (run_id, loop_id, participant_suffix.to_owned());
        let mut loop_directories = self
            .loop_directories
            .lock()
            .map_err(|_| CliError::UnexpectedStateOwned("prompt debug lock poisoned".to_owned()))?;
        if let Some(path) = loop_directories.get(&key) {
            return Ok(path.clone());
        }

        let run_directory = self.resolve_run_directory(run_id)?;
        let path = run_directory.join(format!(
            "{}-{}.{}",
            timestamp_prefix(),
            loop_id.as_hyphenated(),
            participant_suffix
        ));
        std::fs::create_dir_all(&path).map_err(|source| CliError::CreateDir {
            path: path.clone(),
            source,
        })?;
        loop_directories.insert(key, path.clone());
        Ok(path)
    }

    fn record_prompt_file(
        &self,
        run_id: RunId,
        loop_id: WorkItemId,
        participant: &ParticipantId,
        iteration: usize,
        request: &LlmRequest,
    ) -> Result<(), pera_orchestrator::ParticipantError> {
        let loop_directory = self
            .resolve_loop_directory(run_id, loop_id, participant)
            .map_err(|error| pera_orchestrator::ParticipantError::new(error.to_string()))?;
        let iteration_id = format!("{:04}", iteration);
        let request_yaml_path = loop_directory.join(format!("{iteration_id}.request.yaml"));
        let tools_path = loop_directory.join(format!("{iteration_id}.tools.json"));

        std::fs::write(
            &request_yaml_path,
            render_request_yaml(self.model.as_deref(), request).map_err(|error| {
                pera_orchestrator::ParticipantError::new(
                    CliError::UnexpectedStateOwned(error.to_string()).to_string(),
                )
            })?,
        )
        .map_err(|source| {
            pera_orchestrator::ParticipantError::new(
                CliError::WriteFile {
                    path: request_yaml_path,
                    source,
                }
                .to_string(),
            )
        })?;

        let tools_json = serde_json::to_vec_pretty(&request.tools).map_err(|error| {
            pera_orchestrator::ParticipantError::new(
                CliError::UnexpectedStateOwned(error.to_string()).to_string(),
            )
        })?;
        std::fs::write(&tools_path, tools_json).map_err(|source| {
            pera_orchestrator::ParticipantError::new(
                CliError::WriteFile {
                    path: tools_path,
                    source,
                }
                .to_string(),
            )
        })?;

        Ok(())
    }

    fn record_response_file<T: Serialize>(
        &self,
        run_id: RunId,
        loop_id: WorkItemId,
        participant: &ParticipantId,
        iteration: usize,
        response: &T,
    ) -> Result<(), pera_orchestrator::ParticipantError> {
        let loop_directory = self
            .resolve_loop_directory(run_id, loop_id, participant)
            .map_err(|error| pera_orchestrator::ParticipantError::new(error.to_string()))?;
        let iteration_id = format!("{:04}", iteration);
        let response_path = loop_directory.join(format!("{iteration_id}.response.yaml"));
        let response_yaml = serde_yaml::to_string(response).map_err(|error| {
            pera_orchestrator::ParticipantError::new(
                CliError::UnexpectedStateOwned(error.to_string()).to_string(),
            )
        })?;
        std::fs::write(&response_path, response_yaml).map_err(|source| {
            pera_orchestrator::ParticipantError::new(
                CliError::WriteFile {
                    path: response_path,
                    source,
                }
                .to_string(),
            )
        })?;

        Ok(())
    }
}

impl PromptDebugSink for FilePromptDebugSink {
    fn record_prompt(
        &self,
        metadata: &PromptDebugMetadata,
        request: &LlmRequest,
    ) -> Result<(), pera_orchestrator::ParticipantError> {
        self.record_prompt_file(
            metadata.run_id,
            metadata.agent_loop_id,
            &metadata.participant,
            metadata.agent_loop_iteration,
            request,
        )
    }

    fn record_response(
        &self,
        metadata: &PromptDebugMetadata,
        response: &PromptDebugResponseRecord,
    ) -> Result<(), pera_orchestrator::ParticipantError> {
        self.record_response_file(
            metadata.run_id,
            metadata.agent_loop_id,
            &metadata.participant,
            metadata.agent_loop_iteration,
            response,
        )
    }
}

impl SimulatedUserDebugSink for FilePromptDebugSink {
    fn record_prompt(
        &self,
        metadata: &SimulatedUserDebugMetadata,
        request: &LlmRequest,
    ) -> Result<(), pera_orchestrator::ParticipantError> {
        self.record_prompt_file(
            metadata.run_id,
            metadata.agent_loop_id,
            &metadata.participant,
            metadata.agent_loop_iteration,
            request,
        )
    }

    fn record_response(
        &self,
        metadata: &SimulatedUserDebugMetadata,
        response: &SimulatedUserDebugResponseRecord,
    ) -> Result<(), pera_orchestrator::ParticipantError> {
        self.record_response_file(
            metadata.run_id,
            metadata.agent_loop_id,
            &metadata.participant,
            metadata.agent_loop_iteration,
            response,
        )
    }
}

fn participant_suffix(participant: &ParticipantId) -> &str {
    match participant {
        ParticipantId::Agent => "agent",
        ParticipantId::User => "user",
        ParticipantId::Custom(name) => name.as_str(),
    }
}

#[derive(Serialize)]
struct RequestYaml<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    model: Option<&'a str>,
    stream: bool,
    input: Vec<RequestYamlInputItem>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<RequestYamlTool<'a>>,
}

#[derive(Serialize)]
#[serde(tag = "type")]
enum RequestYamlInputItem {
    #[serde(rename = "message")]
    Message {
        role: &'static str,
        content: Vec<RequestYamlContentPart>,
    },
    #[serde(rename = "function_call")]
    FunctionCall {
        call_id: String,
        name: String,
        arguments: serde_yaml::Value,
    },
    #[serde(rename = "function_call_output")]
    FunctionCallOutput {
        call_id: String,
        output: serde_yaml::Value,
    },
}

#[derive(Serialize)]
struct RequestYamlContentPart {
    #[serde(rename = "type")]
    content_type: &'static str,
    text: String,
}

#[derive(Serialize)]
struct RequestYamlTool<'a> {
    #[serde(rename = "type")]
    tool_type: &'static str,
    name: &'a str,
    description: &'a str,
    parameters: serde_yaml::Value,
}

fn render_request_yaml(
    model: Option<&str>,
    request: &LlmRequest,
) -> Result<String, serde_yaml::Error> {
    let mut input = Vec::with_capacity(request.messages.len() + 1);
    input.push(RequestYamlInputItem::Message {
        role: "system",
        content: vec![RequestYamlContentPart {
            content_type: "input_text",
            text: request.system_prompt.clone(),
        }],
    });

    for message in &request.messages {
        match &message.metadata {
            Some(PromptMessageMetadata::ToolCall {
                call_id,
                name,
                arguments,
            }) => input.push(RequestYamlInputItem::FunctionCall {
                call_id: call_id.clone(),
                name: name.clone(),
                arguments: json_to_yaml(arguments.clone()),
            }),
            Some(PromptMessageMetadata::ToolResult {
                call_id,
                output,
                ..
            }) => input.push(RequestYamlInputItem::FunctionCallOutput {
                call_id: call_id.clone(),
                output: json_to_yaml(output.clone()),
            }),
            None => input.push(RequestYamlInputItem::Message {
                role: role_to_provider_role(&message.role),
                content: vec![RequestYamlContentPart {
                    content_type: content_type_for_role(&message.role),
                    text: message.content.clone(),
                }],
            }),
        }
    }

    let tools = request
        .tools
        .iter()
        .map(|tool| RequestYamlTool {
            tool_type: "function",
            name: &tool.name,
            description: &tool.description,
            parameters: json_to_yaml(tool.input_schema.clone()),
        })
        .collect::<Vec<_>>();

    serde_yaml::to_string(&RequestYaml {
        model,
        stream: true,
        input,
        tools,
    })
}

fn json_to_yaml(value: serde_json::Value) -> serde_yaml::Value {
    serde_yaml::to_value(value).unwrap_or(serde_yaml::Value::Null)
}

fn role_to_provider_role(role: &str) -> &'static str {
    match role {
        "system" => "system",
        "developer" => "developer",
        "assistant" => "assistant",
        _ => "user",
    }
}

fn content_type_for_role(role: &str) -> &'static str {
    match role {
        "assistant" => "output_text",
        _ => "input_text",
    }
}

fn timestamp_prefix() -> String {
    Local::now().format("%Y%m%d-%H%M%S").to_string()
}
