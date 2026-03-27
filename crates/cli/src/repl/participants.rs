use async_trait::async_trait;
use pera_orchestrator::{
    CodeAction, Participant, ParticipantDecision, ParticipantError, ParticipantId,
    ParticipantInput, ParticipantOutput,
};
use tokio::sync::mpsc;

use crate::repl::transport::InboundTransportEvent;

pub struct HumanParticipant {
    pub input_rx: mpsc::UnboundedReceiver<InboundTransportEvent>,
}

#[async_trait]
impl Participant for HumanParticipant {
    type Observation = pera_orchestrator::CodeObservation;
    type Action = CodeAction;
    type Outcome = pera_orchestrator::CodeOutcome;

    fn id(&self) -> ParticipantId {
        ParticipantId::User
    }

    async fn respond(
        &mut self,
        _input: ParticipantInput<Self::Observation, Self::Action, Self::Outcome>,
        _output: &mut dyn ParticipantOutput<Self::Action>,
    ) -> Result<ParticipantDecision<Self::Action>, ParticipantError> {
        let mut buffer = String::new();
        loop {
            match self.input_rx.recv().await {
                Some(InboundTransportEvent::InputStarted) => {
                    buffer.clear();
                }
                Some(InboundTransportEvent::InputDelta(delta)) => {
                    buffer.push_str(&delta);
                }
                Some(InboundTransportEvent::InputCommitted) => {
                    let message = buffer.trim_end_matches('\n').trim().to_owned();
                    if message == "/exit" {
                        return Ok(ParticipantDecision::Finish);
                    }
                    return Ok(ParticipantDecision::FinalMessage { content: message });
                }
                Some(InboundTransportEvent::Shutdown) | None => {
                    return Ok(ParticipantDecision::Finish);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use async_trait::async_trait;
    use pera_core::RunId;
    use pera_orchestrator::{
        CodeObservation, ParticipantInboxEvent, RunLimits, TaskSpec, Trajectory, TrajectoryEvent,
    };

    use super::*;

    struct DemoAgentParticipant;

    #[async_trait]
    impl Participant for DemoAgentParticipant {
        type Observation = pera_orchestrator::CodeObservation;
        type Action = CodeAction;
        type Outcome = pera_orchestrator::CodeOutcome;

        fn id(&self) -> ParticipantId {
            ParticipantId::Agent
        }

        async fn respond(
            &mut self,
            input: ParticipantInput<Self::Observation, Self::Action, Self::Outcome>,
            output: &mut dyn ParticipantOutput<Self::Action>,
        ) -> Result<ParticipantDecision<Self::Action>, ParticipantError> {
            let Some(user_message) = last_user_message(&input) else {
                return Ok(ParticipantDecision::Yield);
            };

            let response = if user_message == "/help" {
                "Try typing any message. Use /exit to leave the session.".to_owned()
            } else {
                format!("Echo: {user_message}")
            };

            output.message_start(&ParticipantId::Agent).await?;
            for ch in response.chars() {
                let mut chunk = String::new();
                chunk.push(ch);
                output.message_delta(&ParticipantId::Agent, &chunk).await?;
            }
            output.message_end(&ParticipantId::Agent).await?;

            Ok(ParticipantDecision::FinalMessage { content: response })
        }
    }

    fn last_user_message(
        input: &ParticipantInput<
            pera_orchestrator::CodeObservation,
            CodeAction,
            pera_orchestrator::CodeOutcome,
        >,
    ) -> Option<&str> {
        if let Some(message) = input.inbox.iter().rev().find_map(|event| match event {
            ParticipantInboxEvent::Message {
                from: ParticipantId::User,
                content,
            } => Some(content.as_str()),
            _ => None,
        }) {
            return Some(message);
        }

        match input.trajectory.events.iter().rev().find(|event| {
            matches!(event, TrajectoryEvent::ParticipantMessage { .. })
        }) {
            Some(TrajectoryEvent::ParticipantMessage {
                participant: ParticipantId::User,
                content,
            }) => Some(content.as_str()),
            _ => None,
        }
    }

    struct RecordingOutput {
        chunks: String,
    }

    #[async_trait]
    impl ParticipantOutput<CodeAction> for RecordingOutput {
        async fn message_start(
            &mut self,
            _participant: &ParticipantId,
        ) -> Result<(), ParticipantError> {
            Ok(())
        }

        async fn message_delta(
            &mut self,
            _participant: &ParticipantId,
            delta: &str,
        ) -> Result<(), ParticipantError> {
            self.chunks.push_str(delta);
            Ok(())
        }

        async fn message_end(
            &mut self,
            _participant: &ParticipantId,
        ) -> Result<(), ParticipantError> {
            Ok(())
        }
    }

    fn test_input(
        events: Vec<TrajectoryEvent<CodeObservation, CodeAction, pera_orchestrator::CodeOutcome>>,
    ) -> ParticipantInput<CodeObservation, CodeAction, pera_orchestrator::CodeOutcome> {
        ParticipantInput {
            run_id: RunId::generate(),
            participant: ParticipantId::Agent,
            task: TaskSpec {
                id: "repl".to_owned(),
                instructions: "test".to_owned(),
            },
            limits: RunLimits::default(),
            observation: CodeObservation {
                cwd: std::path::PathBuf::from("."),
                workspace_root: std::path::PathBuf::from("."),
                available_skills: Vec::new(),
            },
            inbox: Vec::<
                ParticipantInboxEvent<CodeAction, pera_orchestrator::CodeOutcome>,
            >::new(),
            trajectory: Trajectory {
                run_id: RunId::generate(),
                events,
            },
        }
    }

    #[tokio::test]
    async fn demo_agent_yields_after_it_already_responded_to_latest_user_message() {
        let mut participant = DemoAgentParticipant;
        let mut output = RecordingOutput {
            chunks: String::new(),
        };
        let input = test_input(vec![
            TrajectoryEvent::ParticipantMessage {
                participant: ParticipantId::User,
                content: "hello".to_owned(),
            },
            TrajectoryEvent::ParticipantMessage {
                participant: ParticipantId::Agent,
                content: "Echo: hello".to_owned(),
            },
        ]);

        let decision = participant.respond(input, &mut output).await.unwrap();

        assert_eq!(decision, ParticipantDecision::Yield);
        assert!(output.chunks.is_empty());
    }

    #[tokio::test]
    async fn demo_agent_responds_when_latest_message_is_from_user() {
        let mut participant = DemoAgentParticipant;
        let mut output = RecordingOutput {
            chunks: String::new(),
        };
        let input = test_input(vec![TrajectoryEvent::ParticipantMessage {
            participant: ParticipantId::User,
            content: "hello".to_owned(),
        }]);

        let decision = participant.respond(input, &mut output).await.unwrap();

        assert_eq!(
            decision,
            ParticipantDecision::FinalMessage {
                content: "Echo: hello".to_owned(),
            }
        );
        assert_eq!(output.chunks, "Echo: hello");
    }

    #[tokio::test]
    async fn demo_agent_responds_to_user_message_from_inbox() {
        let mut participant = DemoAgentParticipant;
        let mut output = RecordingOutput {
            chunks: String::new(),
        };
        let mut input = test_input(Vec::new());
        input.inbox.push(ParticipantInboxEvent::Message {
            from: ParticipantId::User,
            content: "from inbox".to_owned(),
        });

        let decision = participant.respond(input, &mut output).await.unwrap();

        assert_eq!(
            decision,
            ParticipantDecision::FinalMessage {
                content: "Echo: from inbox".to_owned(),
            }
        );
        assert_eq!(output.chunks, "Echo: from inbox");
    }
}
