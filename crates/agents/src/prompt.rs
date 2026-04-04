use pera_orchestrator::{ParticipantId, ParticipantInboxEvent, ParticipantInput, TrajectoryEvent};
use pera_runtime::{WorkspaceAction, WorkspaceObservation, WorkspaceOutcome};

use crate::llm::LlmToolDefinition;

const BASE_SYSTEM_PROMPT: &str = include_str!("prompts/base_system.md");
const SKILLS_SYSTEM_PROMPT: &str = include_str!("prompts/skills_system.md");
const CODE_GENERATION_SYSTEM_PROMPT: &str = include_str!("prompts/code_generation_system.md");

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptMessage {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Clone)]
pub struct PromptContext {
    pub task_id: String,
    pub work_item: Option<pera_orchestrator::WorkItem>,
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
        input: &ParticipantInput<WorkspaceObservation, WorkspaceAction, WorkspaceOutcome>,
    ) -> PromptContext;

    fn build_system_prompt(&self, context: &PromptContext) -> String;

    fn build_user_task_message(&self, context: &PromptContext) -> Option<PromptMessage>;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ProviderBackedPromptBuilder;

impl CodePromptBuilder for ProviderBackedPromptBuilder {
    fn build_context(
        &self,
        input: &ParticipantInput<WorkspaceObservation, WorkspaceAction, WorkspaceOutcome>,
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
            work_item: input.work_item.clone(),
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
        prompt.push_str("\nAvailable skills are listed below.\n");
        prompt.push_str("<available-skills>\n");
        if context.available_skills.is_empty() {
            prompt.push_str("no skill available\n");
        } else {
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
        }
        prompt.push_str("</available-skills>\n");

        prompt.push('\n');
        prompt.push_str(CODE_GENERATION_SYSTEM_PROMPT);
        if !CODE_GENERATION_SYSTEM_PROMPT.ends_with('\n') {
            prompt.push('\n');
        }

        prompt
    }

    fn build_user_task_message(&self, context: &PromptContext) -> Option<PromptMessage> {
        let work_item = context.work_item.as_ref()?;
        let mut content = String::new();
        content.push_str("<task>\n");
        content.push_str(&work_item.content);
        if !work_item.content.ends_with('\n') {
            content.push('\n');
        }
        content.push_str("</task>\n");

        let declarations = context
            .active_skills
            .iter()
            .filter_map(|skill| {
                let stub = skill.python_stub.trim();
                if stub.is_empty() {
                    None
                } else {
                    Some(stub)
                }
            })
            .collect::<Vec<_>>();

        if !declarations.is_empty() {
            content.push('\n');
            content.push_str("<declarations>\n```python\n");
            content.push_str(&declarations.join("\n\n"));
            content.push('\n');
            content.push_str("```\n</declarations>\n");
        }

        Some(PromptMessage {
            role: "user".to_owned(),
            content,
        })
    }
}

fn inbox_message(
    event: &ParticipantInboxEvent<WorkspaceAction, WorkspaceOutcome>,
) -> Option<PromptMessage> {
    match event {
        ParticipantInboxEvent::Message { from, content } => Some(PromptMessage {
            role: role_for_participant(from),
            content: content.clone(),
        }),
        ParticipantInboxEvent::ActionCompleted { outcome, .. } => action_completed_message(outcome),
        ParticipantInboxEvent::ActionFailed { error, .. } => Some(PromptMessage {
            role: "system".to_owned(),
            content: format!(
                "Action failed: {}\nOrigin: {:?}\nDetail:\n{}",
                error.user_message, error.origin, error.detail
            ),
        }),
        ParticipantInboxEvent::Notification { message } => Some(PromptMessage {
            role: "system".to_owned(),
            content: message.clone(),
        }),
        ParticipantInboxEvent::ActionScheduled { .. } => None,
    }
}

fn trajectory_message(
    event: &TrajectoryEvent<WorkspaceObservation, WorkspaceAction, WorkspaceOutcome>,
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

fn action_completed_message(outcome: &WorkspaceOutcome) -> Option<PromptMessage> {
    match outcome {
        WorkspaceOutcome::CodeExecuted { language, result } => Some(PromptMessage {
            role: "system".to_owned(),
            content: format!(
                "Code execution completed.\nLanguage: {language}\nResult:\n{}",
                serde_json::to_string_pretty(result).unwrap_or_else(|_| format!("{result:?}"))
            ),
        }),
        WorkspaceOutcome::SkillLoaded { skill_name } => Some(PromptMessage {
            role: "system".to_owned(),
            content: format!(
                "Skill loaded: {skill_name}. This skill is now active and ready to use. Do not call load_skill for {skill_name} again unless it is later unloaded."
            ),
        }),
        WorkspaceOutcome::SkillUnloaded { skill_name } => Some(PromptMessage {
            role: "system".to_owned(),
            content: format!(
                "Skill unloaded: {skill_name}. This skill is no longer active and must be loaded again before use."
            ),
        }),
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use pera_core::{RunId, WorkItemId};
    use pera_orchestrator::{
        ParticipantId, ParticipantInboxEvent, ParticipantInput, RunLimits, TaskSpec, Trajectory,
        TrajectoryEvent,
    };
    use pera_runtime::{
        AgentWorkspaceTool, WorkspaceActiveSkill, WorkspaceAvailableSkill, WorkspaceObservation,
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
            work_item: Some(pera_orchestrator::WorkItem {
                id: WorkItemId::generate(),
                from: ParticipantId::User,
                content: "Help me inspect the repo".to_owned(),
            }),
            task: TaskSpec {
                id: "task".to_owned(),
                instructions: "Do the work".to_owned(),
            },
            limits: RunLimits {
                max_steps: 10,
                max_steps_per_agent_loop: 10,
                max_actions: 10,
                max_messages: 10,
                max_failed_actions: None,
                max_consecutive_failed_actions: None,
                max_duration: Some(Duration::from_secs(10)),
            },
            observation: WorkspaceObservation {
                available_tools: vec![AgentWorkspaceTool {
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
                available_skills: vec![WorkspaceAvailableSkill {
                    skill_name: "sqlite".to_owned(),
                    description: "Use when you need structured data queries.".to_owned(),
                }],
                active_skills: vec![WorkspaceActiveSkill {
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
        let task_message = builder.build_user_task_message(&context).unwrap();

        assert!(prompt.contains("You are a helpful assistant that solves user tasks."));
        assert!(prompt.contains("Skills are instruction packages that extend agent capabilities."));
        assert!(prompt.contains("The prompt includes a user message with:"));
        assert!(prompt.contains("<available-skills>"));
        assert!(prompt.contains("- name: sqlite"));
        assert!(prompt.contains("when_to_use: Use when you need structured data queries."));
        assert!(prompt.contains("</available-skills>"));
        assert!(!prompt.contains("Use this skill for repository inspection."));
        assert!(!prompt.contains("def status() -> str: ..."));
        assert_eq!(task_message.role, "user");
        assert!(task_message.content.contains("<task>"));
        assert!(task_message.content.contains("Help me inspect the repo"));
        assert!(task_message.content.contains("<declarations>"));
        assert!(task_message.content.contains("def status() -> str: ..."));
    }
}
