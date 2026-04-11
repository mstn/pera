use async_trait::async_trait;
use pera_core::CanonicalValue;
use pera_orchestrator::{
    ActionError, ActionErrorOrigin, Environment, EnvironmentError, EnvironmentEvent,
    LifecycleEvent, ParticipantId, ScheduledAction, TaskSpec,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TodoItem {
    pub id: i64,
    pub text: String,
    pub completed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TodoObservation {
    pub todos: Vec<TodoItem>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TodoAction {
    AddTodo { text: String },
    ToggleTodo { id: i64 },
    DeleteTodo { id: i64 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TodoOutcome {
    pub value: CanonicalValue,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TodoSnapshot {
    todos: Vec<TodoItem>,
    next_id: i64,
}

#[derive(Debug, Default)]
pub struct TodoEnvironment {
    todos: Vec<TodoItem>,
    next_id: i64,
    initialized: bool,
}

impl TodoEnvironment {
    pub fn new() -> Self {
        Self::default()
    }

    fn observation(&self) -> TodoObservation {
        TodoObservation {
            todos: self.todos.clone(),
        }
    }

    fn todos_value(&self) -> CanonicalValue {
        CanonicalValue::List(
            self.todos
                .iter()
                .map(|todo| {
                    CanonicalValue::Record(BTreeMap::from([
                        ("id".to_owned(), CanonicalValue::S64(todo.id)),
                        ("text".to_owned(), CanonicalValue::String(todo.text.clone())),
                        ("completed".to_owned(), CanonicalValue::Bool(todo.completed)),
                    ]))
                })
                .collect(),
        )
    }
}

#[async_trait]
impl Environment for TodoEnvironment {
    type Observation = TodoObservation;
    type Action = TodoAction;
    type Outcome = TodoOutcome;
    type Snapshot = TodoSnapshot;

    async fn reset(&mut self, _task: &TaskSpec) -> Result<Self::Observation, EnvironmentError> {
        if !self.initialized {
            self.todos = vec![
                TodoItem {
                    id: 1,
                    text: "buy milk".to_owned(),
                    completed: false,
                },
                TodoItem {
                    id: 2,
                    text: "write demo notes".to_owned(),
                    completed: true,
                },
            ];
            self.next_id = 3;
            self.initialized = true;
        }
        Ok(self.observation())
    }

    async fn observe(&self) -> Result<Self::Observation, EnvironmentError> {
        Ok(self.observation())
    }

    async fn perform_now(
        &mut self,
        _actor: ParticipantId,
        action: Self::Action,
    ) -> Result<Self::Outcome, EnvironmentError> {
        match action {
            TodoAction::AddTodo { text } => {
                let text = text.trim();
                if text.is_empty() {
                    return Err(EnvironmentError::new("todo text cannot be empty"));
                }
                self.todos.push(TodoItem {
                    id: self.next_id,
                    text: text.to_owned(),
                    completed: false,
                });
                self.next_id += 1;
            }
            TodoAction::ToggleTodo { id } => {
                let todo = self
                    .todos
                    .iter_mut()
                    .find(|todo| todo.id == id)
                    .ok_or_else(|| EnvironmentError::new(format!("todo '{id}' was not found")))?;
                todo.completed = !todo.completed;
            }
            TodoAction::DeleteTodo { id } => {
                let original_len = self.todos.len();
                self.todos.retain(|todo| todo.id != id);
                if self.todos.len() == original_len {
                    return Err(EnvironmentError::new(format!("todo '{id}' was not found")));
                }
            }
        }

        Ok(TodoOutcome {
            value: self.todos_value(),
        })
    }

    async fn schedule(
        &mut self,
        _actor: ParticipantId,
        _action: Self::Action,
    ) -> Result<ScheduledAction, ActionError> {
        Err(ActionError {
            user_message: "The demo environment only supports immediate actions.".to_owned(),
            detail: "deferred execution is not implemented in ui_demo".to_owned(),
            origin: ActionErrorOrigin::Environment,
        })
    }

    async fn poll_events(
        &mut self,
    ) -> Result<Vec<EnvironmentEvent<Self::Action, Self::Outcome>>, EnvironmentError> {
        Ok(Vec::new())
    }

    async fn on_lifecycle_event(
        &mut self,
        _event: LifecycleEvent,
    ) -> Result<(), EnvironmentError> {
        Ok(())
    }

    async fn snapshot(&self) -> Result<Self::Snapshot, EnvironmentError> {
        Ok(TodoSnapshot {
            todos: self.todos.clone(),
            next_id: self.next_id,
        })
    }

    async fn restore(&mut self, snapshot: &Self::Snapshot) -> Result<(), EnvironmentError> {
        self.todos = snapshot.todos.clone();
        self.next_id = snapshot.next_id;
        self.initialized = true;
        Ok(())
    }

    async fn terminal_status(&self) -> Result<Option<String>, EnvironmentError> {
        Ok(None)
    }
}
