use pera_orchestrator::{
    CodeAction, CodeObservation, CodeOutcome, ParticipantId, ParticipantInboxEvent,
    ParticipantInput, TrajectoryEvent,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptMessage {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Clone)]
pub struct PromptContext {
    pub task_id: String,
    pub task_instructions: String,
    pub available_skills: Vec<String>,
    pub inbox: Vec<PromptMessage>,
    pub transcript: Vec<PromptMessage>,
}

pub trait CodePromptBuilder: Send + Sync {
    fn build_context(
        &self,
        input: &ParticipantInput<CodeObservation, CodeAction, CodeOutcome>,
    ) -> PromptContext;

    fn build_system_prompt(&self, context: &PromptContext) -> String;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ProviderBackedPromptBuilder;

impl CodePromptBuilder for ProviderBackedPromptBuilder {
    fn build_context(
        &self,
        input: &ParticipantInput<CodeObservation, CodeAction, CodeOutcome>,
    ) -> PromptContext {
        let inbox = input
            .inbox
            .iter()
            .filter_map(inbox_message)
            .collect::<Vec<_>>();
        let transcript = input
            .trajectory
            .events
            .iter()
            .filter_map(trajectory_message)
            .collect::<Vec<_>>();

        PromptContext {
            task_id: input.task.id.clone(),
            task_instructions: input.task.instructions.clone(),
            available_skills: input.observation.available_skills.clone(),
            inbox,
            transcript,
        }
    }

    fn build_system_prompt(&self, context: &PromptContext) -> String {
        let mut prompt = String::from("You are a coding agent operating in a tool loop.\n");
        prompt.push_str("Respond to the latest relevant message.\n");
        prompt.push_str("Current task:\n");
        prompt.push_str(&context.task_instructions);
        prompt.push('\n');

        if !context.available_skills.is_empty() {
            prompt.push_str("\nAvailable skills:\n");
            for skill in &context.available_skills {
                prompt.push_str("- ");
                prompt.push_str(skill);
                prompt.push('\n');
            }
        }

        prompt
    }
}

fn inbox_message(event: &ParticipantInboxEvent<CodeAction, CodeOutcome>) -> Option<PromptMessage> {
    match event {
        ParticipantInboxEvent::Message { from, content } => Some(PromptMessage {
            role: role_for_participant(from),
            content: content.clone(),
        }),
        ParticipantInboxEvent::Notification { message } => Some(PromptMessage {
            role: "system".to_owned(),
            content: message.clone(),
        }),
        ParticipantInboxEvent::ActionAccepted { .. }
        | ParticipantInboxEvent::ActionCompleted { .. }
        | ParticipantInboxEvent::ActionFailed { .. } => None,
    }
}

fn trajectory_message(
    event: &TrajectoryEvent<CodeObservation, CodeAction, CodeOutcome>,
) -> Option<PromptMessage> {
    match event {
        TrajectoryEvent::ParticipantMessage {
            participant,
            content,
        } => Some(PromptMessage {
            role: role_for_participant(participant),
            content: content.clone(),
        }),
        _ => None,
    }
}

fn role_for_participant(participant: &ParticipantId) -> String {
    match participant {
        ParticipantId::Agent => "assistant".to_owned(),
        ParticipantId::User => "user".to_owned(),
        ParticipantId::Custom(name) => name.clone(),
    }
}
