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
    let mut ephemeral_line_active = false;
    let mut active_status_line: Option<String> = None;
    let mut status_frame = 0usize;
    let mut spinner = time::interval(Duration::from_millis(250));

    loop {
        tokio::select! {
            maybe_event = outbound_rx.recv() => {
                let Some(event) = maybe_event else {
                    break;
                };

                match event {
                    OutboundTransportEvent::MessageStarted { participant } => {
                        clear_ephemeral_line(&mut ephemeral_line_active, &mut active_status_line)?;
                        current_message_has_delta = false;
                        loading_frame = 0;
                        loading_participant = Some(participant.clone());
                        render_loading_frame(&participant, loading_frame)?;
                    }
                    OutboundTransportEvent::MessageDelta { participant, text } => {
                        if !current_message_has_delta {
                            clear_ephemeral_line(&mut ephemeral_line_active, &mut active_status_line)?;
                            loading_participant = None;
                            print!("\r{}> ", participant_label(&participant));
                            current_message_has_delta = true;
                        }
                        print!("{text}");
                        flush_stdout()?;
                    }
                    OutboundTransportEvent::MessageCompleted { participant } => {
                        let _ = participant;
                        let had_delta = current_message_has_delta;
                        current_message_has_delta = false;
                        loading_participant = None;
                        active_status_line = None;
                        ephemeral_line_active = false;
                        if had_delta {
                            println!();
                            print!("you> ");
                            flush_stdout()?;
                        } else {
                            clear_line()?;
                        }
                    }
                    OutboundTransportEvent::Status { participant, text } => {
                        active_status_line =
                            Some(format!("{} status> {text}", participant_label(&participant)));
                        status_frame = 0;
                        render_status_frame(active_status_line.as_deref().unwrap(), status_frame)?;
                        ephemeral_line_active = true;
                    }
                    OutboundTransportEvent::ToolCallStarted { participant, tool_name } => {
                        clear_ephemeral_line(&mut ephemeral_line_active, &mut active_status_line)?;
                        print_colored_persisted_line(
                            &format!(
                                "{} debug> tool {} ...",
                                participant_label(&participant),
                                tool_name
                            ),
                            "36",
                        )?;
                    }
                    OutboundTransportEvent::ToolCallDelta {
                        participant,
                        tool_name,
                        delta,
                    } => {
                        let _ = delta;
                        clear_ephemeral_line(&mut ephemeral_line_active, &mut active_status_line)?;
                        print_colored_persisted_line(
                            &format!(
                                "{} debug> tool {} arguments ...",
                                participant_label(&participant),
                                tool_name,
                            ),
                            "36",
                        )?;
                    }
                    OutboundTransportEvent::ToolCallCompleted { participant, tool_name } => {
                        clear_ephemeral_line(&mut ephemeral_line_active, &mut active_status_line)?;
                        print_colored_persisted_line(
                            &format!(
                                "{} debug> tool {} ready",
                                participant_label(&participant),
                                tool_name,
                            ),
                            "36",
                        )?;
                    }
                    OutboundTransportEvent::ActionPlanned { participant, action } => {
                        clear_ephemeral_line(&mut ephemeral_line_active, &mut active_status_line)?;
                        print_persisted_line(&format!(
                            "{} action> {action}",
                            participant_label(&participant)
                        ))?;
                    }
                    OutboundTransportEvent::ActionCompleted { participant, status } => {
                        clear_ephemeral_line(&mut ephemeral_line_active, &mut active_status_line)?;
                        print_persisted_line(&format!(
                            "{} status> {status}",
                            participant_label(&participant)
                        ))?;
                    }
                    OutboundTransportEvent::ActionFailed { participant, status } => {
                        clear_ephemeral_line(&mut ephemeral_line_active, &mut active_status_line)?;
                        print_persisted_line(&format!(
                            "{} status> {status}",
                            participant_label(&participant)
                        ))?;
                    }
                }
            }
            _ = spinner.tick(), if !current_message_has_delta && (loading_participant.is_some() || active_status_line.is_some()) => {
                if let Some(participant) = &loading_participant {
                    loading_frame = loading_frame.wrapping_add(1);
                    render_loading_frame(participant, loading_frame)?;
                } else if let Some(status) = &active_status_line {
                    status_frame = status_frame.wrapping_add(1);
                    render_status_frame(status, status_frame)?;
                    ephemeral_line_active = true;
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

fn render_ephemeral_line(text: &str) -> Result<(), CliError> {
    print!("\r\x1b[2K{text}");
    flush_stdout()
}

fn render_status_frame(text: &str, frame: usize) -> Result<(), CliError> {
    let dots = match frame % 4 {
        0 => ".  ",
        1 => ".. ",
        2 => "...",
        _ => " ..",
    };
    render_ephemeral_line(&format!("{text} {dots}"))
}

fn clear_ephemeral_line(
    ephemeral_line_active: &mut bool,
    active_status_line: &mut Option<String>,
) -> Result<(), CliError> {
    if *ephemeral_line_active {
        clear_line()?;
        *ephemeral_line_active = false;
    }
    *active_status_line = None;
    Ok(())
}

fn print_persisted_line(text: &str) -> Result<(), CliError> {
    print!("\r\x1b[2K{text}\n");
    print!("you> ");
    flush_stdout()
}

fn print_colored_persisted_line(text: &str, ansi_color: &str) -> Result<(), CliError> {
    print!("\r\x1b[2K\x1b[{ansi_color}m{text}\x1b[0m\n");
    print!("you> ");
    flush_stdout()
}

fn clear_line() -> Result<(), CliError> {
    print!("\r\x1b[2K");
    flush_stdout()
}

fn flush_stdout() -> Result<(), CliError> {
    io::stdout()
        .flush()
        .map_err(|source| CliError::UnexpectedStateOwned(source.to_string()))
}
