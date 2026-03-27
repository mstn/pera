use async_trait::async_trait;
use futures_util::StreamExt;
use serde::Serialize;

use crate::llm::{LlmProvider, LlmRequest, LlmTextStream, LlmToolDefinition};
use crate::prompt::PromptMessage;
use crate::providers::openai::{Message, OpenAiClient, OpenAiConfig};

pub struct OpenAiProvider {
    client: OpenAiClient,
}

impl OpenAiProvider {
    pub fn new(config: OpenAiConfig) -> anyhow::Result<Self> {
        Ok(Self {
            client: OpenAiClient::new(&config)?,
        })
    }
}

#[async_trait]
impl LlmProvider for OpenAiProvider {
    async fn stream(
        &self,
        request: LlmRequest,
    ) -> Result<LlmTextStream, pera_orchestrator::ParticipantError> {
        let LlmRequest {
            system_prompt,
            messages: prompt_messages,
            tools: prompt_tools,
        } = request;
        let mut messages = Vec::with_capacity(prompt_messages.len() + 1);
        messages.push(Message::system(system_prompt));
        messages.extend(prompt_messages.into_iter().map(message_from_prompt));
        let tools = prompt_tools
            .iter()
            .map(tool_from_definition)
            .collect::<Vec<_>>();

        let stream = self
            .client
            .stream_messages(&messages, &tools)
            .await
            .map_err(to_participant_error)?;

        Ok(Box::pin(
            stream.map(|chunk| chunk.map_err(to_participant_error)),
        ))
    }
}

#[derive(Debug, Clone, Serialize)]
struct OpenAiFunctionTool {
    #[serde(rename = "type")]
    tool_type: &'static str,
    name: String,
    description: String,
    parameters: serde_json::Value,
}

fn tool_from_definition(tool: &LlmToolDefinition) -> OpenAiFunctionTool {
    OpenAiFunctionTool {
        tool_type: "function",
        name: tool.name.clone(),
        description: tool.description.clone(),
        parameters: tool.input_schema.clone(),
    }
}

fn message_from_prompt(message: PromptMessage) -> Message {
    match message.role.as_str() {
        "system" => Message::system(message.content),
        "developer" => Message::developer(message.content),
        "assistant" => Message::assistant(message.content),
        _ => Message::user(message.content),
    }
}

fn to_participant_error(error: anyhow::Error) -> pera_orchestrator::ParticipantError {
    pera_orchestrator::ParticipantError::new(error.to_string())
}
