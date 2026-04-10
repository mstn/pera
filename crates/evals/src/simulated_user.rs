use std::marker::PhantomData;

use async_trait::async_trait;
use pera_llm::{LlmProvider, LlmRequest, PromptMessage};
use pera_orchestrator::{
    Participant, ParticipantDecision, ParticipantError, ParticipantId, ParticipantInboxEvent,
    ParticipantInput, ParticipantOutput, TrajectoryEvent,
};

use crate::spec::EvalUserSpec;

const SIMULATED_USER_SYSTEM_PROMPT: &str =
    include_str!("prompts/simulated_user_system.md");
const SIMULATED_USER_CONTEXT_PROMPT: &str =
    include_str!("prompts/simulated_user_context.md");

pub struct SimulatedUserParticipant<P, O, A, U> {
    provider: P,
    spec: EvalUserSpec,
    _marker: PhantomData<fn() -> (O, A, U)>,
}

impl<P, O, A, U> SimulatedUserParticipant<P, O, A, U> {
    pub fn new(provider: P, spec: EvalUserSpec) -> Self {
        Self {
            provider,
            spec,
            _marker: PhantomData,
        }
    }
}

#[async_trait]
impl<P, O, A, U> Participant for SimulatedUserParticipant<P, O, A, U>
where
    P: LlmProvider + Send + Sync + 'static,
    O: Clone + Send + Sync + 'static,
    A: Clone + Send + Sync + 'static,
    U: Clone + Send + Sync + 'static,
{
    type Observation = O;
    type Action = A;
    type Outcome = U;

    fn id(&self) -> ParticipantId {
        ParticipantId::User
    }

    async fn respond(
        &mut self,
        input: ParticipantInput<Self::Observation, Self::Action, Self::Outcome>,
        _output: &mut dyn ParticipantOutput<Self::Action, Self::Outcome>,
    ) -> Result<ParticipantDecision<Self::Action>, ParticipantError> {
        let user_already_spoke = has_user_message(&input);
        if user_already_spoke && !has_new_non_user_inbox_message(&input) {
            return Ok(ParticipantDecision::Yield);
        }

        let request = LlmRequest {
            system_prompt: SIMULATED_USER_SYSTEM_PROMPT.to_owned(),
            messages: build_simulated_user_messages(&self.spec, &input, user_already_spoke),
            tools: Vec::new(),
        };
        let response = self
            .provider
            .complete(request)
            .await
            .map_err(|error| ParticipantError::new(error.to_string()))?;
        let content = response.content.trim();

        if content.eq("FINISH") || content.is_empty() {
            return Ok(ParticipantDecision::Finish);
        }

        Ok(ParticipantDecision::CompleteLoop {
            content: content.to_owned(),
        })
    }
}

fn has_user_message<O, A, U>(input: &ParticipantInput<O, A, U>) -> bool {
    input.trajectory.events.iter().any(|event| {
        matches!(
            event,
            TrajectoryEvent::ParticipantMessage {
                participant: ParticipantId::User,
                ..
            }
        )
    }) || input.inbox.iter().any(|event| {
        matches!(
            event,
            ParticipantInboxEvent::Message {
                from: ParticipantId::User,
                ..
            }
        )
    })
}

fn has_new_non_user_inbox_message<O, A, U>(input: &ParticipantInput<O, A, U>) -> bool {
    input.inbox.iter().any(|event| {
        matches!(
            event,
            ParticipantInboxEvent::Message {
                from,
                ..
            } if *from != ParticipantId::User
        )
    })
}

fn build_simulated_user_messages<O, A, U>(
    spec: &EvalUserSpec,
    input: &ParticipantInput<O, A, U>,
    user_already_spoke: bool,
) -> Vec<PromptMessage> {
    let mut messages = vec![PromptMessage {
        role: "developer".to_owned(),
        content: simulated_user_context(spec, user_already_spoke),
        metadata: None,
    }];

    for event in &input.trajectory.events {
        if let TrajectoryEvent::ParticipantMessage {
            participant,
            content,
        } = event
        {
            messages.push(PromptMessage {
                role: role_for_participant(participant),
                content: content.clone(),
                metadata: None,
            });
        }
    }

    for event in &input.inbox {
        if let ParticipantInboxEvent::Message { from, content } = event {
            messages.push(PromptMessage {
                role: role_for_participant(from),
                content: content.clone(),
                metadata: None,
            });
        }
    }

    messages
}

fn simulated_user_context(spec: &EvalUserSpec, user_already_spoke: bool) -> String {
    let EvalUserSpec::Simulated {
        task,
        reason,
        known_info,
        unknown_info,
    } = spec
    else {
        panic!("simulated user participant requires a simulated user spec");
    };
    SIMULATED_USER_CONTEXT_PROMPT
        .replace("{{task}}", task.trim())
        .replace("{{reason}}", normalize_empty(reason.trim()))
        .replace("{{known_info}}", normalize_empty(known_info.trim()))
        .replace("{{unknown_info}}", normalize_empty(unknown_info.trim()))
        .replace(
            "{{conversation_stage}}",
            if user_already_spoke {
                "Continue the conversation as the user. Respond only if the assistant's latest message calls for a user reply."
            } else {
                "Write the first user message that starts the conversation."
            },
        )
}

fn normalize_empty(value: &str) -> &str {
    if value.is_empty() {
        "none"
    } else {
        value
    }
}

fn role_for_participant(participant: &ParticipantId) -> String {
    match participant {
        ParticipantId::Agent => "assistant".to_owned(),
        ParticipantId::User => "user".to_owned(),
        ParticipantId::Custom(name) => name.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::simulated_user_context;
    use crate::spec::EvalUserSpec;

    #[test]
    fn simulated_user_context_uses_all_simulated_fields() {
        let context = simulated_user_context(
            &EvalUserSpec::Simulated {
                task: "Book travel".to_owned(),
                reason: "It must be booked today".to_owned(),
                known_info: "Two travelers need to be in Berlin.".to_owned(),
                unknown_info: "Available flights and hotel options.".to_owned(),
            },
            false,
        );

        assert!(context.contains("task: Book travel"));
        assert!(context.contains("reason: It must be booked today"));
        assert!(context.contains("known_info: Two travelers need to be in Berlin."));
        assert!(context.contains("unknown_info: Available flights and hotel options."));
    }
}
