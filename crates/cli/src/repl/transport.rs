use pera_orchestrator::ParticipantId;

#[derive(Debug, Clone)]
pub enum InboundTransportEvent {
    InputStarted,
    InputDelta(String),
    InputCommitted,
    Shutdown,
}

#[derive(Debug, Clone)]
pub enum OutboundTransportEvent {
    MessageStarted {
        participant: ParticipantId,
    },
    MessageDelta {
        participant: ParticipantId,
        text: String,
    },
    MessageCompleted {
        participant: ParticipantId,
    },
    Status {
        participant: ParticipantId,
        text: String,
    },
    ToolCallStarted {
        participant: ParticipantId,
        tool_name: String,
    },
    ToolCallDelta {
        participant: ParticipantId,
        tool_name: String,
        delta: String,
    },
    ToolCallCompleted {
        participant: ParticipantId,
        tool_name: String,
    },
    ActionPlanned {
        participant: ParticipantId,
        action: String,
    },
}

pub fn participant_label(participant: &ParticipantId) -> &str {
    match participant {
        ParticipantId::Agent => "agent",
        ParticipantId::User => "user",
        ParticipantId::Custom(_) => "participant",
    }
}
