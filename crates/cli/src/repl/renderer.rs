use std::io::{self, Write};

use crate::error::CliError;
use crate::repl::transport::{OutboundTransportEvent, participant_label};

pub async fn render_transport_output(
    mut outbound_rx: tokio::sync::mpsc::UnboundedReceiver<OutboundTransportEvent>,
) -> Result<(), CliError> {
    while let Some(event) = outbound_rx.recv().await {
        match event {
            OutboundTransportEvent::MessageStarted { participant } => {
                print!("\r{}> ", participant_label(&participant));
                io::stdout()
                    .flush()
                    .map_err(|source| CliError::UnexpectedStateOwned(source.to_string()))?;
            }
            OutboundTransportEvent::MessageDelta { participant, text } => {
                let _ = participant;
                print!("{text}");
                io::stdout()
                    .flush()
                    .map_err(|source| CliError::UnexpectedStateOwned(source.to_string()))?;
            }
            OutboundTransportEvent::MessageCompleted { participant } => {
                let _ = participant;
                println!();
                print!("you> ");
                io::stdout()
                    .flush()
                    .map_err(|source| CliError::UnexpectedStateOwned(source.to_string()))?;
            }
            OutboundTransportEvent::ActionPlanned { participant, action } => {
                println!("\r{} action> {action}", participant_label(&participant));
                print!("you> ");
                io::stdout()
                    .flush()
                    .map_err(|source| CliError::UnexpectedStateOwned(source.to_string()))?;
            }
        }
    }

    Ok(())
}
