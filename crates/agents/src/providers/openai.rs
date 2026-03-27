#![allow(dead_code)]

use std::pin::Pin;

use anyhow::{Context, Result, anyhow, bail};
use async_stream::try_stream;
use futures_util::{Stream, StreamExt};
use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue};
use serde::Serialize;
use serde_json::Value;

const RESPONSES_API_URL: &str = "https://api.openai.com/v1/responses";

pub type TextStream = Pin<Box<dyn Stream<Item = Result<String>> + Send>>;

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

    pub async fn stream_messages<T>(&self, messages: &[Message], tools: &[T]) -> Result<TextStream>
    where
        T: Serialize,
    {
        let response = self
            .http
            .post(RESPONSES_API_URL)
            .json(&ResponsesRequest {
                model: self.model.clone(),
                input: messages.iter().cloned().map(ApiMessage::from).collect(),
                stream: true,
                tools,
            })
            .send()
            .await
            .context("failed to send request to OpenAI")?
            .error_for_status()
            .context("OpenAI returned an error status")?;

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

                    if let Some(delta) = parse_sse_block(&block)? {
                        yield delta;
                    }
                }
            }

            if !buffer.trim().is_empty() {
                if let Some(delta) = parse_sse_block(buffer.trim())? {
                    yield delta;
                }
            }
        };

        Ok(Box::pin(stream))
    }
}

#[derive(Debug, Clone)]
pub struct Message {
    pub role: MessageRole,
    pub content: String,
}

#[allow(dead_code)]
impl Message {
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: MessageRole::User,
            content: content.into(),
        }
    }

    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: MessageRole::System,
            content: content.into(),
        }
    }

    pub fn developer(content: impl Into<String>) -> Self {
        Self {
            role: MessageRole::Developer,
            content: content.into(),
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: MessageRole::Assistant,
            content: content.into(),
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
    input: Vec<ApiMessage>,
    stream: bool,
    #[serde(skip_serializing_if = "<[_]>::is_empty")]
    tools: &'a [T],
}

#[derive(Serialize)]
struct ApiMessage {
    role: &'static str,
    content: Vec<ApiContentPart>,
}

impl From<Message> for ApiMessage {
    fn from(message: Message) -> Self {
        let content_type = message.role.content_type();
        Self {
            role: message.role.as_str(),
            content: vec![ApiContentPart {
                content_type,
                text: message.content,
            }],
        }
    }
}

#[derive(Serialize)]
struct ApiContentPart {
    #[serde(rename = "type")]
    content_type: &'static str,
    text: String,
}

fn parse_sse_block(block: &str) -> Result<Option<String>> {
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
            .map(ToOwned::to_owned)),
        "response.completed" => Ok(None),
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
