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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmToolDefinition {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
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

pub type LlmTextStream = Pin<Box<dyn Stream<Item = Result<String, ParticipantError>> + Send>>;

#[async_trait]
pub trait LlmProvider: Send + Sync {
    async fn stream(&self, request: LlmRequest) -> Result<LlmTextStream, ParticipantError>;

    async fn complete(&self, request: LlmRequest) -> Result<LlmResponse, ParticipantError> {
        let mut stream = self.stream(request).await?;
        let mut content = String::new();
        while let Some(chunk) = stream.next().await {
            content.push_str(&chunk?);
        }
        Ok(LlmResponse { content })
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct UnconfiguredLlmProvider;

#[async_trait]
impl LlmProvider for UnconfiguredLlmProvider {
    async fn stream(&self, _request: LlmRequest) -> Result<LlmTextStream, ParticipantError> {
        let chunk = "LLM provider is not configured yet.".to_owned();
        Ok(Box::pin(futures_util::stream::once(async move { Ok(chunk) })))
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

        output.message_start(&ParticipantId::Agent).await?;
        let mut stream = self.provider.stream(request).await?;
        let mut content = String::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            content.push_str(&chunk);
            output.message_delta(&ParticipantId::Agent, &chunk).await?;
        }
        if content.trim().is_empty() {
            let fallback = "[empty response]";
            content.push_str(fallback);
            output
                .message_delta(&ParticipantId::Agent, fallback)
                .await?;
        }
        output.message_end(&ParticipantId::Agent).await?;

        Ok(ParticipantDecision::FinalMessage { content })
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
