use std::marker::PhantomData;
use std::sync::Arc;

use async_trait::async_trait;
use pera_core::{RunId, WorkItemId};
use pera_llm::{LlmProvider, LlmRequest, PromptMessage};
use pera_orchestrator::{
    Participant, ParticipantDecision, ParticipantError, ParticipantId, ParticipantInboxEvent,
    ParticipantInput, ParticipantOutput, TrajectoryEvent,
};
use serde::{Deserialize, Serialize};

use crate::spec::EvalUserSpec;

const SIMULATED_USER_FIRST_TURN_SYSTEM_PROMPT: &str =
    include_str!("prompts/simulated_user_first_turn_system.md");
const SIMULATED_USER_FOLLOW_UP_SYSTEM_PROMPT: &str =
    include_str!("prompts/simulated_user_follow_up_system.md");
const SIMULATED_USER_FIRST_TURN_CONTEXT_PROMPT: &str =
    include_str!("prompts/simulated_user_first_turn_context.md");
const SIMULATED_USER_FOLLOW_UP_CONTEXT_PROMPT: &str =
    include_str!("prompts/simulated_user_follow_up_context.md");

pub struct SimulatedUserParticipant<P, O, A, U> {
    provider: P,
    spec: EvalUserSpec,
    debug_sink: Arc<dyn SimulatedUserDebugSink>,
    _marker: PhantomData<fn() -> (O, A, U)>,
}

impl<P, O, A, U> SimulatedUserParticipant<P, O, A, U> {
    pub fn new(provider: P, spec: EvalUserSpec) -> Self {
        Self::with_debug_sink(provider, spec, Arc::new(NoopSimulatedUserDebugSink))
    }

    pub fn with_debug_sink(
        provider: P,
        spec: EvalUserSpec,
        debug_sink: Arc<dyn SimulatedUserDebugSink>,
    ) -> Self {
        Self {
            provider,
            spec,
            debug_sink,
            _marker: PhantomData,
        }
    }
}

#[derive(Debug, Clone)]
pub struct SimulatedUserDebugMetadata {
    pub run_id: RunId,
    pub agent_loop_id: WorkItemId,
    pub agent_loop_iteration: usize,
    pub participant: ParticipantId,
    pub task_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SimulatedUserDebugResponseRecord {
    pub status: SimulatedUserDebugResponseStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SimulatedUserDebugResponseStatus {
    Ok,
    Error,
}

pub trait SimulatedUserDebugSink: Send + Sync {
    fn record_prompt(
        &self,
        _metadata: &SimulatedUserDebugMetadata,
        _request: &LlmRequest,
    ) -> Result<(), ParticipantError> {
        Ok(())
    }

    fn record_response(
        &self,
        _metadata: &SimulatedUserDebugMetadata,
        _response: &SimulatedUserDebugResponseRecord,
    ) -> Result<(), ParticipantError> {
        Ok(())
    }
}

#[derive(Debug, Default)]
pub struct NoopSimulatedUserDebugSink;

impl SimulatedUserDebugSink for NoopSimulatedUserDebugSink {}

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
        if user_already_spoke && !has_new_non_user_prompt(&input) {
            return Ok(ParticipantDecision::Yield);
        }

        let request = LlmRequest {
            system_prompt: simulated_user_system_prompt(user_already_spoke).to_owned(),
            messages: build_simulated_user_messages(&self.spec, &input, user_already_spoke),
            tools: Vec::new(),
        };
        let debug_metadata = SimulatedUserDebugMetadata {
            run_id: input.run_id,
            agent_loop_id: input.agent_loop_id,
            agent_loop_iteration: input.agent_loop_iteration,
            participant: input.participant.clone(),
            task_id: input.task.id.clone(),
        };
        self.debug_sink.record_prompt(&debug_metadata, &request)?;
        let response = self
            .provider
            .complete(request)
            .await
            .map_err(|error| {
                let _ = self.debug_sink.record_response(
                    &debug_metadata,
                    &SimulatedUserDebugResponseRecord {
                        status: SimulatedUserDebugResponseStatus::Error,
                        content: None,
                        error: Some(error.to_string()),
                    },
                );
                ParticipantError::new(error.to_string())
            })?;
        let content = response.content.trim();
        self.debug_sink.record_response(
            &debug_metadata,
            &SimulatedUserDebugResponseRecord {
                status: SimulatedUserDebugResponseStatus::Ok,
                content: (!content.is_empty()).then(|| content.to_owned()),
                error: None,
            },
        )?;

        if content.eq("FINISH") || content.is_empty() {
            return Ok(ParticipantDecision::Finish);
        }

        Ok(ParticipantDecision::FinalMessage {
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

fn has_new_non_user_prompt<O, A, U>(input: &ParticipantInput<O, A, U>) -> bool {
    let has_non_user_work_item = input
        .work_item
        .as_ref()
        .is_some_and(|work_item| work_item.from != ParticipantId::User);
    has_non_user_work_item || input.inbox.iter().any(|event| {
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
    simulated_user_context_prompt(user_already_spoke)
        .replace("{{task}}", task.trim())
        .replace("{{reason}}", normalize_empty(reason.trim()))
        .replace("{{known_info}}", normalize_empty(known_info.trim()))
        .replace("{{unknown_info}}", normalize_empty(unknown_info.trim()))
}

fn simulated_user_system_prompt(user_already_spoke: bool) -> &'static str {
    if user_already_spoke {
        SIMULATED_USER_FOLLOW_UP_SYSTEM_PROMPT
    } else {
        SIMULATED_USER_FIRST_TURN_SYSTEM_PROMPT
    }
}

fn simulated_user_context_prompt(user_already_spoke: bool) -> &'static str {
    if user_already_spoke {
        SIMULATED_USER_FOLLOW_UP_CONTEXT_PROMPT
    } else {
        SIMULATED_USER_FIRST_TURN_CONTEXT_PROMPT
    }
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
    use pera_core::{RunId, WorkItemId};
    use pera_orchestrator::{
        ParticipantId, ParticipantInput, RunLimits, TaskSpec, Trajectory,
    };

    use super::{has_new_non_user_prompt, simulated_user_context};
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

    #[test]
    fn non_user_work_item_counts_as_new_prompt() {
        let input = ParticipantInput::<(), (), ()> {
            run_id: RunId::generate(),
            agent_loop_id: WorkItemId::generate(),
            agent_loop_iteration: 1,
            participant: ParticipantId::User,
            work_item: Some(pera_orchestrator::WorkItem {
                id: WorkItemId::generate(),
                from: ParticipantId::Agent,
                content: "Can they return on Friday?".to_owned(),
            }),
            task: TaskSpec {
                id: "task".to_owned(),
                instructions: "test".to_owned(),
            },
            limits: RunLimits::default(),
            observation: (),
            inbox: Vec::new(),
            trajectory: Trajectory::new(RunId::generate()),
        };

        assert!(has_new_non_user_prompt(&input));
    }
}
