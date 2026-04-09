#![allow(dead_code)]

use std::pin::Pin;

use anyhow::{Context, Result, anyhow, bail};
use async_stream::try_stream;
use futures_util::{Stream, StreamExt};
use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue};
use serde::Serialize;
use serde_json::Value;

const RESPONSES_API_URL: &str = "https://api.openai.com/v1/responses";

pub type OpenAiEventStream = Pin<Box<dyn Stream<Item = Result<OpenAiResponseEvent>> + Send>>;

#[derive(Debug, Clone)]
pub struct OpenAiConfig {
    pub api_key: String,
    pub model: String,
}

pub struct OpenAiClient {
    http: reqwest::Client,
    model: String,
}

impl OpenAiClient {
    pub fn new(config: &OpenAiConfig) -> Result<Self> {
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

    pub async fn stream_messages<T>(
        &self,
        messages: &[Message],
        tools: &[T],
    ) -> Result<OpenAiEventStream>
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
pub enum Message {
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

#[allow(dead_code)]
impl Message {
    pub fn user(content: impl Into<String>) -> Self {
        Self::Text {
            role: MessageRole::User,
            content: content.into(),
        }
    }

    pub fn system(content: impl Into<String>) -> Self {
        Self::Text {
            role: MessageRole::System,
            content: content.into(),
        }
    }

    pub fn developer(content: impl Into<String>) -> Self {
        Self::Text {
            role: MessageRole::Developer,
            content: content.into(),
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self::Text {
            role: MessageRole::Assistant,
            content: content.into(),
        }
    }

    pub fn function_call(
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

    pub fn function_call_output(call_id: impl Into<String>, output: impl Into<String>) -> Self {
        Self::FunctionCallOutput {
            call_id: call_id.into(),
            output: output.into(),
        }
    }
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy)]
pub enum MessageRole {
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
            Message::FunctionCallOutput { call_id, output } => {
                Self::FunctionCallOutput { call_id, output }
            }
        }
    }
}

#[derive(Serialize)]
struct ApiContentPart {
    #[serde(rename = "type")]
    content_type: &'static str,
    text: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum OpenAiResponseEvent {
    Text(String),
    ToolCallStart { call_id: String, name: String },
    ToolCallDelta {
        call_id: String,
        name: String,
        arguments_delta: String,
    },
    ToolCall(OpenAiToolCall),
}

#[derive(Debug, Clone, PartialEq)]
pub struct OpenAiToolCall {
    pub call_id: Option<String>,
    pub name: String,
    pub arguments: Value,
}

fn parse_sse_block(block: &str) -> Result<Option<OpenAiResponseEvent>> {
    let mut data_lines = Vec::new();

    for line in block.lines() {
        if let Some(rest) = line.strip_prefix("data:") {
            data_lines.push(rest.trim_start());
        }
    }

    if data_lines.is_empty() {
        return Ok(None);
    }

    let data = data_lines.join("\n");

    if data == "[DONE]" {
        return Ok(None);
    }

    let event: Value =
        serde_json::from_str(&data).context("failed to parse streaming event JSON")?;
    let event_type = event
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("streaming event missing type"))?;

    match event_type {
        "response.output_text.delta" => Ok(event
            .get("delta")
            .and_then(Value::as_str)
            .map(|delta| OpenAiResponseEvent::Text(delta.to_owned()))),
        "response.output_item.added" => parse_output_item_added(&event),
        "response.function_call_arguments.delta" => parse_function_call_arguments_delta(&event),
        "response.output_item.done" => parse_output_item_done(&event),
        "response.completed" => parse_response_completed(&event),
        "error" => {
            let message = event
            .get("error")
            .and_then(|error| error.get("message"))
                .and_then(Value::as_str)
                .unwrap_or("unknown streaming error");
            bail!(message.to_string())
        }
        _ => Ok(None),
    }
}

fn parse_output_item_added(event: &Value) -> Result<Option<OpenAiResponseEvent>> {
    let Some(item) = event.get("item") else {
        return Ok(None);
    };
    if item.get("type").and_then(Value::as_str) != Some("function_call") {
        return Ok(None);
    }
    let call_id = item
        .get("call_id")
        .or_else(|| item.get("id"))
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("function_call item missing call_id"))?;
    let name = item
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("function_call item missing name"))?;
    Ok(Some(OpenAiResponseEvent::ToolCallStart {
        call_id: call_id.to_owned(),
        name: name.to_owned(),
    }))
}

fn parse_function_call_arguments_delta(event: &Value) -> Result<Option<OpenAiResponseEvent>> {
    let delta = event
        .get("delta")
        .and_then(Value::as_str)
        .filter(|delta| !delta.is_empty());
    let Some(arguments_delta) = delta else {
        return Ok(None);
    };
    let call_id = event
        .get("call_id")
        .or_else(|| event.get("item_id"))
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("function_call_arguments.delta missing call_id"))?;
    let name = event
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("tool");
    Ok(Some(OpenAiResponseEvent::ToolCallDelta {
        call_id: call_id.to_owned(),
        name: name.to_owned(),
        arguments_delta: arguments_delta.to_owned(),
    }))
}

fn parse_output_item_done(event: &Value) -> Result<Option<OpenAiResponseEvent>> {
    let Some(item) = event.get("item") else {
        return Ok(None);
    };
    parse_function_call_item(item)
}

fn parse_response_completed(event: &Value) -> Result<Option<OpenAiResponseEvent>> {
    let Some(output) = event
        .get("response")
        .and_then(|response| response.get("output"))
        .and_then(Value::as_array)
    else {
        return Ok(None);
    };

    for item in output {
        if let Some(tool_call) = parse_function_call_item(item)? {
            return Ok(Some(tool_call));
        }
    }

    Ok(None)
}

fn parse_function_call_item(item: &Value) -> Result<Option<OpenAiResponseEvent>> {
    if item.get("type").and_then(Value::as_str) != Some("function_call") {
        return Ok(None);
    }
    let name = item
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("function_call item missing name"))?;
    let arguments = item
        .get("arguments")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("function_call item missing arguments"))?;
    let arguments = serde_json::from_str(arguments)
        .with_context(|| format!("failed to parse function_call arguments for '{name}'"))?;
    Ok(Some(OpenAiResponseEvent::ToolCall(OpenAiToolCall {
        call_id: item
            .get("call_id")
            .or_else(|| item.get("id"))
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        name: name.to_owned(),
        arguments,
    })))
}
