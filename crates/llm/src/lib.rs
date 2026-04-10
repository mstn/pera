mod openai;

use std::pin::Pin;

use anyhow::Result;
use async_trait::async_trait;
use futures_util::StreamExt;
use futures_util::stream::Stream;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use openai::{OpenAiConfig, OpenAiProvider};

#[derive(Debug, Clone)]
pub struct LlmRequest {
    pub system_prompt: String,
    pub messages: Vec<PromptMessage>,
    pub tools: Vec<LlmToolDefinition>,
}

#[derive(Debug, Clone)]
pub struct LlmResponse {
    pub content: String,
    pub tool_call: Option<LlmToolCall>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmToolDefinition {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LlmToolCall {
    pub call_id: Option<String>,
    pub name: String,
    pub arguments: Value,
}

#[derive(Debug, Clone, PartialEq)]
pub enum LlmStreamEvent {
    Text(String),
    ToolCallStart {
        call_id: String,
        name: String,
    },
    ToolCallDelta {
        call_id: String,
        name: String,
        arguments_delta: String,
    },
    ToolCall(LlmToolCall),
}

pub type LlmStream = Pin<Box<dyn Stream<Item = Result<LlmStreamEvent>> + Send>>;

#[async_trait]
pub trait LlmProvider: Send + Sync {
    async fn stream(&self, request: LlmRequest) -> Result<LlmStream>;

    async fn complete(&self, request: LlmRequest) -> Result<LlmResponse> {
        let mut stream = self.stream(request).await?;
        let mut content = String::new();
        let mut tool_call = None;
        while let Some(chunk) = stream.next().await {
            match chunk? {
                LlmStreamEvent::Text(text) => content.push_str(&text),
                LlmStreamEvent::ToolCallStart { .. } | LlmStreamEvent::ToolCallDelta { .. } => {}
                LlmStreamEvent::ToolCall(call) => {
                    if tool_call.is_none() {
                        tool_call = Some(call);
                    }
                }
            }
        }
        Ok(LlmResponse { content, tool_call })
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct UnconfiguredLlmProvider;

#[async_trait]
impl LlmProvider for UnconfiguredLlmProvider {
    async fn stream(&self, _request: LlmRequest) -> Result<LlmStream> {
        let chunk = "LLM provider is not configured yet.".to_owned();
        Ok(Box::pin(futures_util::stream::once(async move {
            Ok(LlmStreamEvent::Text(chunk))
        })))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct PromptMessage {
    pub role: String,
    pub content: String,
    pub metadata: Option<PromptMessageMetadata>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PromptMessageMetadata {
    ToolCall {
        call_id: String,
        name: String,
        arguments: serde_json::Value,
    },
    ToolResult {
        call_id: String,
        name: String,
        output: serde_json::Value,
    },
}

impl PromptMessage {
    pub fn text(role: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: role.into(),
            content: content.into(),
            metadata: None,
        }
    }

    pub fn tool_call(
        call_id: impl Into<String>,
        name: impl Into<String>,
        arguments: serde_json::Value,
    ) -> Self {
        Self {
            role: "assistant".to_owned(),
            content: String::new(),
            metadata: Some(PromptMessageMetadata::ToolCall {
                call_id: call_id.into(),
                name: name.into(),
                arguments,
            }),
        }
    }

    pub fn tool_result(
        call_id: impl Into<String>,
        name: impl Into<String>,
        output: serde_json::Value,
    ) -> Self {
        Self {
            role: "tool".to_owned(),
            content: String::new(),
            metadata: Some(PromptMessageMetadata::ToolResult {
                call_id: call_id.into(),
                name: name.into(),
                output,
            }),
        }
    }
}
