use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use futures_util::StreamExt;
use futures_util::stream::Stream;
use pera_core::{RunId, WorkItemId};
use pera_orchestrator::{
    Participant, ParticipantDecision,
    ParticipantError, ParticipantId, ParticipantInput, ParticipantOutput,
};
use pera_runtime::{WorkspaceAction, WorkspaceObservation, WorkspaceOutcome};
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptDebugResponseRecord {
    pub status: PromptDebugResponseStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call: Option<PromptDebugToolCall>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<PromptDebugErrorRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PromptDebugResponseStatus {
    Ok,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptDebugToolCall {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub call_id: Option<String>,
    pub name: String,
    pub arguments: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptDebugErrorRecord {
    pub user_message: String,
    pub debug_message: String,
}

pub trait PromptDebugSink: Send + Sync {
    fn record_prompt(
        &self,
        metadata: &PromptDebugMetadata,
        request: &LlmRequest,
    ) -> Result<(), ParticipantError>;

    fn record_response(
        &self,
        _metadata: &PromptDebugMetadata,
        _response: &PromptDebugResponseRecord,
    ) -> Result<(), ParticipantError> {
        Ok(())
    }
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
    type Observation = WorkspaceObservation;
    type Action = WorkspaceAction;
    type Outcome = WorkspaceOutcome;

    fn id(&self) -> ParticipantId {
        ParticipantId::Agent
    }

    async fn respond(
        &mut self,
        input: ParticipantInput<Self::Observation, Self::Action, Self::Outcome>,
        output: &mut dyn ParticipantOutput<Self::Action, Self::Outcome>,
    ) -> Result<ParticipantDecision<Self::Action>, ParticipantError> {
        let context = self.prompt_builder.build_context(&input);
        let request = LlmRequest {
            system_prompt: self.prompt_builder.build_system_prompt(&context),
            messages: prompt_messages(&context),
            tools: context.tools.clone(),
        };
        let debug_metadata = PromptDebugMetadata {
            run_id: input.run_id,
            agent_loop_id: input.agent_loop_id,
            agent_loop_iteration: input.agent_loop_iteration,
            participant: input.participant.clone(),
            task_id: input.task.id.clone(),
        };
        self.debug_sink
            .record_prompt(&debug_metadata, &request)?;

        output.message_start(&ParticipantId::Agent).await?;
        let mut stream = match self.provider.stream(request).await {
            Ok(stream) => stream,
            Err(error) => {
                self.debug_sink.record_response(
                    &debug_metadata,
                    &PromptDebugResponseRecord {
                        status: PromptDebugResponseStatus::Error,
                        content: None,
                        tool_call: None,
                        error: Some(PromptDebugErrorRecord {
                            user_message: "The model request failed before the agent could act."
                                .to_owned(),
                            debug_message: error.to_string(),
                        }),
                    },
                )?;
                return Err(error);
            }
        };
        let mut content = String::new();
        let mut tool_call = None;
        let started_message = true;
        let mut pending_tool_name = None;
        while let Some(chunk) = stream.next().await {
            match chunk {
                Err(error) => {
                    self.debug_sink.record_response(
                        &debug_metadata,
                        &PromptDebugResponseRecord {
                            status: PromptDebugResponseStatus::Error,
                            content: (!content.is_empty()).then(|| content.clone()),
                            tool_call: tool_call.as_ref().map(prompt_debug_tool_call),
                            error: Some(PromptDebugErrorRecord {
                                user_message:
                                    "The model response failed while the agent was waiting for it."
                                        .to_owned(),
                                debug_message: error.to_string(),
                            }),
                        },
                    )?;
                    return Err(error);
                }
                Ok(chunk) => match chunk {
                LlmStreamEvent::Text(chunk) => {
                    content.push_str(&chunk);
                    output.message_delta(&ParticipantId::Agent, &chunk).await?;
                }
                LlmStreamEvent::ToolCallStart { name, .. } => {
                    pending_tool_name = Some(name.clone());
                    output
                        .status_update(&ParticipantId::Agent, &status_for_tool_start(&name))
                        .await?;
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
                    if let Some(status) = status_for_tool_delta(&name, &arguments_delta) {
                        output
                            .status_update(&ParticipantId::Agent, &status)
                            .await?;
                    }
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
                },
            }
        }
        if content.trim().is_empty() && tool_call.is_none() {
            let fallback = "[empty response]";
            content.push_str(fallback);
            output
                .message_delta(&ParticipantId::Agent, fallback)
                .await?;
        }
        if started_message {
            output.message_end(&ParticipantId::Agent).await?;
        }

        self.debug_sink.record_response(
            &debug_metadata,
            &PromptDebugResponseRecord {
                status: PromptDebugResponseStatus::Ok,
                content: Some(content.clone()),
                tool_call: tool_call.as_ref().map(prompt_debug_tool_call),
                error: None,
            },
        )?;

        if let Some(tool_call) = tool_call {
            if tool_call.name == "execute_code" {
                let handoff = required_string_argument(&tool_call.arguments, "handoff_user_message")?;
                output.message_start(&ParticipantId::Agent).await?;
                output.message_delta(&ParticipantId::Agent, &handoff).await?;
                output.message_end(&ParticipantId::Agent).await?;
            }
            let decision = map_tool_call_to_decision(tool_call)?;
            if !matches!(
                decision,
                ParticipantDecision::Action {
                    action: WorkspaceAction::ExecuteCode { .. },
                    ..
                }
            ) {
                output
                    .status_update(
                        &ParticipantId::Agent,
                        &status_for_action_decision(&decision),
                    )
                    .await?;
            }
            return Ok(decision);
        }

        Ok(ParticipantDecision::FinalMessage { content })
    }
}

fn prompt_debug_tool_call(call: &LlmToolCall) -> PromptDebugToolCall {
    PromptDebugToolCall {
        call_id: call.call_id.clone(),
        name: call.name.clone(),
        arguments: call.arguments.clone(),
    }
}

fn map_tool_call_to_decision(
    tool_call: LlmToolCall,
) -> Result<ParticipantDecision<WorkspaceAction>, ParticipantError> {
    match tool_call.name.as_str() {
        "load_skill" => {
            let skill_name = required_string_argument(&tool_call.arguments, "skill_name")?;
            Ok(ParticipantDecision::Action {
                action: WorkspaceAction::LoadSkill { skill_name },
                execution: pera_orchestrator::ActionExecution::Immediate,
            })
        }
        "unload_skill" => {
            let skill_name = required_string_argument(&tool_call.arguments, "skill_name")?;
            Ok(ParticipantDecision::Action {
                action: WorkspaceAction::UnloadSkill { skill_name },
                execution: pera_orchestrator::ActionExecution::Immediate,
            })
        }
        "execute_code" => {
            let language = required_string_argument(&tool_call.arguments, "language")?;
            let source = required_string_argument(&tool_call.arguments, "source")?;
            Ok(ParticipantDecision::Action {
                action: WorkspaceAction::ExecuteCode { language, source },
                execution: pera_orchestrator::ActionExecution::DeferredBlocking,
            })
        }
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

fn status_for_tool_start(tool_name: &str) -> String {
    match tool_name {
        "load_skill" => "preparing skill load".to_owned(),
        "unload_skill" => "preparing skill unload".to_owned(),
        "execute_code" => "preparing code execution".to_owned(),
        _ => format!("preparing tool call: {tool_name}"),
    }
}

fn status_for_tool_delta(tool_name: &str, arguments_delta: &str) -> Option<String> {
    let skill_name = extract_skill_name_hint(arguments_delta)?;
    match tool_name {
        "load_skill" => Some(format!("loading skill {skill_name}")),
        "unload_skill" => Some(format!("unloading skill {skill_name}")),
        _ => None,
    }
}

fn status_for_action_decision(decision: &ParticipantDecision<WorkspaceAction>) -> String {
    match decision {
        ParticipantDecision::Action {
            action: WorkspaceAction::LoadSkill { skill_name },
            ..
        } => format!("loading skill {skill_name}"),
        ParticipantDecision::Action {
            action: WorkspaceAction::UnloadSkill { skill_name },
            ..
        } => format!("unloading skill {skill_name}"),
        ParticipantDecision::Action {
            action: WorkspaceAction::ExecuteCode { .. },
            ..
        } => "executing code".to_owned(),
        ParticipantDecision::Message { .. }
        | ParticipantDecision::FinalMessage { .. }
        | ParticipantDecision::Yield
        | ParticipantDecision::Finish => "updating".to_owned(),
    }
}

fn extract_skill_name_hint(arguments_delta: &str) -> Option<String> {
    let key = "\"skill_name\"";
    let start = arguments_delta.find(key)?;
    let after_key = &arguments_delta[start + key.len()..];
    let first_quote = after_key.find('"')?;
    let after_first_quote = &after_key[first_quote + 1..];
    let second_quote = after_first_quote.find('"')?;
    Some(after_first_quote[..second_quote].to_owned())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{LlmToolCall, map_tool_call_to_decision};
    use pera_orchestrator::ParticipantDecision;
    use pera_runtime::WorkspaceAction;

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
                action: WorkspaceAction::LoadSkill {
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
                action: WorkspaceAction::UnloadSkill {
                    skill_name: "secret-service".to_owned(),
                },
                execution: pera_orchestrator::ActionExecution::Immediate,
            }
        );
    }

    #[test]
    fn maps_execute_code_tool_call_to_action() {
        let decision = map_tool_call_to_decision(LlmToolCall {
            call_id: None,
            name: "execute_code".to_owned(),
            arguments: json!({
                "language": "python",
                "source": "print(1)",
                "handoff_user_message": "Running a quick check."
            }),
        })
        .unwrap();

        assert_eq!(
            decision,
            ParticipantDecision::Action {
                action: WorkspaceAction::ExecuteCode {
                    language: "python".to_owned(),
                    source: "print(1)".to_owned(),
                },
                execution: pera_orchestrator::ActionExecution::DeferredBlocking,
            }
        );
    }
}

fn prompt_messages(context: &PromptContext) -> Vec<PromptMessage> {
    let builder = ProviderBackedPromptBuilder;
    let mut messages = Vec::new();
    if let Some(task_message) = builder.build_user_task_message(context) {
        messages.push(task_message);
    }
    messages.extend(context.transcript.clone());
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
