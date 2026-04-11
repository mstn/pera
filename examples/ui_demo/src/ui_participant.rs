use async_trait::async_trait;
use pera_core::CanonicalValue;
use pera_orchestrator::{
    ActionExecution, Participant, ParticipantDecision, ParticipantError, ParticipantId,
    ParticipantInboxEvent, ParticipantInput, ParticipantOutput,
};
use pera_ui::{
    UiActionArgValue, UiActionInvocation, UiActionResultBinding, UiEvent, UiNode, UiNodeId,
    UiPropValue, UiSpec,
};
use serde_json::{Map, Value, json};
use std::collections::VecDeque;

use crate::todo_env::{TodoAction, TodoObservation, TodoOutcome};

#[derive(Debug)]
pub struct UiDemoParticipant {
    spec: UiSpec,
    data_model: Value,
    pending_results: VecDeque<Option<UiActionResultBinding>>,
}

impl UiDemoParticipant {
    pub fn new(spec: UiSpec) -> Self {
        let data_model = spec.initial_data.clone().unwrap_or_else(|| json!({}));
        Self {
            spec,
            data_model,
            pending_results: VecDeque::new(),
        }
    }

    fn sync_from_observation(&mut self, observation: &TodoObservation) {
        let todos = Value::Array(
            observation
                .todos
                .iter()
                .map(|todo| {
                    json!({
                        "id": todo.id,
                        "text": todo.text,
                        "completed": todo.completed,
                    })
                })
                .collect(),
        );
        set_json_path(&mut self.data_model, "/todos", todos);
    }

    fn find_node(&self, node_id: &str) -> Result<&UiNode, ParticipantError> {
        self.spec
            .node(&UiNodeId::new(node_id))
            .ok_or_else(|| ParticipantError::new(format!("ui node '{node_id}' was not found")))
    }

    fn set_bound_value(&mut self, node_id: &str, value: String) -> Result<String, ParticipantError> {
        let node = self.find_node(node_id)?;
        let Some(UiPropValue::Binding { binding }) = node.props.get("value").cloned() else {
            return Err(ParticipantError::new(format!(
                "ui node '{node_id}' does not expose a writable value binding"
            )));
        };
        set_json_path(&mut self.data_model, &binding.path, Value::String(value));
        Ok(self.render())
    }

    fn trigger_click(&mut self, node_id: &str) -> Result<ParticipantDecision<TodoAction>, ParticipantError> {
        let node = self.find_node(node_id)?;
        let handler = node
            .events
            .iter()
            .find(|handler| handler.event == UiEvent::Click)
            .cloned()
            .ok_or_else(|| ParticipantError::new(format!("ui node '{node_id}' has no click handler")))?;

        let action = map_action(&handler.action, &self.data_model)?;
        self.pending_results.push_back(handler.result.clone());

        Ok(ParticipantDecision::Action {
            notification: Some(format!("triggering {}", handler.action.name.as_str())),
            action,
            execution: ActionExecution::Immediate,
        })
    }

    fn apply_action_result(
        &mut self,
        outcome: &TodoOutcome,
        binding: Option<UiActionResultBinding>,
    ) {
        if let Some(binding) = binding {
            set_json_path(
                &mut self.data_model,
                &binding.path,
                canonical_to_json(&outcome.value),
            );
        }
    }

    fn render(&self) -> String {
        let title = self.spec.title.as_deref().unwrap_or("UI Demo");
        let draft = self
            .data_model
            .pointer("/draft")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let selected_id = self
            .data_model
            .pointer("/selected_id")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let todos = self
            .data_model
            .pointer("/todos")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();

        let mut lines = vec![
            format!("{title}"),
            format!("draft: {draft}"),
            format!("selected_id: {selected_id}"),
            "todos:".to_owned(),
        ];

        if todos.is_empty() {
            lines.push("- none".to_owned());
        } else {
            for todo in todos {
                let id = todo.get("id").and_then(Value::as_i64).unwrap_or_default();
                let text = todo.get("text").and_then(Value::as_str).unwrap_or_default();
                let completed = todo
                    .get("completed")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let marker = if completed { "x" } else { " " };
                lines.push(format!("- [{marker}] {id}: {text}"));
            }
        }

        lines.push("commands:".to_owned());
        lines.push("- set draft_input <text>".to_owned());
        lines.push("- set selected_input <todo-id>".to_owned());
        lines.push("- click add_button".to_owned());
        lines.push("- click toggle_button".to_owned());
        lines.push("- click delete_button".to_owned());
        lines.push("- render".to_owned());

        lines.join("\n")
    }

    fn handle_message(
        &mut self,
        content: &str,
    ) -> Result<ParticipantDecision<TodoAction>, ParticipantError> {
        let mut parts = content.splitn(3, ' ');
        let command = parts.next().unwrap_or_default();
        match command {
            "render" => Ok(ParticipantDecision::FinalMessage {
                content: self.render(),
            }),
            "set" => {
                let node_id = parts
                    .next()
                    .ok_or_else(|| ParticipantError::new("usage: set <node_id> <value>"))?;
                let value = parts
                    .next()
                    .ok_or_else(|| ParticipantError::new("usage: set <node_id> <value>"))?;
                Ok(ParticipantDecision::FinalMessage {
                    content: self.set_bound_value(node_id, value.to_owned())?,
                })
            }
            "click" => {
                let node_id = parts
                    .next()
                    .ok_or_else(|| ParticipantError::new("usage: click <node_id>"))?;
                self.trigger_click(node_id)
            }
            _ => Ok(ParticipantDecision::FinalMessage {
                content: format!(
                    "Unknown command '{content}'.\n\n{}",
                    self.render()
                ),
            }),
        }
    }
}

#[async_trait]
impl Participant for UiDemoParticipant {
    type Observation = TodoObservation;
    type Action = TodoAction;
    type Outcome = TodoOutcome;

    fn id(&self) -> ParticipantId {
        ParticipantId::Custom("ui".to_owned())
    }

    async fn respond(
        &mut self,
        input: ParticipantInput<Self::Observation, Self::Action, Self::Outcome>,
        _output: &mut dyn ParticipantOutput<Self::Action, Self::Outcome>,
    ) -> Result<ParticipantDecision<Self::Action>, ParticipantError> {
        self.sync_from_observation(&input.observation);

        for event in &input.inbox {
            match event {
                ParticipantInboxEvent::ActionCompleted { outcome, .. } => {
                    let binding = self.pending_results.pop_front().flatten();
                    self.apply_action_result(outcome, binding);
                    return Ok(ParticipantDecision::FinalMessage {
                        content: self.render(),
                    });
                }
                ParticipantInboxEvent::ActionFailed { error, .. } => {
                    let _ = self.pending_results.pop_front();
                    return Ok(ParticipantDecision::FinalMessage {
                        content: format!("Action failed: {}\n\n{}", error.user_message, self.render()),
                    });
                }
                ParticipantInboxEvent::Message { .. } => {}
                ParticipantInboxEvent::ActionScheduled { .. }
                | ParticipantInboxEvent::Notification { .. } => {}
            }
        }

        if let Some(work_item) = &input.work_item {
            return self.handle_message(&work_item.content);
        }

        Ok(ParticipantDecision::Yield)
    }
}

fn map_action(
    invocation: &UiActionInvocation,
    data_model: &Value,
) -> Result<TodoAction, ParticipantError> {
    match invocation.name.as_str() {
        "add_todo" => Ok(TodoAction::AddTodo {
            text: required_string_arg(invocation, data_model, "text")?,
        }),
        "toggle_todo" => Ok(TodoAction::ToggleTodo {
            id: required_i64_arg(invocation, data_model, "id")?,
        }),
        "delete_todo" => Ok(TodoAction::DeleteTodo {
            id: required_i64_arg(invocation, data_model, "id")?,
        }),
        other => Err(ParticipantError::new(format!(
            "unsupported todo action '{other}'"
        ))),
    }
}

fn required_string_arg(
    invocation: &UiActionInvocation,
    data_model: &Value,
    name: &str,
) -> Result<String, ParticipantError> {
    let value = resolve_arg_value(invocation, data_model, name)?;
    match value {
        CanonicalValue::String(value) => Ok(value),
        other => Err(ParticipantError::new(format!(
            "action arg '{name}' expected string, got {other:?}"
        ))),
    }
}

fn required_i64_arg(
    invocation: &UiActionInvocation,
    data_model: &Value,
    name: &str,
) -> Result<i64, ParticipantError> {
    let value = resolve_arg_value(invocation, data_model, name)?;
    match value {
        CanonicalValue::S64(value) => Ok(value),
        CanonicalValue::S32(value) => Ok(i64::from(value)),
        CanonicalValue::String(value) => value.parse::<i64>().map_err(|error| {
            ParticipantError::new(format!("action arg '{name}' could not parse '{value}' as i64: {error}"))
        }),
        other => Err(ParticipantError::new(format!(
            "action arg '{name}' expected integer-like value, got {other:?}"
        ))),
    }
}

fn resolve_arg_value(
    invocation: &UiActionInvocation,
    data_model: &Value,
    name: &str,
) -> Result<CanonicalValue, ParticipantError> {
    let arg = invocation
        .args
        .get(name)
        .ok_or_else(|| ParticipantError::new(format!("missing action arg '{name}'")))?;
    match arg {
        UiActionArgValue::Literal { value } => Ok(value.clone()),
        UiActionArgValue::Binding { binding } => {
            let value = data_model.pointer(&binding.path).ok_or_else(|| {
                ParticipantError::new(format!("ui binding path '{}' was not found", binding.path))
            })?;
            json_to_canonical(value)
        }
    }
}

fn json_to_canonical(value: &Value) -> Result<CanonicalValue, ParticipantError> {
    match value {
        Value::Null => Ok(CanonicalValue::Null),
        Value::Bool(value) => Ok(CanonicalValue::Bool(*value)),
        Value::Number(value) => value
            .as_i64()
            .map(CanonicalValue::S64)
            .ok_or_else(|| ParticipantError::new("only i64-compatible numeric UI values are supported")),
        Value::String(value) => Ok(CanonicalValue::String(value.clone())),
        Value::Array(items) => items
            .iter()
            .map(json_to_canonical)
            .collect::<Result<Vec<_>, _>>()
            .map(CanonicalValue::List),
        Value::Object(object) => {
            let mut fields = std::collections::BTreeMap::new();
            for (key, value) in object {
                fields.insert(key.clone(), json_to_canonical(value)?);
            }
            Ok(CanonicalValue::Record(fields))
        }
    }
}

fn canonical_to_json(value: &CanonicalValue) -> Value {
    match value {
        CanonicalValue::Null => Value::Null,
        CanonicalValue::Bool(value) => Value::Bool(*value),
        CanonicalValue::S32(value) => json!(value),
        CanonicalValue::S64(value) => json!(value),
        CanonicalValue::U32(value) => json!(value),
        CanonicalValue::U64(value) => json!(value),
        CanonicalValue::String(value) => Value::String(value.clone()),
        CanonicalValue::List(items) | CanonicalValue::Tuple(items) => {
            Value::Array(items.iter().map(canonical_to_json).collect())
        }
        CanonicalValue::Record(fields) => {
            let mut object = Map::new();
            for (key, value) in fields {
                object.insert(key.clone(), canonical_to_json(value));
            }
            Value::Object(object)
        }
        CanonicalValue::EnumCase(value) => Value::String(value.clone()),
    }
}

fn set_json_path(root: &mut Value, path: &str, value: Value) {
    if path.is_empty() || path == "/" {
        *root = value;
        return;
    }

    let segments = path
        .trim_start_matches('/')
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>();

    if segments.is_empty() {
        *root = value;
        return;
    }

    let mut current = root;
    for segment in &segments[..segments.len() - 1] {
        if !current.is_object() {
            *current = Value::Object(Map::new());
        }
        let object = current.as_object_mut().expect("object just created");
        current = object
            .entry((*segment).to_owned())
            .or_insert_with(|| Value::Object(Map::new()));
    }

    let last = segments.last().expect("segments is not empty");
    if !current.is_object() {
        *current = Value::Object(Map::new());
    }
    current
        .as_object_mut()
        .expect("object just created")
        .insert((*last).to_owned(), value);
}
