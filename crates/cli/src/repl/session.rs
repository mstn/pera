use std::io::{self, BufRead, Write};

use async_trait::async_trait;
use pera_agents::{LlmAgentParticipant, OpenAiConfig as OpenAiProviderConfig, OpenAiProvider, ProviderBackedPromptBuilder};
use pera_orchestrator::{
    CodeAction, InitialInboxMessage, Participant, ParticipantError, ParticipantId,
    ParticipantOutput, RuntimeCodeEnvironment, RunLimits, RunRequest, TaskSpec,
    TerminationCondition,
};
use pera_runtime::CodeEnvironment;
use tokio::sync::mpsc;

use crate::config::AgentConfig;
use crate::error::CliError;
use crate::repl::participants::HumanParticipant;
use crate::repl::renderer::render_transport_output;
use crate::repl::transport::{InboundTransportEvent, OutboundTransportEvent};

pub async fn run_repl(agent_config: AgentConfig) -> Result<(), CliError> {
    let environment = RuntimeCodeEnvironment::new(
        CodeEnvironment::new(&agent_config.project_root, None)
            .map_err(|error| CliError::UnexpectedStateOwned(error.to_string()))?,
    );
    let (console_input_tx, console_input_rx) = mpsc::unbounded_channel();
    let (console_output_tx, console_output_rx) = mpsc::unbounded_channel();

    let input_handle =
        std::thread::spawn(move || read_console_input(console_input_tx));
    let render_handle =
        tokio::spawn(render_transport_output(console_output_rx));

    println!("Starting REPL. Type /help for help, /exit to quit.");
    println!(
        "Project root: {}",
        agent_config.project_root.display()
    );
    if let Some(openai) = &agent_config.openai {
        let api_key_status = if openai.api_key.is_empty() {
            "missing API key"
        } else {
            "API key present"
        };
        println!(
            "Configured OpenAI model: {} ({api_key_status})",
            openai.model
        );
    } else {
        println!("OpenAI configuration not found. The LLM agent is still unconfigured.");
    }
    print!("you> ");
    let _ = io::stdout().flush();

    let participants: Vec<
        Box<
            dyn Participant<
                    Observation = pera_orchestrator::CodeObservation,
                    Action = CodeAction,
                    Outcome = pera_orchestrator::CodeOutcome,
                >,
        >,
    > = vec![
        Box::new(HumanParticipant { input_rx: console_input_rx }),
        match &agent_config.openai {
            Some(openai) => Box::new(
                LlmAgentParticipant::new(
                    OpenAiProvider::new(OpenAiProviderConfig {
                        api_key: openai.api_key.clone(),
                        model: openai.model.clone(),
                    })
                    .map_err(|error| CliError::UnexpectedStateOwned(error.to_string()))?,
                    ProviderBackedPromptBuilder,
                ),
            ),
            None => Box::new(LlmAgentParticipant::unconfigured()),
        },
    ];
    let mut orchestrator =
        pera_orchestrator::Orchestrator::from_participants(participants, environment);
    let mut output = TransportBackedOutput {
        output_tx: console_output_tx,
    };
    let result = orchestrator
        .run_with_output(
            RunRequest {
                task: TaskSpec {
                    id: "cli-repl".to_owned(),
                    instructions: "Interactive user and agent conversation".to_owned(),
                },
                limits: RunLimits {
                    max_steps: usize::MAX,
                    max_steps_per_agent_loop: usize::MAX,
                    max_actions: usize::MAX,
                    max_messages: usize::MAX,
                    max_duration: None,
                },
                termination_condition: TerminationCondition::AnyOfParticipantsFinished(
                    vec![ParticipantId::User],
                ),
                initial_messages: vec![InitialInboxMessage {
                    to: ParticipantId::User,
                    from: ParticipantId::Custom("system".to_owned()),
                    content: "start".to_owned(),
                }],
            },
            &mut output,
        )
        .await
        .map_err(|error| CliError::UnexpectedStateOwned(error.to_string()));

    drop(output);
    let _ = input_handle.join();
    render_handle
        .await
        .map_err(|error| CliError::UnexpectedStateOwned(error.to_string()))?
        .map_err(|error| CliError::UnexpectedStateOwned(error.to_string()))?;

    result.map(|_| ())
}

struct TransportBackedOutput {
    output_tx: mpsc::UnboundedSender<OutboundTransportEvent>,
}

#[async_trait]
impl ParticipantOutput<CodeAction> for TransportBackedOutput {
    async fn message_start(
        &mut self,
        participant: &ParticipantId,
    ) -> Result<(), ParticipantError> {
        self.output_tx
            .send(OutboundTransportEvent::MessageStarted {
                participant: participant.clone(),
            })
            .map_err(|_| ParticipantError::new("stream output channel is closed"))
    }

    async fn message_delta(
        &mut self,
        participant: &ParticipantId,
        delta: &str,
    ) -> Result<(), ParticipantError> {
        self.output_tx
            .send(OutboundTransportEvent::MessageDelta {
                participant: participant.clone(),
                text: delta.to_owned(),
            })
            .map_err(|_| ParticipantError::new("stream output channel is closed"))
    }

    async fn message_end(
        &mut self,
        participant: &ParticipantId,
    ) -> Result<(), ParticipantError> {
        self.output_tx
            .send(OutboundTransportEvent::MessageCompleted {
                participant: participant.clone(),
            })
            .map_err(|_| ParticipantError::new("stream output channel is closed"))
    }

    async fn action_planned(
        &mut self,
        participant: &ParticipantId,
        action: &CodeAction,
    ) -> Result<(), ParticipantError> {
        self.output_tx
            .send(OutboundTransportEvent::ActionPlanned {
                participant: participant.clone(),
                action: format!("{action:?}"),
            })
            .map_err(|_| ParticipantError::new("stream output channel is closed"))
    }
}

fn read_console_input(
    console_input_tx: mpsc::UnboundedSender<InboundTransportEvent>,
) -> Result<(), CliError> {
    let stdin = io::stdin();
    let mut lines = stdin.lock().lines();
    while let Some(line) = lines.next() {
        let line = line.map_err(|source| CliError::UnexpectedStateOwned(source.to_string()))?;
        console_input_tx
            .send(InboundTransportEvent::InputStarted)
            .map_err(|_| CliError::UnexpectedStateOwned("repl input channel closed".to_owned()))?;
        for ch in line.chars() {
            console_input_tx
                .send(InboundTransportEvent::InputDelta(ch.to_string()))
                .map_err(|_| CliError::UnexpectedStateOwned("repl input channel closed".to_owned()))?;
        }
        console_input_tx
            .send(InboundTransportEvent::InputCommitted)
            .map_err(|_| CliError::UnexpectedStateOwned("repl input channel closed".to_owned()))?;

        if line.trim() == "/exit" {
            break;
        }
    }
    let _ = console_input_tx.send(InboundTransportEvent::Shutdown);
    Ok(())
}
