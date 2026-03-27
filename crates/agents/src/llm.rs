use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use futures_util::StreamExt;
use futures_util::stream::Stream;
use pera_core::{RunId, WorkItemId};
use pera_orchestrator::{
    CodeAction, CodeObservation, CodeOutcome, Participant, ParticipantDecision,
    ParticipantError, ParticipantId, ParticipantInput, ParticipantOutput,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::prompt::{CodePromptBuilder, PromptContext, PromptMessage, ProviderBackedPromptBuilder};

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

#[derive(Debug, Clone)]
pub struct PromptDebugMetadata {
    pub run_id: RunId,
    pub agent_loop_id: WorkItemId,
    pub agent_loop_iteration: usize,
    pub participant: ParticipantId,
    pub task_id: String,
}

pub trait PromptDebugSink: Send + Sync {
    fn record_prompt(
        &self,
        metadata: &PromptDebugMetadata,
        request: &LlmRequest,
    ) -> Result<(), ParticipantError>;
}

#[derive(Debug, Default)]
pub struct NoopPromptDebugSink;

impl PromptDebugSink for NoopPromptDebugSink {
    fn record_prompt(
        &self,
        _metadata: &PromptDebugMetadata,
        _request: &LlmRequest,
    ) -> Result<(), ParticipantError> {
        Ok(())
    }
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

pub type LlmStream = Pin<Box<dyn Stream<Item = Result<LlmStreamEvent, ParticipantError>> + Send>>;

#[async_trait]
pub trait LlmProvider: Send + Sync {
    async fn stream(&self, request: LlmRequest) -> Result<LlmStream, ParticipantError>;

    async fn complete(&self, request: LlmRequest) -> Result<LlmResponse, ParticipantError> {
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
    async fn stream(&self, _request: LlmRequest) -> Result<LlmStream, ParticipantError> {
        let chunk = "LLM provider is not configured yet.".to_owned();
        Ok(Box::pin(futures_util::stream::once(async move {
            Ok(LlmStreamEvent::Text(chunk))
        })))
    }
}

pub struct LlmAgentParticipant<P = UnconfiguredLlmProvider, B = ProviderBackedPromptBuilder> {
    provider: P,
    prompt_builder: B,
    debug_sink: Arc<dyn PromptDebugSink>,
}

impl LlmAgentParticipant<UnconfiguredLlmProvider, ProviderBackedPromptBuilder> {
    pub fn unconfigured() -> Self {
        Self {
            provider: UnconfiguredLlmProvider,
            prompt_builder: ProviderBackedPromptBuilder,
            debug_sink: Arc::new(NoopPromptDebugSink),
        }
    }
}

impl<P, B> LlmAgentParticipant<P, B> {
    pub fn new(provider: P, prompt_builder: B) -> Self {
        Self {
            provider,
            prompt_builder,
            debug_sink: Arc::new(NoopPromptDebugSink),
        }
    }

    pub fn with_debug_sink(
        provider: P,
        prompt_builder: B,
        debug_sink: Arc<dyn PromptDebugSink>,
    ) -> Self {
        Self {
            provider,
            prompt_builder,
            debug_sink,
        }
    }
}

#[async_trait]
impl<P, B> Participant for LlmAgentParticipant<P, B>
where
    P: LlmProvider + 'static,
    B: CodePromptBuilder + 'static,
{
    type Observation = CodeObservation;
    type Action = CodeAction;
    type Outcome = CodeOutcome;

    fn id(&self) -> ParticipantId {
        ParticipantId::Agent
    }

    async fn respond(
        &mut self,
        input: ParticipantInput<Self::Observation, Self::Action, Self::Outcome>,
        output: &mut dyn ParticipantOutput<Self::Action>,
    ) -> Result<ParticipantDecision<Self::Action>, ParticipantError> {
        let context = self.prompt_builder.build_context(&input);
        let request = LlmRequest {
            system_prompt: self.prompt_builder.build_system_prompt(&context),
            messages: prompt_messages(&context),
            tools: context.tools.clone(),
        };
        self.debug_sink.record_prompt(
            &PromptDebugMetadata {
                run_id: input.run_id,
                agent_loop_id: input.agent_loop_id,
                agent_loop_iteration: input.agent_loop_iteration,
                participant: input.participant.clone(),
                task_id: input.task.id.clone(),
            },
            &request,
        )?;

        let mut stream = self.provider.stream(request).await?;
        let mut content = String::new();
        let mut tool_call = None;
        let mut started_message = false;
        let mut pending_tool_name = None;
        while let Some(chunk) = stream.next().await {
            match chunk? {
                LlmStreamEvent::Text(chunk) => {
                    if !started_message {
                        output.message_start(&ParticipantId::Agent).await?;
                        started_message = true;
                    }
                    content.push_str(&chunk);
                    output.message_delta(&ParticipantId::Agent, &chunk).await?;
                }
                LlmStreamEvent::ToolCallStart { name, .. } => {
                    pending_tool_name = Some(name.clone());
                    output
                        .tool_call_start(&ParticipantId::Agent, &name)
                        .await?;
                }
                LlmStreamEvent::ToolCallDelta {
                    name,
                    arguments_delta,
                    ..
                } => {
                    pending_tool_name = Some(name.clone());
                    output
                        .tool_call_delta(&ParticipantId::Agent, &name, &arguments_delta)
                        .await?;
                }
                LlmStreamEvent::ToolCall(call) => {
                    if let Some(tool_name) = pending_tool_name.take().or_else(|| Some(call.name.clone()))
                    {
                        output
                            .tool_call_end(&ParticipantId::Agent, &tool_name)
                            .await?;
                    }
                    if tool_call.is_none() {
                        tool_call = Some(call);
                    }
                }
            }
        }
        if content.trim().is_empty() && tool_call.is_none() {
            if !started_message {
                output.message_start(&ParticipantId::Agent).await?;
                started_message = true;
            }
            let fallback = "[empty response]";
            content.push_str(fallback);
            output
                .message_delta(&ParticipantId::Agent, fallback)
                .await?;
        }
        if started_message {
            output.message_end(&ParticipantId::Agent).await?;
        }

        if let Some(tool_call) = tool_call {
            let decision = map_tool_call_to_decision(tool_call)?;
            output
                .action_planned(&ParticipantId::Agent, match &decision {
                    ParticipantDecision::Action { action, .. } => action,
                    _ => unreachable!(),
                })
                .await?;
            return Ok(decision);
        }

        Ok(ParticipantDecision::FinalMessage { content })
    }
}

fn map_tool_call_to_decision(
    tool_call: LlmToolCall,
) -> Result<ParticipantDecision<CodeAction>, ParticipantError> {
    match tool_call.name.as_str() {
        "load_skill" => {
            let skill_name = required_string_argument(&tool_call.arguments, "skill_name")?;
            Ok(ParticipantDecision::Action {
                action: CodeAction::LoadSkill { skill_name },
                execution: pera_orchestrator::ActionExecution::Immediate,
            })
        }
        "unload_skill" => {
            let skill_name = required_string_argument(&tool_call.arguments, "skill_name")?;
            Ok(ParticipantDecision::Action {
                action: CodeAction::UnloadSkill { skill_name },
                execution: pera_orchestrator::ActionExecution::Immediate,
            })
        }
        "execute_code" => Err(ParticipantError::new(
            "execute_code tool parsing is not implemented yet",
        )),
        other => Err(ParticipantError::new(format!(
            "unsupported tool call '{other}'"
        ))),
    }
}

fn required_string_argument(arguments: &Value, field: &str) -> Result<String, ParticipantError> {
    arguments
        .get(field)
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .ok_or_else(|| ParticipantError::new(format!("tool call is missing string field '{field}'")))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{LlmToolCall, map_tool_call_to_decision};
    use pera_orchestrator::{CodeAction, ParticipantDecision};

    #[test]
    fn maps_load_skill_tool_call_to_action() {
        let decision = map_tool_call_to_decision(LlmToolCall {
            call_id: None,
            name: "load_skill".to_owned(),
            arguments: json!({ "skill_name": "secret-service" }),
        })
        .unwrap();

        assert_eq!(
            decision,
            ParticipantDecision::Action {
                action: CodeAction::LoadSkill {
                    skill_name: "secret-service".to_owned(),
                },
                execution: pera_orchestrator::ActionExecution::Immediate,
            }
        );
    }

    #[test]
    fn maps_unload_skill_tool_call_to_action() {
        let decision = map_tool_call_to_decision(LlmToolCall {
            call_id: None,
            name: "unload_skill".to_owned(),
            arguments: json!({ "skill_name": "secret-service" }),
        })
        .unwrap();

        assert_eq!(
            decision,
            ParticipantDecision::Action {
                action: CodeAction::UnloadSkill {
                    skill_name: "secret-service".to_owned(),
                },
                execution: pera_orchestrator::ActionExecution::Immediate,
            }
        );
    }
}

fn prompt_messages(context: &PromptContext) -> Vec<PromptMessage> {
    let mut messages = context.transcript.clone();
    let overlap = suffix_prefix_overlap(&messages, &context.inbox);
    messages.extend(context.inbox.iter().skip(overlap).cloned());
    messages
}

fn suffix_prefix_overlap(
    transcript: &[PromptMessage],
    inbox: &[PromptMessage],
) -> usize {
    let max_overlap = transcript.len().min(inbox.len());
    for overlap in (1..=max_overlap).rev() {
        if transcript[transcript.len() - overlap..] == inbox[..overlap] {
            return overlap;
        }
    }
    0
}
