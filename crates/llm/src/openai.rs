use std::pin::Pin;

use anyhow::{Context, Result, anyhow, bail};
use async_stream::try_stream;
use async_trait::async_trait;
use futures_util::{Stream, StreamExt};
use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue};
use serde::Serialize;
use serde_json::Value;

use crate::{
    LlmProvider, LlmRequest, LlmStream, LlmStreamEvent, LlmToolCall, LlmToolDefinition,
    PromptMessage, PromptMessageMetadata,
};

const RESPONSES_API_URL: &str = "https://api.openai.com/v1/responses";

type OpenAiEventStream = Pin<Box<dyn Stream<Item = Result<OpenAiResponseEvent>> + Send>>;

#[derive(Debug, Clone)]
pub struct OpenAiConfig {
    pub api_key: String,
    pub model: String,
}

pub struct OpenAiProvider {
    client: OpenAiClient,
}

impl OpenAiProvider {
    pub fn new(config: OpenAiConfig) -> Result<Self> {
        Ok(Self {
            client: OpenAiClient::new(&config)?,
        })
    }
}

#[async_trait]
impl LlmProvider for OpenAiProvider {
    async fn stream(&self, request: LlmRequest) -> Result<LlmStream> {
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

        let stream = self.client.stream_messages(&messages, &tools).await?;

        Ok(Box::pin(stream.map(|event| event.map(event_from_openai))))
    }
}

struct OpenAiClient {
    http: reqwest::Client,
    model: String,
}

impl OpenAiClient {
    fn new(config: &OpenAiConfig) -> Result<Self> {
        let mut headers = HeaderMap::new();
        let bearer = format!("Bearer {}", config.api_key);
        let auth_value = HeaderValue::from_str(&bearer)
            .context("invalid OpenAI API key for Authorization header")?;

        headers.insert(AUTHORIZATION, auth_value);
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(ACCEPT, HeaderValue::from_static("text/event-stream"));

        let http = reqwest::Client::builder()
            .default_headers(headers)
            .build()
            .context("failed to build HTTP client")?;

        Ok(Self {
            http,
            model: config.model.clone(),
        })
    }

    async fn stream_messages<T>(&self, messages: &[Message], tools: &[T]) -> Result<OpenAiEventStream>
    where
        T: Serialize,
    {
        let response = self
            .http
            .post(RESPONSES_API_URL)
            .json(&ResponsesRequest {
                model: self.model.clone(),
                input: messages.iter().cloned().map(ApiInputItem::from).collect(),
                stream: true,
                tools,
            })
            .send()
            .await
            .context("failed to send request to OpenAI")?;

        let status = response.status();
        if !status.is_success() {
            let body = response
                .text()
                .await
                .unwrap_or_else(|error| format!("<failed to read response body: {error}>"));
            bail!("OpenAI returned HTTP {status}: {}", summarize_error_body(&body));
        }

        let stream = try_stream! {
            let mut response_stream = response.bytes_stream();
            let mut buffer = String::new();

            while let Some(chunk) = response_stream.next().await {
                let chunk = chunk.context("failed to read streaming response chunk")?;
                let chunk = std::str::from_utf8(&chunk).context("stream chunk was not valid UTF-8")?;
                buffer.push_str(chunk);

                while let Some(block_end) = buffer.find("\n\n") {
                    let block = buffer[..block_end].to_owned();
                    buffer.drain(..block_end + 2);

                    if let Some(event) = parse_sse_block(&block)? {
                        yield event;
                    }
                }
            }

            if !buffer.trim().is_empty() {
                if let Some(event) = parse_sse_block(buffer.trim())? {
                    yield event;
                }
            }
        };

        Ok(Box::pin(stream))
    }
}

fn summarize_error_body(body: &str) -> String {
    let body = body.trim();
    if body.is_empty() {
        return "<empty response body>".to_owned();
    }

    const MAX_LEN: usize = 2000;
    if body.len() <= MAX_LEN {
        body.to_owned()
    } else {
        format!("{}...", &body[..MAX_LEN])
    }
}

#[derive(Debug, Clone)]
enum Message {
    Text {
        role: MessageRole,
        content: String,
    },
    FunctionCall {
        call_id: String,
        name: String,
        arguments: Value,
    },
    FunctionCallOutput {
        call_id: String,
        output: String,
    },
}

impl Message {
    fn system(content: impl Into<String>) -> Self {
        Self::Text {
            role: MessageRole::System,
            content: content.into(),
        }
    }

    fn developer(content: impl Into<String>) -> Self {
        Self::Text {
            role: MessageRole::Developer,
            content: content.into(),
        }
    }

    fn user(content: impl Into<String>) -> Self {
        Self::Text {
            role: MessageRole::User,
            content: content.into(),
        }
    }

    fn assistant(content: impl Into<String>) -> Self {
        Self::Text {
            role: MessageRole::Assistant,
            content: content.into(),
        }
    }

    fn function_call(
        call_id: impl Into<String>,
        name: impl Into<String>,
        arguments: Value,
    ) -> Self {
        Self::FunctionCall {
            call_id: call_id.into(),
            name: name.into(),
            arguments,
        }
    }

    fn function_call_output(call_id: impl Into<String>, output: impl Into<String>) -> Self {
        Self::FunctionCallOutput {
            call_id: call_id.into(),
            output: output.into(),
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum MessageRole {
    System,
    Developer,
    User,
    Assistant,
}

impl MessageRole {
    fn as_str(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Developer => "developer",
            Self::User => "user",
            Self::Assistant => "assistant",
        }
    }

    fn content_type(self) -> &'static str {
        match self {
            Self::Assistant => "output_text",
            Self::System | Self::Developer | Self::User => "input_text",
        }
    }
}

#[derive(Serialize)]
struct ResponsesRequest<'a, T> {
    model: String,
    input: Vec<ApiInputItem>,
    stream: bool,
    #[serde(skip_serializing_if = "<[_]>::is_empty")]
    tools: &'a [T],
}

#[derive(Serialize)]
#[serde(tag = "type")]
enum ApiInputItem {
    #[serde(rename = "message")]
    Message {
        role: &'static str,
        content: Vec<ApiContentPart>,
    },
    #[serde(rename = "function_call")]
    FunctionCall {
        call_id: String,
        name: String,
        arguments: String,
    },
    #[serde(rename = "function_call_output")]
    FunctionCallOutput {
        call_id: String,
        output: String,
    },
}

impl From<Message> for ApiInputItem {
    fn from(message: Message) -> Self {
        match message {
            Message::Text { role, content } => {
                let content_type = role.content_type();
                Self::Message {
                    role: role.as_str(),
                    content: vec![ApiContentPart {
                        content_type,
                        text: content,
                    }],
                }
            }
            Message::FunctionCall {
                call_id,
                name,
                arguments,
            } => Self::FunctionCall {
                call_id,
                name,
                arguments: arguments.to_string(),
            },
            Message::FunctionCallOutput { call_id, output } => Self::FunctionCallOutput {
                call_id,
                output,
            },
        }
    }
}

#[derive(Serialize)]
struct ApiContentPart {
    #[serde(rename = "type")]
    content_type: &'static str,
    text: String,
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
    match message.metadata {
        Some(PromptMessageMetadata::ToolCall {
            call_id,
            name,
            arguments,
        }) => Message::function_call(call_id, name, arguments),
        Some(PromptMessageMetadata::ToolResult {
            call_id, output, ..
        }) => Message::function_call_output(call_id, output.to_string()),
        None => match message.role.as_str() {
            "system" => Message::system(message.content),
            "developer" => Message::developer(message.content),
            "assistant" => Message::assistant(message.content),
            _ => Message::user(message.content),
        },
    }
}

fn event_from_openai(event: OpenAiResponseEvent) -> LlmStreamEvent {
    match event {
        OpenAiResponseEvent::Text(text) => LlmStreamEvent::Text(text),
        OpenAiResponseEvent::ToolCallStart { call_id, name } => {
            LlmStreamEvent::ToolCallStart { call_id, name }
        }
        OpenAiResponseEvent::ToolCallDelta {
            call_id,
            name,
            arguments_delta,
        } => LlmStreamEvent::ToolCallDelta {
            call_id,
            name,
            arguments_delta,
        },
        OpenAiResponseEvent::ToolCall(call) => LlmStreamEvent::ToolCall(LlmToolCall {
            call_id: call.call_id,
            name: call.name,
            arguments: call.arguments,
        }),
    }
}

#[derive(Debug, Clone)]
enum OpenAiResponseEvent {
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
    ToolCall(OpenAiToolCall),
}

#[derive(Debug, Clone)]
struct OpenAiToolCall {
    call_id: Option<String>,
    name: String,
    arguments: Value,
}

fn parse_sse_block(block: &str) -> Result<Option<OpenAiResponseEvent>> {
    let mut event_name = None;
    let mut data_lines = Vec::new();

    for line in block.lines() {
        if let Some(value) = line.strip_prefix("event: ") {
            event_name = Some(value.trim().to_owned());
        } else if let Some(value) = line.strip_prefix("data: ") {
            data_lines.push(value);
        }
    }

    let Some(event_name) = event_name else {
        return Ok(None);
    };
    if event_name == "done" {
        return Ok(None);
    }

    let data = data_lines.join("\n");
    if data.trim().is_empty() {
        return Ok(None);
    }

    let payload: Value = serde_json::from_str(&data)
        .with_context(|| format!("failed to parse OpenAI SSE payload for event '{event_name}'"))?;

    match event_name.as_str() {
        "response.output_text.delta" => Ok(payload
            .get("delta")
            .and_then(Value::as_str)
            .map(|delta| OpenAiResponseEvent::Text(delta.to_owned()))),
        "response.function_call_arguments.delta" => {
            let call_id = payload
                .get("item_id")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("missing item_id in function call delta event"))?
                .to_owned();
            let name = payload
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("missing name in function call delta event"))?
                .to_owned();
            let arguments_delta = payload
                .get("delta")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("missing delta in function call delta event"))?
                .to_owned();
            Ok(Some(OpenAiResponseEvent::ToolCallDelta {
                call_id,
                name,
                arguments_delta,
            }))
        }
        "response.output_item.added" => {
            let item = payload
                .get("item")
                .ok_or_else(|| anyhow!("missing item in output item added event"))?;
            if item.get("type").and_then(Value::as_str) != Some("function_call") {
                return Ok(None);
            }
            let call_id = item
                .get("id")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("missing function call id in output item added event"))?
                .to_owned();
            let name = item
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("missing function call name in output item added event"))?
                .to_owned();
            Ok(Some(OpenAiResponseEvent::ToolCallStart { call_id, name }))
        }
        "response.output_item.done" => {
            let item = payload
                .get("item")
                .ok_or_else(|| anyhow!("missing item in output item done event"))?;
            if item.get("type").and_then(Value::as_str) != Some("function_call") {
                return Ok(None);
            }
            let name = item
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("missing function call name in output item done event"))?
                .to_owned();
            let arguments = item
                .get("arguments")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("missing arguments in function call done event"))?;
            let arguments = serde_json::from_str(arguments)
                .context("failed to parse function call arguments as JSON")?;
            Ok(Some(OpenAiResponseEvent::ToolCall(OpenAiToolCall {
                call_id: item.get("call_id").and_then(Value::as_str).map(ToOwned::to_owned),
                name,
                arguments,
            })))
        }
        _ => Ok(None),
    }
}
