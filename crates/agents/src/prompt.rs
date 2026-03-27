use pera_orchestrator::{
    CodeAction, CodeObservation, CodeOutcome, ParticipantId, ParticipantInboxEvent,
    ParticipantInput, TrajectoryEvent,
};

use crate::llm::LlmToolDefinition;

const BASE_SYSTEM_PROMPT: &str = include_str!("prompts/base_system.md");
const SKILLS_SYSTEM_PROMPT: &str = include_str!("prompts/skills_system.md");

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptMessage {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Clone)]
pub struct PromptContext {
    pub task_id: String,
    pub task_instructions: String,
    pub tools: Vec<LlmToolDefinition>,
    pub available_skills: Vec<AvailableSkillPrompt>,
    pub active_skills: Vec<ActiveSkillPrompt>,
    pub inbox: Vec<PromptMessage>,
    pub transcript: Vec<PromptMessage>,
}

#[derive(Debug, Clone)]
pub struct AvailableSkillPrompt {
    pub skill_name: String,
    pub description: String,
}

#[derive(Debug, Clone)]
pub struct ActiveSkillPrompt {
    pub skill_name: String,
    pub instructions: String,
    pub python_stub: String,
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
            tools: input
                .observation
                .available_tools
                .iter()
                .map(|tool| LlmToolDefinition {
                    name: tool.name.clone(),
                    description: tool.description.clone(),
                    input_schema: tool.input_schema.clone(),
                })
                .collect(),
            available_skills: input
                .observation
                .available_skills
                .iter()
                .map(|skill| AvailableSkillPrompt {
                    skill_name: skill.skill_name.clone(),
                    description: skill.description.clone(),
                })
                .collect(),
            active_skills: input
                .observation
                .active_skills
                .iter()
                .map(|skill| ActiveSkillPrompt {
                    skill_name: skill.skill_name.clone(),
                    instructions: skill.instructions.clone(),
                    python_stub: skill.python_stub.clone(),
                })
                .collect(),
            inbox,
            transcript,
        }
    }

    fn build_system_prompt(&self, context: &PromptContext) -> String {
        let mut prompt = String::new();
        prompt.push_str(BASE_SYSTEM_PROMPT);
        if !BASE_SYSTEM_PROMPT.ends_with('\n') {
            prompt.push('\n');
        }
        prompt.push('\n');
        prompt.push_str(SKILLS_SYSTEM_PROMPT);
        if !SKILLS_SYSTEM_PROMPT.ends_with('\n') {
            prompt.push('\n');
        }
        if !context.available_skills.is_empty() {
            prompt.push_str(
                "\nUse the `load_skill` tool before relying on a skill. Available skills are listed below.\n",
            );
            prompt.push_str("<available-skills>\n");
            for skill in &context.available_skills {
                prompt.push_str("- name: ");
                prompt.push_str(&skill.skill_name);
                prompt.push('\n');
                prompt.push_str("  when_to_use: ");
                if skill.description.trim().is_empty() {
                    prompt.push_str("No description provided.");
                } else {
                    prompt.push_str(&skill.description);
                }
                prompt.push('\n');
            }
            prompt.push_str("</available-skills>\n");
        }

        if !context.active_skills.is_empty() {
            prompt.push_str("\nActive skills:\n");
            for skill in &context.active_skills {
                prompt.push_str("\nSkill: ");
                prompt.push_str(&skill.skill_name);
                prompt.push('\n');
                if !skill.instructions.trim().is_empty() {
                    prompt.push_str("Instructions:\n");
                    prompt.push_str(&skill.instructions);
                    prompt.push('\n');
                }
                if !skill.python_stub.trim().is_empty() {
                    prompt.push_str("Python stub:\n```python\n");
                    prompt.push_str(&skill.python_stub);
                    if !skill.python_stub.ends_with('\n') {
                        prompt.push('\n');
                    }
                    prompt.push_str("```\n");
                }
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

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use pera_core::{RunId, WorkItemId};
    use pera_orchestrator::{
        CodeActiveSkill, CodeAvailableSkill, CodeObservation, CodeToolDefinition,
        ParticipantInboxEvent, ParticipantInput, ParticipantId, RunLimits, TaskSpec, Trajectory,
        TrajectoryEvent,
    };
    use serde_json::json;

    use super::{CodePromptBuilder, ProviderBackedPromptBuilder};

    #[test]
    fn prompt_builder_includes_available_and_active_skills() {
        let builder = ProviderBackedPromptBuilder;
        let input = ParticipantInput {
            run_id: RunId::generate(),
            agent_loop_id: WorkItemId::generate(),
            agent_loop_iteration: 1,
            participant: ParticipantId::Agent,
            task: TaskSpec {
                id: "task".to_owned(),
                instructions: "Do the work".to_owned(),
            },
            limits: RunLimits {
                max_steps: 10,
                max_steps_per_agent_loop: 10,
                max_actions: 10,
                max_messages: 10,
                max_duration: Some(Duration::from_secs(10)),
            },
            observation: CodeObservation {
                available_tools: vec![CodeToolDefinition {
                    name: "load_skill".to_owned(),
                    description: "Activate a skill.".to_owned(),
                    input_schema: json!({
                        "type": "object",
                        "properties": {
                            "skill_name": { "type": "string" }
                        },
                        "required": ["skill_name"]
                    }),
                }],
                available_skills: vec![CodeAvailableSkill {
                    skill_name: "sqlite".to_owned(),
                    description: "Use when you need structured data queries.".to_owned(),
                }],
                active_skills: vec![CodeActiveSkill {
                    skill_name: "git".to_owned(),
                    instructions: "Use this skill for repository inspection.".to_owned(),
                    python_stub: "def status() -> str: ...".to_owned(),
                }],
            },
            inbox: vec![ParticipantInboxEvent::Message {
                from: ParticipantId::User,
                content: "Help me inspect the repo".to_owned(),
            }],
            trajectory: Trajectory {
                run_id: RunId::generate(),
                events: vec![TrajectoryEvent::ParticipantMessage {
                    participant: ParticipantId::User,
                    content: "Help me inspect the repo".to_owned(),
                }],
            },
        };

        let context = builder.build_context(&input);
        let prompt = builder.build_system_prompt(&context);

        assert!(prompt.contains("Use the `load_skill` tool before relying on a skill."));
        assert!(prompt.contains("<available-skills>"));
        assert!(prompt.contains("- name: sqlite"));
        assert!(prompt.contains("when_to_use: Use when you need structured data queries."));
        assert!(prompt.contains("</available-skills>"));
        assert!(prompt.contains("Active skills:"));
        assert!(prompt.contains("Skill: git"));
        assert!(prompt.contains("Use this skill for repository inspection."));
        assert!(prompt.contains("def status() -> str: ..."));
    }
}
