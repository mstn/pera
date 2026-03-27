use std::io::{self, Write};

use tokio::time::{self, Duration};

use crate::error::CliError;
use crate::repl::transport::{OutboundTransportEvent, participant_label};

pub async fn render_transport_output(
    mut outbound_rx: tokio::sync::mpsc::UnboundedReceiver<OutboundTransportEvent>,
) -> Result<(), CliError> {
    let mut current_message_has_delta = false;
    let mut loading_participant = None;
    let mut loading_frame = 0usize;
    let mut spinner = time::interval(Duration::from_millis(250));

    loop {
        tokio::select! {
            maybe_event = outbound_rx.recv() => {
                let Some(event) = maybe_event else {
                    break;
                };

                match event {
                    OutboundTransportEvent::MessageStarted { participant } => {
                        current_message_has_delta = false;
                        loading_frame = 0;
                        loading_participant = Some(participant.clone());
                        render_loading_frame(&participant, loading_frame)?;
                    }
                    OutboundTransportEvent::MessageDelta { participant, text } => {
                        if !current_message_has_delta {
                            loading_participant = None;
                            print!("\r{}> ", participant_label(&participant));
                            current_message_has_delta = true;
                        }
                        print!("{text}");
                        flush_stdout()?;
                    }
                    OutboundTransportEvent::MessageCompleted { participant } => {
                        let _ = participant;
                        current_message_has_delta = false;
                        loading_participant = None;
                        println!();
                        print!("you> ");
                        flush_stdout()?;
                    }
                    OutboundTransportEvent::ToolCallStarted { participant, tool_name } => {
                        loading_participant = None;
                        current_message_has_delta = false;
                        println!("\r{} tool> {} ...", participant_label(&participant), tool_name);
                        print!("you> ");
                        flush_stdout()?;
                    }
                    OutboundTransportEvent::ToolCallDelta {
                        participant,
                        tool_name,
                        delta,
                    } => {
                        println!(
                            "\r{} tool> {} {}",
                            participant_label(&participant),
                            tool_name,
                            delta
                        );
                        print!("you> ");
                        flush_stdout()?;
                    }
                    OutboundTransportEvent::ToolCallCompleted { participant, tool_name } => {
                        println!(
                            "\r{} tool> {} ready",
                            participant_label(&participant),
                            tool_name
                        );
                        print!("you> ");
                        flush_stdout()?;
                    }
                    OutboundTransportEvent::ActionPlanned { participant, action } => {
                        println!("\r{} action> {action}", participant_label(&participant));
                        print!("you> ");
                        flush_stdout()?;
                    }
                }
            }
            _ = spinner.tick(), if loading_participant.is_some() && !current_message_has_delta => {
                loading_frame = loading_frame.wrapping_add(1);
                if let Some(participant) = &loading_participant {
                    render_loading_frame(participant, loading_frame)?;
                }
            }
        }
    }

    Ok(())
}

fn render_loading_frame(participant: &pera_orchestrator::ParticipantId, frame: usize) -> Result<(), CliError> {
    let dots = match frame % 4 {
        0 => ".  ",
        1 => ".. ",
        2 => "...",
        _ => " ..",
    };
    print!("\r{}> {dots}", participant_label(participant));
    flush_stdout()
}

fn flush_stdout() -> Result<(), CliError> {
    io::stdout()
        .flush()
        .map_err(|source| CliError::UnexpectedStateOwned(source.to_string()))
}
