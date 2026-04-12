use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use pera_core::CanonicalValue;
use pera_orchestrator::{
    ActionExecution, InitialInboxMessage, Orchestrator, Participant, ParticipantDecision,
    ParticipantError, ParticipantId, ParticipantInboxEvent, ParticipantInput, ParticipantOutput,
    RunLimits, RunRequest, TaskSpec, TerminationCondition,
};
use pera_ui::{
    UiActionArgValue, UiActionInvocation, UiActionResultBinding, UiEvent, UiNode, UiNodeId,
    UiPropValue, UiSpec,
};
use serde_json::{Map, Value, json};
use std::collections::VecDeque;

use super::todo::{TodoAction, TodoEnvironment, TodoObservation, TodoOutcome};
use super::{UiEventRequest, UiSessionSnapshot};

#[derive(Debug)]
struct UiRuntimeState {
    spec: UiSpec,
    data_model: Value,
    pending_results: VecDeque<Option<UiActionResultBinding>>,
    status: String,
}

#[derive(Debug)]
struct TodoUiParticipant {
    runtime: Arc<Mutex<UiRuntimeState>>,
}

impl TodoUiParticipant {
    fn new(runtime: Arc<Mutex<UiRuntimeState>>) -> Self {
        Self { runtime }
    }

    fn sync_from_observation(&self, observation: &TodoObservation) {
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
        let mut runtime = self.runtime.lock().expect("ui runtime mutex poisoned");
        set_json_path(&mut runtime.data_model, "/todos", todos);
    }

    fn find_node(&self, node_id: &str) -> Result<UiNode, ParticipantError> {
        let runtime = self.runtime.lock().expect("ui runtime mutex poisoned");
        runtime
            .spec
            .node(&UiNodeId::new(node_id))
            .cloned()
            .ok_or_else(|| ParticipantError::new(format!("ui node '{node_id}' was not found")))
    }

    fn render(&self) -> String {
        let runtime = self.runtime.lock().expect("ui runtime mutex poisoned");
        render_ui(&runtime.spec, &runtime.data_model)
    }

    fn set_bound_value(
        &self,
        node_id: &str,
        value: String,
    ) -> Result<ParticipantDecision<TodoAction>, ParticipantError> {
        let node = self.find_node(node_id)?;
        let Some(UiPropValue::Binding { binding }) = node.props.get("value").cloned() else {
            return Err(ParticipantError::new(format!(
                "ui node '{node_id}' does not expose a writable value binding"
            )));
        };
        let mut runtime = self.runtime.lock().expect("ui runtime mutex poisoned");
        set_json_path(&mut runtime.data_model, &binding.path, Value::String(value));
        runtime.status = "ready".to_owned();
        Ok(ParticipantDecision::FinalMessage {
            content: render_ui(&runtime.spec, &runtime.data_model),
        })
    }

    fn trigger_click(
        &self,
        node_id: &str,
    ) -> Result<ParticipantDecision<TodoAction>, ParticipantError> {
        let node = self.find_node(node_id)?;
        let handler = node
            .events
            .iter()
            .find(|handler| handler.event == UiEvent::Click)
            .cloned()
            .ok_or_else(|| ParticipantError::new(format!("ui node '{node_id}' has no click handler")))?;

        let action = {
            let mut runtime = self.runtime.lock().expect("ui runtime mutex poisoned");
            let action = map_action(&handler.action, &runtime.data_model)?;
            runtime.pending_results.push_back(handler.result.clone());
            runtime.status = format!("triggering {}", handler.action.name.as_str());
            action
        };

        Ok(ParticipantDecision::Action {
            notification: Some(format!("triggering {}", handler.action.name.as_str())),
            action,
            execution: ActionExecution::Immediate,
        })
    }

    fn apply_action_result(
        &self,
        outcome: &TodoOutcome,
        binding: Option<UiActionResultBinding>,
    ) -> ParticipantDecision<TodoAction> {
        let mut runtime = self.runtime.lock().expect("ui runtime mutex poisoned");
        if let Some(binding) = binding {
            set_json_path(
                &mut runtime.data_model,
                &binding.path,
                canonical_to_json(&outcome.value),
            );
        }
        runtime.status = "ready".to_owned();
        ParticipantDecision::FinalMessage {
            content: render_ui(&runtime.spec, &runtime.data_model),
        }
    }

    fn apply_action_failure(&self, message: &str) -> ParticipantDecision<TodoAction> {
        let mut runtime = self.runtime.lock().expect("ui runtime mutex poisoned");
        let _ = runtime.pending_results.pop_front();
        runtime.status = "error".to_owned();
        ParticipantDecision::FinalMessage {
            content: format!("{message}\n\n{}", render_ui(&runtime.spec, &runtime.data_model)),
        }
    }

    fn handle_command(
        &self,
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
                self.set_bound_value(node_id, value.to_owned())
            }
            "click" => {
                let node_id = parts
                    .next()
                    .ok_or_else(|| ParticipantError::new("usage: click <node_id>"))?;
                self.trigger_click(node_id)
            }
            _ => Ok(ParticipantDecision::FinalMessage {
                content: format!("Unknown command '{content}'.\n\n{}", self.render()),
            }),
        }
    }
}

#[async_trait]
impl Participant for TodoUiParticipant {
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
                    let binding = {
                        let mut runtime = self.runtime.lock().expect("ui runtime mutex poisoned");
                        runtime.pending_results.pop_front().flatten()
                    };
                    return Ok(self.apply_action_result(outcome, binding));
                }
                ParticipantInboxEvent::ActionFailed { error, .. } => {
                    return Ok(self.apply_action_failure(&format!(
                        "Action failed: {}",
                        error.user_message
                    )));
                }
                ParticipantInboxEvent::Message { .. }
                | ParticipantInboxEvent::ActionScheduled { .. }
                | ParticipantInboxEvent::Notification { .. } => {}
            }
        }

        if let Some(work_item) = &input.work_item {
            return self.handle_command(&work_item.content);
        }

        Ok(ParticipantDecision::Yield)
    }
}

pub struct UiSessionRunner {
    session_id: String,
    runtime: Arc<Mutex<UiRuntimeState>>,
    orchestrator: Orchestrator<TodoEnvironment, pera_orchestrator::NoopEvaluator>,
}

impl UiSessionRunner {
    pub async fn new(session_id: String, spec: UiSpec, state: Value) -> Result<Self, String> {
        let initial_state = merge_initial_state(spec.initial_data.clone(), state);
        let runtime = Arc::new(Mutex::new(UiRuntimeState {
            spec: spec.clone(),
            data_model: initial_state,
            pending_results: VecDeque::new(),
            status: "ready".to_owned(),
        }));
        let participant = TodoUiParticipant::new(Arc::clone(&runtime));
        let environment = TodoEnvironment::new();
        let mut orchestrator = Orchestrator::new(participant, environment);
        let _ = orchestrator
            .run(run_request("render"))
            .await
            .map_err(|error| format!("failed to initialize ui session: {error}"))?;

        Ok(Self {
            session_id,
            runtime,
            orchestrator,
        })
    }

    pub fn snapshot(&self) -> UiSessionSnapshot {
        let runtime = self.runtime.lock().expect("ui runtime mutex poisoned");
        UiSessionSnapshot {
            session_id: self.session_id.clone(),
            spec: runtime.spec.clone(),
            state: runtime.data_model.clone(),
            status: runtime.status.clone(),
        }
    }

    pub async fn handle_event(&mut self, event: UiEventRequest) -> Result<UiSessionSnapshot, String> {
        let command = event_to_command(&event)?;
        let result = self
            .orchestrator
            .run(run_request(&command))
            .await
            .map_err(|error| format!("failed to run ui session: {error}"))?;

        if let Some(status) = status_from_result(&result) {
            let mut runtime = self.runtime.lock().expect("ui runtime mutex poisoned");
            runtime.status = status;
        }

        Ok(self.snapshot())
    }
}

fn render_ui(spec: &UiSpec, data_model: &Value) -> String {
    let title = spec.title.as_deref().unwrap_or("UI Session");
    let draft = data_model
        .pointer("/draft")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let selected_id = data_model
        .pointer("/selected_id")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let todos = data_model
        .pointer("/todos")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    let mut lines = vec![
        title.to_owned(),
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

    lines.join("\n")
}

fn run_request(command: &str) -> RunRequest {
    RunRequest {
        task: TaskSpec {
            id: "ui-session".to_owned(),
            instructions: "Run the UI session".to_owned(),
        },
        limits: RunLimits {
            max_steps: 8,
            max_steps_per_agent_loop: 8,
            max_actions: 4,
            max_messages: 8,
            max_failed_actions: None,
            max_consecutive_failed_actions: None,
            max_blocked_action_wait: None,
            max_duration: None,
        },
        termination_condition: TerminationCondition::AnyOfParticipantsCompletedLoop(vec![
            ParticipantId::Custom("ui".to_owned()),
        ]),
        initial_messages: vec![InitialInboxMessage {
            to: ParticipantId::Custom("ui".to_owned()),
            from: ParticipantId::Custom("server".to_owned()),
            content: command.to_owned(),
        }],
    }
}

fn merge_initial_state(spec_initial: Option<Value>, override_state: Value) -> Value {
    match spec_initial {
        Some(spec_initial) => merge_json(spec_initial, override_state),
        None => override_state,
    }
}

fn merge_json(base: Value, overlay: Value) -> Value {
    match (base, overlay) {
        (Value::Object(mut base), Value::Object(overlay)) => {
            for (key, value) in overlay {
                let merged = match base.remove(&key) {
                    Some(base_value) => merge_json(base_value, value),
                    None => value,
                };
                base.insert(key, merged);
            }
            Value::Object(base)
        }
        (_, overlay) => overlay,
    }
}

fn event_to_command(event: &UiEventRequest) -> Result<String, String> {
    match event.event_type.as_str() {
        "render" => Ok("render".to_owned()),
        "set_value" => {
            let node_id = required_payload_string(&event.payload, "node_id")?;
            let value = required_payload_string(&event.payload, "value")?;
            Ok(format!("set {node_id} {value}"))
        }
        "trigger_event" => {
            let node_id = required_payload_string(&event.payload, "node_id")?;
            let event_name = event
                .payload
                .get("event")
                .and_then(Value::as_str)
                .unwrap_or("click");
            match event_name {
                "click" => Ok(format!("click {node_id}")),
                other => Err(format!("unsupported ui event '{other}'")),
            }
        }
        other => Err(format!("unsupported request event_type '{other}'")),
    }
}

fn required_payload_string(payload: &Value, field: &str) -> Result<String, String> {
    payload
        .get(field)
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .ok_or_else(|| format!("request payload is missing string field '{field}'"))
}

fn status_from_result(
    result: &pera_orchestrator::RunResult<TodoObservation, TodoAction, TodoOutcome>,
) -> Option<String> {
    match &result.finish_reason {
        pera_orchestrator::FinishReason::ParticipantCompletedLoop { .. } => None,
        other => Some(format!("{other:?}")),
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
