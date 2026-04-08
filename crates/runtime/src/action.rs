use std::error::Error;
use std::fmt::{Display, Formatter};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use std::time::Instant;

use async_trait::async_trait;
use pera_core::{ActionId, ActionRequest, ActionResult, CanonicalValue, RunId};
use tokio::sync::mpsc;
use tokio::time;
use tracing::{debug, error, trace};
use wasmtime::component::Val;

use crate::catalog::{
    InvocationContext, InvocationErrorSource, InvocationEventSource, SkillRuntime, WarmInstance,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionProcessorError {
    message: String,
}

impl ActionProcessorError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl Display for ActionProcessorError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl Error for ActionProcessorError {}

impl From<anyhow::Error> for ActionProcessorError {
    fn from(value: anyhow::Error) -> Self {
        Self::new(value.to_string())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActionExecutionUpdate {
    Claimed {
        run_id: RunId,
        action_id: ActionId,
        skill_name: String,
        action_name: String,
        worker_id: String,
    },
    Completed(ActionResult),
    Failed {
        run_id: RunId,
        action_id: ActionId,
        skill_name: String,
        action_name: String,
        message: String,
    },
}

#[async_trait]
pub trait ActionExecutor: Clone + Send + Sync + 'static {
    async fn execute(&self, action: ActionRequest) -> ActionExecutionUpdate;
}

#[derive(Debug)]
pub(crate) struct ActionWorker<A> {
    worker_id: String,
    action_executor: A,
    action_rx: mpsc::UnboundedReceiver<ActionRequest>,
    update_tx: mpsc::UnboundedSender<ActionExecutionUpdate>,
    execution_timeout: Duration,
}

impl<A> ActionWorker<A>
where
    A: ActionExecutor,
{
    const DEFAULT_EXECUTION_TIMEOUT: Duration = Duration::from_secs(30);

    pub(crate) fn new(
        worker_id: impl Into<String>,
        action_executor: A,
        action_rx: mpsc::UnboundedReceiver<ActionRequest>,
        update_tx: mpsc::UnboundedSender<ActionExecutionUpdate>,
    ) -> Self {
        Self::new_with_timeout(
            worker_id,
            action_executor,
            action_rx,
            update_tx,
            Self::DEFAULT_EXECUTION_TIMEOUT,
        )
    }

    pub(crate) fn new_with_timeout(
        worker_id: impl Into<String>,
        action_executor: A,
        action_rx: mpsc::UnboundedReceiver<ActionRequest>,
        update_tx: mpsc::UnboundedSender<ActionExecutionUpdate>,
        execution_timeout: Duration,
    ) -> Self {
        Self {
            worker_id: worker_id.into(),
            action_executor,
            action_rx,
            update_tx,
            execution_timeout,
        }
    }

    pub(crate) async fn run(mut self) {
        while let Some(action) = self.action_rx.recv().await {
            let _ = self.update_tx.send(ActionExecutionUpdate::Claimed {
                run_id: action.run_id,
                action_id: action.id,
                skill_name: action.skill.skill_name.clone(),
                action_name: action.invocation.action_name.as_str().to_owned(),
                worker_id: self.worker_id.clone(),
            });

            debug!(
                run_id = %action.run_id,
                action_id = %action.id,
                worker_id = %self.worker_id,
                "worker executing action",
            );
            let run_id = action.run_id;
            let action_id = action.id;
            let skill_name = action.skill.skill_name.clone();
            let action_name = action.invocation.action_name.as_str().to_owned();
            let update = match time::timeout(self.execution_timeout, self.action_executor.execute(action)).await {
                Ok(update) => update,
                Err(_) => ActionExecutionUpdate::Failed {
                    run_id,
                    action_id,
                    skill_name,
                    action_name,
                    message: format!(
                        "action execution timed out after {}s",
                        self.execution_timeout.as_secs()
                    ),
                },
            };
            debug!(
                worker_id = %self.worker_id,
                update_kind = match &update {
                    ActionExecutionUpdate::Claimed { .. } => "claimed",
                    ActionExecutionUpdate::Completed(_) => "completed",
                    ActionExecutionUpdate::Failed { .. } => "failed",
                },
                "worker produced update",
            );
            let _ = self.update_tx.send(update);
            trace!(worker_id = %self.worker_id, "worker sent update");
        }
    }
}

#[derive(Clone)]
pub struct WasmtimeComponentActionExecutor {
    runtime: Arc<SkillRuntime>,
}

impl WasmtimeComponentActionExecutor {
    pub fn new(runtime: SkillRuntime) -> Result<Self, ActionProcessorError> {
        Ok(Self {
            runtime: Arc::new(runtime),
        })
    }

    fn execute_sync(
        runtime: &SkillRuntime,
        action: &ActionRequest,
        instance: Arc<Mutex<WarmInstance>>,
    ) -> Result<CanonicalValue, ActionProcessorError> {
        let started_at = Instant::now();
        let canonical_action_id = format!(
            "{}.{}",
            action.skill.skill_name,
            action.invocation.action_name.as_str()
        );
        let action_definition = runtime
            .catalog()
            .action_registry()
            .resolve_canonical_action(&canonical_action_id)
            .cloned()
            .ok_or_else(|| {
                ActionProcessorError::new(format!(
                    "unknown canonical action '{}'",
                    canonical_action_id
                ))
            })?;
        let wasm_invocation = runtime
            .catalog()
            .wasmtime_adapter()
            .canonical_invocation_to_wasmtime_invocation(
                &action.skill.skill_name,
                &action.invocation,
            )
            .map_err(|error| ActionProcessorError::new(error.to_string()))?;

        debug!(
            run_id = %action.run_id,
            action_id = %action.id,
            skill = %action.skill.skill_name,
            export = %wasm_invocation.export_name,
            canonical_action = %wasm_invocation.locator.canonical_action_id,
            "action start",
        );

        let lock_started_at = Instant::now();
        let mut instance = instance
            .lock()
            .map_err(|_| ActionProcessorError::new("warm instance mutex is poisoned"))?;
        trace!(
            run_id = %action.run_id,
            action_id = %action.id,
            elapsed_ms = lock_started_at.elapsed().as_millis(),
            "instance lock acquired",
        );
        let function_export = instance
            .function_exports
            .get(&wasm_invocation.export_name)
            .cloned()
            .ok_or_else(|| {
                ActionProcessorError::new(format!(
                    "component function '{}' was not found",
                    wasm_invocation.export_name
                ))
            })?;
        let wasmtime_instance = instance.instance;
        let lookup_started_at = Instant::now();
        let func = wasmtime_instance
            .get_func(&mut instance.store, &function_export)
            .ok_or_else(|| {
                ActionProcessorError::new(format!(
                    "component function '{}' was not found",
                    wasm_invocation.export_name
                ))
            })?;
        trace!(
            run_id = %action.run_id,
            action_id = %action.id,
            elapsed_ms = lookup_started_at.elapsed().as_millis(),
            "wasmtime function resolved",
        );

        let mut results = match &action_definition.result {
            pera_canonical::CanonicalFunctionResult::None => Vec::new(),
            _ => vec![Val::Bool(false)],
        };
        let is_first_call = instance.invocation_count == 0;
        instance.invocation_count += 1;
        let call_started_at = Instant::now();
        instance.store.data_mut().begin_invocation(InvocationContext::new(
            action.run_id,
            action.id,
            wasm_invocation.locator.canonical_action_id.clone(),
            wasm_invocation.export_name.clone(),
        ));
        func.call(
            &mut instance.store,
            &wasm_invocation.arguments,
            &mut results,
        )
        .map_err(|error| {
            instance.store.data_mut().finish_invocation_failure();
            let invocation = instance.store.data().invocation().clone();
            ActionProcessorError::new(format_component_call_error(
                &invocation,
                &error,
            ))
        })?;
        instance.store.data_mut().finish_invocation_success();
        debug!(
            run_id = %action.run_id,
            action_id = %action.id,
            skill = %action.skill.skill_name,
            export = %wasm_invocation.export_name,
            elapsed_ms = call_started_at.elapsed().as_millis(),
            "wasmtime call completed",
        );
        if is_first_call {
            debug!(
                run_id = %action.run_id,
                action_id = %action.id,
                skill = %action.skill.skill_name,
                export = %wasm_invocation.export_name,
                elapsed_ms = call_started_at.elapsed().as_millis(),
                "first invocation on warm instance completed",
            );
        }
        let result_val = match results.as_slice() {
            [] => Val::Option(None),
            [value] => value.clone(),
            _ => Val::Tuple(results),
        };
        let decode_started_at = Instant::now();
        let canonical_value = runtime
            .catalog()
            .wasmtime_adapter()
            .wasmtime_value_to_canonical_value(
                &wasm_invocation.locator.canonical_action_id,
                &result_val,
            )
            .map_err(|error| {
                ActionProcessorError::new(format!(
                    "failed to decode component result for '{}': {error}",
                    wasm_invocation.locator.canonical_action_id
                ))
            })?;
        trace!(
            run_id = %action.run_id,
            action_id = %action.id,
            elapsed_ms = decode_started_at.elapsed().as_millis(),
            "wasmtime result decoded",
        );
        debug!(
            run_id = %action.run_id,
            action_id = %action.id,
            skill = %action.skill.skill_name,
            export = %wasm_invocation.export_name,
            elapsed_ms = started_at.elapsed().as_millis(),
            "action complete",
        );
        Ok(canonical_value)
    }
}

fn format_component_call_error(
    invocation: &InvocationContext,
    error: &impl std::fmt::Display,
) -> String {
    let invocation_error = invocation
        .error
        .as_ref()
        .map(|error| format!("{} failed: {}", format_error_source(&error.source), error.message));
    let event_summary = if invocation.events.is_empty() {
        None
    } else {
        Some(
            invocation
                .events
                .iter()
                .map(|event| format!("{}: {}", format_event_source(&event.source), event.message))
                .collect::<Vec<_>>()
                .join("\n"),
        )
    };

    let mut message = String::new();
    match invocation_error {
        Some(invocation_error) => {
            message.push_str(&format!(
                "component call failed for '{}': {}",
                invocation.canonical_action_id, invocation_error
            ));
        }
        None => {
            message.push_str(&format!(
                "component call failed for '{}': {}",
                invocation.canonical_action_id, error
            ));
        }
    }

    message.push_str(&format!(
        "\n\nInvocation:\nrun_id={}\naction_id={}\nexport={}\nstatus={:?}\nelapsed_ms={}",
        invocation.run_id,
        invocation.action_id,
        invocation.export_name,
        invocation.status,
        invocation.started_at.elapsed().as_millis()
    ));

    if let Some(event_summary) = event_summary {
        message.push_str(&format!("\n\nEvents:\n{}", event_summary));
    }

    message.push_str(&format!("\n\nWasm backtrace:\n{}", error));
    message
}

fn format_event_source(source: &InvocationEventSource) -> String {
    match source {
        InvocationEventSource::Provider { name, operation } => format!("{name} {operation}"),
        InvocationEventSource::Wasi { operation } => format!("wasi {operation}"),
        InvocationEventSource::Runtime { operation } => format!("runtime {operation}"),
        InvocationEventSource::Component => "component".to_owned(),
    }
}

fn format_error_source(source: &InvocationErrorSource) -> String {
    match source {
        InvocationErrorSource::Provider { name, operation } => format!("{name} {operation}"),
        InvocationErrorSource::Wasi { operation } => format!("wasi {operation}"),
        InvocationErrorSource::Runtime { operation } => format!("runtime {operation}"),
        InvocationErrorSource::Component => "component".to_owned(),
    }
}

#[async_trait]
impl ActionExecutor for WasmtimeComponentActionExecutor {
    async fn execute(&self, action: ActionRequest) -> ActionExecutionUpdate {
        let load_instance_started_at = Instant::now();
        let instance = match self.runtime.warm_instance(&action.skill).await {
            Ok(instance) => instance,
            Err(error) => {
                return ActionExecutionUpdate::Failed {
                    run_id: action.run_id,
                    action_id: action.id,
                    skill_name: action.skill.skill_name.clone(),
                    action_name: action.invocation.action_name.as_str().to_owned(),
                    message: error.to_string(),
                };
            }
        };
        trace!(
            run_id = %action.run_id,
            action_id = %action.id,
            skill = %action.skill.skill_name,
            elapsed_ms = load_instance_started_at.elapsed().as_millis(),
            "executor acquired warm instance",
        );
        let run_id = action.run_id;
        let action_id = action.id;
        let action_skill = action.skill.clone();
        let action_name = action.invocation.action_name.as_str().to_owned();
        let runtime = Arc::clone(&self.runtime);
        let runtime_for_task = Arc::clone(&runtime);
        let execute_started_at = Instant::now();
        let result =
            tokio::task::spawn_blocking(move || {
                Self::execute_sync(runtime_for_task.as_ref(), &action, instance)
            })
                .await;
        trace!(
            run_id = %run_id,
            action_id = %action_id,
            join_result = if result.is_ok() { "ok" } else { "join-error" },
            elapsed_ms = execute_started_at.elapsed().as_millis(),
            "action task joined",
        );
        match result {
            Ok(Ok(value)) => {
                debug!(run_id = %run_id, action_id = %action_id, "action executor returning completed");
                ActionExecutionUpdate::Completed(ActionResult { action_id, value })
            }
            Ok(Err(error)) => {
                runtime.evict_warm_instance(&action_skill).await;
                error!(run_id = %run_id, action_id = %action_id, error = %error, "action executor returning failed");
                ActionExecutionUpdate::Failed {
                    run_id,
                    action_id,
                    skill_name: action_skill.skill_name.clone(),
                    action_name: action_name.clone(),
                    message: error.to_string(),
                }
            }
            Err(error) => {
                error!(run_id = %run_id, action_id = %action_id, error = %error, "action executor join failed");
                ActionExecutionUpdate::Failed {
                    run_id,
                    action_id,
                    skill_name: action_skill.skill_name.clone(),
                    action_name,
                    message: error.to_string(),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::time::Duration;

    use super::{ActionExecutionUpdate, ActionExecutor, ActionWorker};
    use async_trait::async_trait;
    use pera_core::{ActionName, ActionRequest, ActionSkillRef, CanonicalInvocation, RunId};
    use tokio::sync::mpsc;

    #[derive(Clone, Copy)]
    struct SlowActionExecutor;

    #[async_trait]
    impl ActionExecutor for SlowActionExecutor {
        async fn execute(&self, _action: ActionRequest) -> ActionExecutionUpdate {
            tokio::time::sleep(Duration::from_millis(50)).await;
            unreachable!("worker timeout should fire before executor completes");
        }
    }

    #[tokio::test]
    async fn action_worker_fails_claimed_actions_that_time_out() {
        let (action_tx, action_rx) = mpsc::unbounded_channel();
        let (update_tx, mut update_rx) = mpsc::unbounded_channel();
        let worker = ActionWorker::new_with_timeout(
            "action-worker-1",
            SlowActionExecutor,
            action_rx,
            update_tx,
            Duration::from_millis(5),
        );
        let worker_task = tokio::spawn(worker.run());

        let request = ActionRequest {
            id: pera_core::ActionId::generate(),
            run_id: RunId::generate(),
            skill: ActionSkillRef {
                skill_name: "calendar-ops".to_owned(),
                skill_version: None,
                profile_name: Some("shared-room-required".to_owned()),
            },
            invocation: CanonicalInvocation {
                action_name: ActionName::new("list-required-meetings"),
                arguments: BTreeMap::new(),
            },
        };
        action_tx.send(request.clone()).unwrap();
        drop(action_tx);

        let claimed = update_rx.recv().await.expect("claimed update");
        assert!(matches!(
            claimed,
            ActionExecutionUpdate::Claimed {
                action_id,
                ref skill_name,
                ..
            } if action_id == request.id && skill_name == "calendar-ops"
        ));

        let failed = update_rx.recv().await.expect("failed update");
        assert!(matches!(
            failed,
            ActionExecutionUpdate::Failed {
                action_id,
                ref skill_name,
                ref action_name,
                ref message,
                ..
            } if action_id == request.id
                && skill_name == "calendar-ops"
                && action_name == "list-required-meetings"
                && message.contains("timed out")
        ));

        worker_task.await.unwrap();
    }
}
