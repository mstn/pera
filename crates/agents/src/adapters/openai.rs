use async_trait::async_trait;
use futures_util::StreamExt;

use crate::llm::{LlmProvider, LlmRequest, LlmResponse};
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
    async fn complete(
        &self,
        request: LlmRequest,
    ) -> Result<LlmResponse, pera_orchestrator::ParticipantError> {
        let mut messages = Vec::with_capacity(request.messages.len() + 1);
        messages.push(Message::system(request.system_prompt));
        messages.extend(request.messages.into_iter().map(message_from_prompt));

        let mut stream = self
            .client
            .stream_messages(&messages)
            .await
            .map_err(to_participant_error)?;

        let mut content = String::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(to_participant_error)?;
            content.push_str(&chunk);
        }

        Ok(LlmResponse { content })
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
