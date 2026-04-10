use pera_orchestrator::{
    ActionError, ParticipantId, ParticipantInboxEvent, ParticipantInput, TrajectoryEvent,
};
use pera_canonical::render_python_value;
use pera_llm::{LlmToolDefinition, PromptMessage};
use pera_runtime::{WorkspaceAction, WorkspaceObservation, WorkspaceOutcome};
use serde_json::json;
use std::collections::{BTreeSet, VecDeque};

const BASE_SYSTEM_PROMPT: &str = include_str!("prompts/base_system.md");
const SKILLS_SYSTEM_PROMPT: &str = include_str!("prompts/skills_system.md");
const CODE_GENERATION_SYSTEM_PROMPT: &str = include_str!("prompts/code_generation_system.md");

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
        let (transcript, mut history_state) = build_transcript(&input.trajectory.events);
        let inbox = build_inbox(&input.inbox, &mut history_state);

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

        Some(PromptMessage::text("user", content))
    }
}

fn role_for_participant(participant: &ParticipantId) -> String {
    match participant {
        ParticipantId::Agent => "assistant".to_owned(),
        ParticipantId::User => "user".to_owned(),
        ParticipantId::Custom(name) => name.clone(),
    }
}

#[derive(Default)]
struct PromptHistoryState {
    next_tool_call_id: usize,
    pending_unassigned: VecDeque<String>,
    action_ids: std::collections::BTreeMap<pera_core::ActionId, String>,
    seen_action_ids: BTreeSet<pera_core::ActionId>,
}

impl PromptHistoryState {
    fn allocate_call_id(&mut self) -> String {
        let call_id = format!("history-call-{}", self.next_tool_call_id);
        self.next_tool_call_id += 1;
        self.pending_unassigned.push_back(call_id.clone());
        call_id
    }

    fn bind_action_id(&mut self, action_id: &pera_core::ActionId) -> Option<String> {
        let call_id = self.pending_unassigned.pop_front()?;
        self.action_ids.insert(*action_id, call_id.clone());
        self.seen_action_ids.insert(*action_id);
        Some(call_id)
    }

    fn resolve_action_id(&mut self, action_id: &pera_core::ActionId) -> Option<String> {
        self.action_ids
            .get(action_id)
            .cloned()
            .or_else(|| self.pending_unassigned.pop_front())
    }

    fn resolve_or_allocate_action_id(&mut self, action_id: &pera_core::ActionId) -> String {
        self.resolve_action_id(action_id)
            .unwrap_or_else(|| {
                let call_id = format!("history-call-{}", self.next_tool_call_id);
                self.next_tool_call_id += 1;
                call_id
            })
    }

    fn mark_seen_action_id(&mut self, action_id: &pera_core::ActionId) {
        self.seen_action_ids.insert(*action_id);
    }

    fn has_seen_action_id(&self, action_id: &pera_core::ActionId) -> bool {
        self.seen_action_ids.contains(action_id)
    }
}

fn build_transcript(
    events: &[TrajectoryEvent<WorkspaceObservation, WorkspaceAction, WorkspaceOutcome>],
) -> (Vec<PromptMessage>, PromptHistoryState) {
    let mut state = PromptHistoryState::default();
    let mut messages = Vec::new();

    for event in events {
        match event {
            TrajectoryEvent::ParticipantMessage {
                participant,
                content,
            } => messages.push(PromptMessage::text(role_for_participant(participant), content.clone())),
            TrajectoryEvent::ActionRequested {
                participant: ParticipantId::Agent,
                action,
                ..
            } => {
                let call_id = state.allocate_call_id();
                let (name, arguments) = serialize_workspace_action(action);
                messages.push(PromptMessage::tool_call(call_id, name, arguments));
            }
            TrajectoryEvent::ActionScheduled {
                participant: ParticipantId::Agent,
                action_id,
                ..
            } => {
                let _ = state.bind_action_id(action_id);
            }
            TrajectoryEvent::ActionCompleted {
                action_id,
                outcome,
                ..
            } => {
                state.mark_seen_action_id(action_id);
                let call_id = state.resolve_or_allocate_action_id(action_id);
                let (name, output) = serialize_workspace_outcome(outcome);
                messages.push(PromptMessage::tool_result(call_id, name, output));
            }
            TrajectoryEvent::ActionFailed {
                action_id,
                error,
                ..
            } => {
                state.mark_seen_action_id(action_id);
                let call_id = state.resolve_or_allocate_action_id(action_id);
                messages.push(PromptMessage::tool_result(
                    call_id,
                    "action_error",
                    serialize_action_error(error),
                ));
            }
            _ => {}
        }
    }

    (messages, state)
}

fn build_inbox(
    events: &[ParticipantInboxEvent<WorkspaceAction, WorkspaceOutcome>],
    state: &mut PromptHistoryState,
) -> Vec<PromptMessage> {
    let mut messages = Vec::new();
    for event in events {
        match event {
            ParticipantInboxEvent::Message { from, content } => {
                messages.push(PromptMessage::text(role_for_participant(from), content.clone()))
            }
            ParticipantInboxEvent::ActionScheduled { action_id, action } => {
                if state.has_seen_action_id(action_id) {
                    continue;
                }
                let call_id = state.allocate_call_id();
                state.action_ids.insert(*action_id, call_id.clone());
                state.mark_seen_action_id(action_id);
                let (name, arguments) = serialize_workspace_action(action);
                messages.push(PromptMessage::tool_call(call_id, name, arguments));
            }
            ParticipantInboxEvent::ActionCompleted { action_id, outcome } => {
                if state.has_seen_action_id(action_id) {
                    continue;
                }
                state.mark_seen_action_id(action_id);
                let call_id = state.resolve_or_allocate_action_id(action_id);
                let (name, output) = serialize_workspace_outcome(outcome);
                messages.push(PromptMessage::tool_result(call_id, name, output));
            }
            ParticipantInboxEvent::ActionFailed { action_id, error } => {
                if state.has_seen_action_id(action_id) {
                    continue;
                }
                state.mark_seen_action_id(action_id);
                let call_id = state.resolve_or_allocate_action_id(action_id);
                messages.push(PromptMessage::tool_result(
                    call_id,
                    "action_error",
                    serialize_action_error(error),
                ));
            }
            ParticipantInboxEvent::Notification { message } => {
                messages.push(PromptMessage::text("system", message.clone()))
            }
        }
    }
    messages
}

fn serialize_workspace_action(action: &WorkspaceAction) -> (String, serde_json::Value) {
    match action {
        WorkspaceAction::LoadSkill { skill_name } => (
            "load_skill".to_owned(),
            json!({ "skill_name": skill_name }),
        ),
        WorkspaceAction::UnloadSkill { skill_name } => (
            "unload_skill".to_owned(),
            json!({ "skill_name": skill_name }),
        ),
        WorkspaceAction::ExecuteCode { language, source } => (
            "execute_code".to_owned(),
            json!({ "language": language, "source": source }),
        ),
    }
}

fn serialize_workspace_outcome(outcome: &WorkspaceOutcome) -> (String, serde_json::Value) {
    match outcome {
        WorkspaceOutcome::CodeExecuted { language, result } => (
            "code_executed".to_owned(),
            match result {
                Some(result) => {
                    json!({ "language": language, "result": render_python_value(result) })
                }
                None => json!({ "language": language }),
            },
        ),
        WorkspaceOutcome::SkillLoaded { skill_name } => (
            "skill_loaded".to_owned(),
            json!({ "skill_name": skill_name }),
        ),
        WorkspaceOutcome::SkillUnloaded { skill_name } => (
            "skill_unloaded".to_owned(),
            json!({ "skill_name": skill_name }),
        ),
    }
}

fn serialize_action_error(error: &ActionError) -> serde_json::Value {
    json!({
        "user_message": error.user_message,
        "detail": error.detail,
        "origin": format!("{:?}", error.origin),
    })
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use pera_core::{RunId, Value, WorkItemId};
    use pera_orchestrator::{
        ParticipantId, ParticipantInboxEvent, ParticipantInput, RunLimits, TaskSpec, Trajectory,
        TrajectoryEvent,
    };
    use pera_runtime::{
        AgentWorkspaceTool, WorkspaceAction, WorkspaceActiveSkill, WorkspaceAvailableSkill,
        WorkspaceObservation, WorkspaceOutcome,
    };
    use pera_llm::PromptMessageMetadata;
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
                max_blocked_action_wait: None,
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

    #[test]
    fn prompt_messages_include_assistant_handoff_and_code_block() {
        let builder = ProviderBackedPromptBuilder;
        let input = ParticipantInput {
            run_id: RunId::generate(),
            agent_loop_id: WorkItemId::generate(),
            agent_loop_iteration: 2,
            participant: ParticipantId::Agent,
            work_item: Some(pera_orchestrator::WorkItem {
                id: WorkItemId::generate(),
                from: ParticipantId::User,
                content: "Check something".to_owned(),
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
                max_blocked_action_wait: None,
                max_duration: Some(Duration::from_secs(10)),
            },
            observation: WorkspaceObservation {
                available_tools: vec![],
                available_skills: vec![],
                active_skills: vec![],
            },
            inbox: vec![],
            trajectory: Trajectory {
                run_id: RunId::generate(),
                events: vec![
                    TrajectoryEvent::ParticipantMessage {
                        participant: ParticipantId::User,
                        content: "Check something".to_owned(),
                    },
                    TrajectoryEvent::ParticipantMessage {
                        participant: ParticipantId::Agent,
                        content: "Running a quick check.".to_owned(),
                    },
                    TrajectoryEvent::ActionRequested {
                        participant: ParticipantId::Agent,
                        action: pera_runtime::WorkspaceAction::ExecuteCode {
                            language: "python".to_owned(),
                            source: "result = 1 + 1\nresult".to_owned(),
                        },
                        execution: pera_orchestrator::ActionExecution::DeferredBlocking,
                    },
                ],
            },
        };

        let context = builder.build_context(&input);
        assert!(context.transcript.iter().any(|message| message.content == "Running a quick check."));
        assert!(context.transcript.iter().any(|message| matches!(
            &message.metadata,
            Some(PromptMessageMetadata::ToolCall { name, arguments, .. })
                if name == "execute_code"
                    && arguments.get("source").and_then(|value| value.as_str()) == Some("result = 1 + 1\nresult")
        )));
    }

    #[test]
    fn prompt_messages_render_code_execution_results_as_python() {
        let builder = ProviderBackedPromptBuilder;
        let input = ParticipantInput {
            run_id: RunId::generate(),
            agent_loop_id: WorkItemId::generate(),
            agent_loop_iteration: 2,
            participant: ParticipantId::Agent,
            work_item: Some(pera_orchestrator::WorkItem {
                id: WorkItemId::generate(),
                from: ParticipantId::User,
                content: "Check something".to_owned(),
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
                max_blocked_action_wait: None,
                max_duration: Some(Duration::from_secs(10)),
            },
            observation: WorkspaceObservation {
                available_tools: vec![],
                available_skills: vec![],
                active_skills: vec![],
            },
            inbox: vec![ParticipantInboxEvent::ActionCompleted {
                action_id: pera_core::ActionId::generate(),
                outcome: WorkspaceOutcome::CodeExecuted {
                    language: "python".to_owned(),
                    result: Some(Value::List(vec![
                        Value::Record {
                            name: "meeting".to_owned(),
                            fields: std::collections::BTreeMap::from([
                                ("title".to_owned(), Value::String("Delta Review".to_owned())),
                                ("city".to_owned(), Value::String("Berlin".to_owned())),
                            ]),
                        },
                        Value::Bool(true),
                    ])),
                },
            }],
            trajectory: Trajectory {
                run_id: RunId::generate(),
                events: vec![],
            },
        };

        let context = builder.build_context(&input);
        let rendered = context.inbox
            .iter()
            .find(|message| matches!(
                &message.metadata,
                Some(PromptMessageMetadata::ToolResult { name, output, .. })
                    if name == "code_executed"
                        && output.get("result").is_some()
            ))
            .expect("expected code execution tool result");

        match &rendered.metadata {
            Some(PromptMessageMetadata::ToolResult { name, output, .. }) => {
                assert_eq!(name, "code_executed");
                assert_eq!(output.get("language").and_then(|value| value.as_str()), Some("python"));
                let result = output
                    .get("result")
                    .and_then(|value| value.as_str())
                    .expect("missing string result");
                assert!(result.contains("Meeting("));
                assert!(result.contains("Delta Review"));
                assert!(result.contains("Berlin"));
                assert!(result.contains("True"));
            }
            _ => panic!("expected tool result metadata"),
        }
    }

    #[test]
    fn prompt_transcript_persists_code_execution_results_as_system_messages() {
        let builder = ProviderBackedPromptBuilder;
        let input = ParticipantInput {
            run_id: RunId::generate(),
            agent_loop_id: WorkItemId::generate(),
            agent_loop_iteration: 3,
            participant: ParticipantId::Agent,
            work_item: Some(pera_orchestrator::WorkItem {
                id: WorkItemId::generate(),
                from: ParticipantId::User,
                content: "Check something".to_owned(),
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
                max_blocked_action_wait: None,
                max_duration: Some(Duration::from_secs(10)),
            },
            observation: WorkspaceObservation {
                available_tools: vec![],
                available_skills: vec![],
                active_skills: vec![],
            },
            inbox: vec![],
            trajectory: Trajectory {
                run_id: RunId::generate(),
                events: vec![TrajectoryEvent::ActionCompleted {
                    participant: ParticipantId::Agent,
                    action_id: pera_core::ActionId::generate(),
                    outcome: WorkspaceOutcome::CodeExecuted {
                        language: "python".to_owned(),
                        result: Some(Value::List(vec![
                            Value::Int(1),
                            Value::String("two".to_owned()),
                        ])),
                    },
                }],
            },
        };

        let context = builder.build_context(&input);
        let rendered = context.transcript
            .iter()
            .find(|message| matches!(
                &message.metadata,
                Some(PromptMessageMetadata::ToolResult { name, .. }) if name == "code_executed"
            ))
            .expect("expected persisted code execution result in transcript");

        assert_eq!(rendered.role, "tool");
        match &rendered.metadata {
            Some(PromptMessageMetadata::ToolResult { output, .. }) => {
                assert!(output.to_string().contains("\"language\":\"python\""));
                assert!(output.to_string().contains("[1, \\\"two\\\"]"));
            }
            _ => panic!("expected tool result metadata"),
        }
    }

    #[test]
    fn prompt_transcript_persists_non_code_action_completions() {
        let builder = ProviderBackedPromptBuilder;
        let input = ParticipantInput {
            run_id: RunId::generate(),
            agent_loop_id: WorkItemId::generate(),
            agent_loop_iteration: 3,
            participant: ParticipantId::Agent,
            work_item: Some(pera_orchestrator::WorkItem {
                id: WorkItemId::generate(),
                from: ParticipantId::User,
                content: "Check something".to_owned(),
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
                max_blocked_action_wait: None,
                max_duration: Some(Duration::from_secs(10)),
            },
            observation: WorkspaceObservation {
                available_tools: vec![],
                available_skills: vec![],
                active_skills: vec![],
            },
            inbox: vec![],
            trajectory: Trajectory {
                run_id: RunId::generate(),
                events: vec![TrajectoryEvent::ActionCompleted {
                    participant: ParticipantId::Agent,
                    action_id: pera_core::ActionId::generate(),
                    outcome: WorkspaceOutcome::SkillLoaded {
                        skill_name: "travel-policy".to_owned(),
                    },
                }],
            },
        };

        let context = builder.build_context(&input);
        let rendered = context.transcript
            .iter()
            .find(|message| matches!(
                &message.metadata,
                Some(PromptMessageMetadata::ToolResult { name, .. }) if name == "skill_loaded"
            ))
            .expect("expected persisted skill completion in transcript");

        assert_eq!(rendered.role, "tool");
    }

    #[test]
    fn prompt_messages_include_action_failures_as_system_messages() {
        let builder = ProviderBackedPromptBuilder;
        let input = ParticipantInput {
            run_id: RunId::generate(),
            agent_loop_id: WorkItemId::generate(),
            agent_loop_iteration: 2,
            participant: ParticipantId::Agent,
            work_item: Some(pera_orchestrator::WorkItem {
                id: WorkItemId::generate(),
                from: ParticipantId::User,
                content: "Check something".to_owned(),
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
                max_blocked_action_wait: None,
                max_duration: Some(Duration::from_secs(10)),
            },
            observation: WorkspaceObservation {
                available_tools: vec![],
                available_skills: vec![],
                active_skills: vec![],
            },
            inbox: vec![],
            trajectory: Trajectory {
                run_id: RunId::generate(),
                events: vec![TrajectoryEvent::ActionFailed {
                    participant: ParticipantId::Agent,
                    action_id: pera_core::ActionId::generate(),
                    error: pera_orchestrator::ActionError {
                        user_message: "Variable lookup failed".to_owned(),
                        detail: "name_lookup for 'meetings'".to_owned(),
                        origin: pera_orchestrator::ActionErrorOrigin::Interpreter,
                    },
                }],
            },
        };

        let context = builder.build_context(&input);
        let rendered = context.transcript
            .iter()
            .find(|message| matches!(
                &message.metadata,
                Some(PromptMessageMetadata::ToolResult { name, .. }) if name == "action_error"
            ))
            .expect("expected action error tool result in transcript");

        match &rendered.metadata {
            Some(PromptMessageMetadata::ToolResult { output, .. }) => {
                assert!(output.to_string().contains("Variable lookup failed"));
                assert!(output.to_string().contains("name_lookup for 'meetings'"));
            }
            _ => panic!("expected tool result metadata"),
        }
    }

    #[test]
    fn transcript_suppresses_duplicate_inbox_action_completion() {
        let builder = ProviderBackedPromptBuilder;
        let action_id = pera_core::ActionId::generate();
        let input = ParticipantInput {
            run_id: RunId::generate(),
            agent_loop_id: WorkItemId::generate(),
            agent_loop_iteration: 2,
            participant: ParticipantId::Agent,
            work_item: Some(pera_orchestrator::WorkItem {
                id: WorkItemId::generate(),
                from: ParticipantId::User,
                content: "Check something".to_owned(),
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
                max_blocked_action_wait: None,
                max_duration: Some(Duration::from_secs(10)),
            },
            observation: WorkspaceObservation {
                available_tools: vec![],
                available_skills: vec![],
                active_skills: vec![],
            },
            inbox: vec![ParticipantInboxEvent::ActionCompleted {
                action_id,
                outcome: WorkspaceOutcome::SkillLoaded {
                    skill_name: "calendar-ops".to_owned(),
                },
            }],
            trajectory: Trajectory {
                run_id: RunId::generate(),
                events: vec![
                    TrajectoryEvent::ActionRequested {
                        participant: ParticipantId::Agent,
                        action: WorkspaceAction::LoadSkill {
                            skill_name: "calendar-ops".to_owned(),
                        },
                        execution: pera_orchestrator::ActionExecution::Immediate,
                    },
                    TrajectoryEvent::ActionScheduled {
                        participant: ParticipantId::Agent,
                        action_id,
                        action: WorkspaceAction::LoadSkill {
                            skill_name: "calendar-ops".to_owned(),
                        },
                        execution: pera_orchestrator::ActionExecution::Immediate,
                    },
                    TrajectoryEvent::ActionCompleted {
                        participant: ParticipantId::Agent,
                        action_id,
                        outcome: WorkspaceOutcome::SkillLoaded {
                            skill_name: "calendar-ops".to_owned(),
                        },
                    },
                ],
            },
        };

        let context = builder.build_context(&input);

        assert!(context.transcript.iter().any(|message| matches!(
            &message.metadata,
            Some(PromptMessageMetadata::ToolResult { name, .. }) if name == "skill_loaded"
        )));
        assert!(!context.inbox.iter().any(|message| matches!(
            &message.metadata,
            Some(PromptMessageMetadata::ToolResult { name, .. }) if name == "skill_loaded"
        )));
    }
}
