use async_trait::async_trait;
use pera_orchestrator::{
    CodeAction, CodeObservation, CodeOutcome, Participant, ParticipantDecision, ParticipantError,
    ParticipantId, ParticipantInput, ParticipantOutput,
};

use crate::prompt::{CodePromptBuilder, PromptContext, PromptMessage, ProviderBackedPromptBuilder};

#[derive(Debug, Clone)]
pub struct LlmRequest {
    pub system_prompt: String,
    pub messages: Vec<PromptMessage>,
}

#[derive(Debug, Clone)]
pub struct LlmResponse {
    pub content: String,
}

#[async_trait]
pub trait LlmProvider: Send + Sync {
    async fn complete(&self, request: LlmRequest) -> Result<LlmResponse, ParticipantError>;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct UnconfiguredLlmProvider;

#[async_trait]
impl LlmProvider for UnconfiguredLlmProvider {
    async fn complete(&self, _request: LlmRequest) -> Result<LlmResponse, ParticipantError> {
        Ok(LlmResponse {
            content: "LLM provider is not configured yet.".to_owned(),
        })
    }
}

pub struct LlmAgentParticipant<P = UnconfiguredLlmProvider, B = ProviderBackedPromptBuilder> {
    provider: P,
    prompt_builder: B,
}

impl LlmAgentParticipant<UnconfiguredLlmProvider, ProviderBackedPromptBuilder> {
    pub fn unconfigured() -> Self {
        Self {
            provider: UnconfiguredLlmProvider,
            prompt_builder: ProviderBackedPromptBuilder,
        }
    }
}

impl<P, B> LlmAgentParticipant<P, B> {
    pub fn new(provider: P, prompt_builder: B) -> Self {
        Self {
            provider,
            prompt_builder,
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
        };
        let response = self.provider.complete(request).await?;

        output.message_start(&ParticipantId::Agent).await?;
        for ch in response.content.chars() {
            let mut chunk = String::new();
            chunk.push(ch);
            output.message_delta(&ParticipantId::Agent, &chunk).await?;
        }
        output.message_end(&ParticipantId::Agent).await?;

        Ok(ParticipantDecision::FinalMessage {
            content: response.content,
        })
    }
}

fn prompt_messages(context: &PromptContext) -> Vec<PromptMessage> {
    let mut messages = Vec::new();
    messages.extend(context.transcript.iter().cloned());
    messages.extend(context.inbox.iter().cloned());
    messages
}
