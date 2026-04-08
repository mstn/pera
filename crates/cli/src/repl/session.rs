use std::sync::Arc;
use std::io::{self, BufRead, Write};

use async_trait::async_trait;
use pera_agents::{
    LlmAgentParticipant, OpenAiConfig as OpenAiProviderConfig, OpenAiProvider,
    ProviderBackedPromptBuilder,
};
use pera_orchestrator::{
    ActionError, InitialInboxMessage, ParticipantError, ParticipantId, ParticipantOutput,
    RunLimits, RunRequest, TaskSpec, TerminationCondition,
};
use pera_runtime::{
    AgentWorkspace, WorkspaceAction, WorkspaceOutcome, WorkspaceParticipantDyn,
};
use tokio::sync::mpsc;
use tracing::info;

use crate::config::AgentConfig;
use crate::error::CliError;
use crate::repl::participants::HumanParticipant;
use crate::repl::prompt_debug::FilePromptDebugSink;
use crate::repl::renderer::render_transport_output;
use crate::repl::transport::{InboundTransportEvent, OutboundTransportEvent};

pub async fn run_repl(agent_config: AgentConfig) -> Result<(), CliError> {
    info!(
        root = %agent_config.root.display(),
        catalog_root = %agent_config.root.join("catalog").join("skills").display(),
        "building agent workspace for repl",
    );
    let environment = AgentWorkspace::from_root(&agent_config.root)
        .await
        .map_err(|error| CliError::UnexpectedStateOwned(error.to_string()))?;
    let (console_input_tx, console_input_rx) = mpsc::unbounded_channel();
    let (console_output_tx, console_output_rx) = mpsc::unbounded_channel();

    let input_handle =
        std::thread::spawn(move || read_console_input(console_input_tx));
    let render_handle =
        tokio::spawn(render_transport_output(console_output_rx));

    println!("Starting REPL. Type /help for help, /exit to quit.");
    println!(
        "Root: {}",
        agent_config.root.display()
    );
    if agent_config.debug {
        println!("Prompt debug logging: enabled");
    }
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

    let participants: Vec<Box<WorkspaceParticipantDyn>> = vec![
        Box::new(HumanParticipant { input_rx: console_input_rx }),
        match &agent_config.openai {
            Some(openai) => {
                let participant = if agent_config.debug {
                    LlmAgentParticipant::with_debug_sink(
                        OpenAiProvider::new(OpenAiProviderConfig {
                            api_key: openai.api_key.clone(),
                            model: openai.model.clone(),
                        })
                        .map_err(|error| CliError::UnexpectedStateOwned(error.to_string()))?,
                        ProviderBackedPromptBuilder,
                        Arc::new(FilePromptDebugSink::new(
                            agent_config.root.clone(),
                            Some(openai.model.clone()),
                        )),
                    )
                } else {
                    LlmAgentParticipant::new(
                        OpenAiProvider::new(OpenAiProviderConfig {
                            api_key: openai.api_key.clone(),
                            model: openai.model.clone(),
                        })
                        .map_err(|error| CliError::UnexpectedStateOwned(error.to_string()))?,
                        ProviderBackedPromptBuilder,
                    )
                };
                Box::new(participant)
            }
            None => Box::new(LlmAgentParticipant::unconfigured()),
        },
    ];
    let mut orchestrator =
        pera_orchestrator::Orchestrator::from_participants(participants, environment);
    let mut output = TransportBackedOutput {
        output_tx: console_output_tx,
        show_tool_events: agent_config.debug,
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
                    max_failed_actions: None,
                    max_consecutive_failed_actions: None,
                    max_blocked_action_wait: None,
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
    show_tool_events: bool,
}

#[async_trait]
impl ParticipantOutput<WorkspaceAction, WorkspaceOutcome> for TransportBackedOutput {
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

    async fn status_update(
        &mut self,
        participant: &ParticipantId,
        status: &str,
    ) -> Result<(), ParticipantError> {
        let text = display_status(status);
        self.output_tx
            .send(OutboundTransportEvent::Status {
                participant: participant.clone(),
                text,
            })
            .map_err(|_| ParticipantError::new("stream output channel is closed"))
    }

    async fn tool_call_start(
        &mut self,
        participant: &ParticipantId,
        tool_name: &str,
    ) -> Result<(), ParticipantError> {
        if !self.show_tool_events {
            return Ok(());
        }
        self.output_tx
            .send(OutboundTransportEvent::ToolCallStarted {
                participant: participant.clone(),
                tool_name: tool_name.to_owned(),
            })
            .map_err(|_| ParticipantError::new("stream output channel is closed"))
    }

    async fn tool_call_delta(
        &mut self,
        participant: &ParticipantId,
        tool_name: &str,
        delta: &str,
    ) -> Result<(), ParticipantError> {
        if !self.show_tool_events {
            return Ok(());
        }
        self.output_tx
            .send(OutboundTransportEvent::ToolCallDelta {
                participant: participant.clone(),
                tool_name: tool_name.to_owned(),
                delta: delta.to_owned(),
            })
            .map_err(|_| ParticipantError::new("stream output channel is closed"))
    }

    async fn tool_call_end(
        &mut self,
        participant: &ParticipantId,
        tool_name: &str,
    ) -> Result<(), ParticipantError> {
        if !self.show_tool_events {
            return Ok(());
        }
        self.output_tx
            .send(OutboundTransportEvent::ToolCallCompleted {
                participant: participant.clone(),
                tool_name: tool_name.to_owned(),
            })
            .map_err(|_| ParticipantError::new("stream output channel is closed"))
    }

    async fn action_planned(
        &mut self,
        participant: &ParticipantId,
        action: &WorkspaceAction,
    ) -> Result<(), ParticipantError> {
        if matches!(action, WorkspaceAction::ExecuteCode { .. }) {
            return Ok(());
        }
        let action = match action {
            WorkspaceAction::LoadSkill { skill_name } => format!("load skill {skill_name}"),
            WorkspaceAction::UnloadSkill { skill_name } => format!("unload skill {skill_name}"),
            WorkspaceAction::ExecuteCode { .. } => unreachable!("execute_code is hidden from the REPL"),
        };
        self.output_tx
            .send(OutboundTransportEvent::ActionPlanned {
                participant: participant.clone(),
                action,
            })
            .map_err(|_| ParticipantError::new("stream output channel is closed"))
    }

    async fn action_completed(
        &mut self,
        participant: &ParticipantId,
        action: &WorkspaceAction,
        outcome: &WorkspaceOutcome,
    ) -> Result<(), ParticipantError> {
        if matches!(action, WorkspaceAction::ExecuteCode { .. }) {
            return Ok(());
        }
        let status = match (action, outcome) {
            (
                WorkspaceAction::ExecuteCode { .. },
                WorkspaceOutcome::CodeExecuted { .. },
            ) => "code execution completed".to_owned(),
            (
                WorkspaceAction::LoadSkill { skill_name },
                WorkspaceOutcome::SkillLoaded { .. },
            ) => format!("skill loaded: {skill_name}"),
            (
                WorkspaceAction::UnloadSkill { skill_name },
                WorkspaceOutcome::SkillUnloaded { .. },
            ) => format!("skill unloaded: {skill_name}"),
            _ => return Ok(()),
        };
        self.output_tx
            .send(OutboundTransportEvent::ActionCompleted {
                participant: participant.clone(),
                status,
            })
            .map_err(|_| ParticipantError::new("stream output channel is closed"))
    }

    async fn action_failed(
        &mut self,
        participant: &ParticipantId,
        action: &WorkspaceAction,
        error: &ActionError,
    ) -> Result<(), ParticipantError> {
        let status = match action {
            WorkspaceAction::ExecuteCode { .. } => {
                format!("request failed: {}", error.user_message)
            }
            WorkspaceAction::LoadSkill { skill_name } => {
                format!("failed to load skill {skill_name}: {}", error.user_message)
            }
            WorkspaceAction::UnloadSkill { skill_name } => {
                format!("failed to unload skill {skill_name}: {}", error.user_message)
            }
        };
        self.output_tx
            .send(OutboundTransportEvent::ActionFailed {
                participant: participant.clone(),
                status,
            })
            .map_err(|_| ParticipantError::new("stream output channel is closed"))
    }
}

fn display_status(status: &str) -> String {
    if is_execution_status(status) {
        "working".to_owned()
    } else {
        status.to_owned()
    }
}

fn is_execution_status(status: &str) -> bool {
    matches!(
        status,
        "preparing code execution"
            | "executing code"
            | "code execution submitted"
            | "running code"
            | "waiting for skill action"
            | "resuming code execution"
    ) || status.starts_with("skill action claimed by ")
        || status.starts_with("skill action failed: ")
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
