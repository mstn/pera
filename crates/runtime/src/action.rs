use std::error::Error;
use std::fmt::{Display, Formatter};
use std::sync::Arc;

use anyhow::anyhow;
use async_trait::async_trait;
use pera_canonical::WasmValue;
use pera_core::{ActionId, ActionRequest, ActionResult, RunId, Value};
use tokio::sync::mpsc;
use wasmtime::component::{Linker, ResourceTable, Val};
use wasmtime::{Config, Engine, Store};
use wasmtime_wasi::p2::{IoView, WasiCtx, WasiCtxBuilder, WasiView};

use crate::catalog::SkillRuntime;
use crate::capabilities::SqliteCapabilityProvider;

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

#[async_trait]
pub trait ActionHandler: Send + Sync + 'static {
    async fn handle(&self, action: &ActionRequest) -> Result<Value, ActionProcessorError>;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct RejectingActionHandler;

impl RejectingActionHandler {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl ActionHandler for RejectingActionHandler {
    async fn handle(&self, action: &ActionRequest) -> Result<Value, ActionProcessorError> {
        Err(ActionProcessorError::new(format!(
            "no action processor is configured for '{}'",
            action.action_name.as_str()
        )))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActionExecutionUpdate {
    Claimed {
        run_id: RunId,
        action_id: ActionId,
        worker_id: String,
    },
    Completed(ActionResult),
    Failed {
        run_id: RunId,
        action_id: ActionId,
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
}

impl<A> ActionWorker<A>
where
    A: ActionExecutor,
{
    pub(crate) fn new(
        worker_id: impl Into<String>,
        action_executor: A,
        action_rx: mpsc::UnboundedReceiver<ActionRequest>,
        update_tx: mpsc::UnboundedSender<ActionExecutionUpdate>,
    ) -> Self {
        Self {
            worker_id: worker_id.into(),
            action_executor,
            action_rx,
            update_tx,
        }
    }

    pub(crate) async fn run(mut self) {
        while let Some(action) = self.action_rx.recv().await {
            let _ = self.update_tx.send(ActionExecutionUpdate::Claimed {
                run_id: action.run_id,
                action_id: action.id,
                worker_id: self.worker_id.clone(),
            });

            let update = self.action_executor.execute(action).await;
            let _ = self.update_tx.send(update);
        }
    }
}

#[derive(Debug, Clone)]
pub struct InProcessActionExecutor<H> {
    handler: H,
}

impl<H> InProcessActionExecutor<H> {
    pub fn new(handler: H) -> Self {
        Self { handler }
    }
}

#[derive(Debug, Clone)]
pub struct WasmtimeComponentActionExecutor {
    runtime: SkillRuntime,
    engine: Engine,
}

struct WasmHostState {
    table: ResourceTable,
    wasi: WasiCtx,
}

impl WasmHostState {
    fn new() -> Self {
        let wasi = WasiCtxBuilder::new()
            .inherit_stdout()
            .inherit_stderr()
            .build();
        Self {
            table: ResourceTable::new(),
            wasi,
        }
    }
}

impl IoView for WasmHostState {
    fn table(&mut self) -> &mut ResourceTable {
        &mut self.table
    }
}

impl WasiView for WasmHostState {
    fn ctx(&mut self) -> &mut WasiCtx {
        &mut self.wasi
    }
}

impl WasmtimeComponentActionExecutor {
    pub fn new(
        runtime: SkillRuntime,
    ) -> Result<Self, ActionProcessorError> {
        let mut config = Config::new();
        config.wasm_component_model(true);
        let engine = Engine::new(&config)
            .map_err(|error| ActionProcessorError::new(error.to_string()))?;
        Ok(Self {
            runtime,
            engine,
        })
    }

    fn execute_sync(&self, action: &ActionRequest) -> Result<Value, ActionProcessorError> {
        let action_definition = self.resolve_action_definition(action)?;
        let skill = self.resolve_skill(action)?;
        let wasm_invocation = self
            .runtime
            .catalog()
            .wasm_adapter()
            .lower_action_request(action)
            .map_err(|error| ActionProcessorError::new(error.to_string()))?;
        let component = self
            .runtime
            .load_component(&action.skill, &self.engine)
            .map_err(|error| ActionProcessorError::new(error.to_string()))?;

        let mut linker = Linker::new(&self.engine);
        wasmtime_wasi::p2::add_to_linker_sync(&mut linker)
            .map_err(|error| ActionProcessorError::new(error.to_string()))?;
        self.link_imports(&mut linker, &skill, action)?;
        let mut store = Store::new(&self.engine, WasmHostState::new());
        let instance = linker
            .instantiate(&mut store, &component)
            .map_err(|error| ActionProcessorError::new(error.to_string()))?;
        let export_interface = skill
            .world
            .exports
            .iter()
            .find(|interface| {
                interface
                    .functions
                    .iter()
                    .any(|function| function.name == wasm_invocation.export_name)
            })
            .ok_or_else(|| {
                ActionProcessorError::new(format!(
                    "component export interface for '{}' was not found",
                    wasm_invocation.export_name
                ))
            })?;
        let instance_export = component
            .get_export_index(None, &export_interface.name)
            .ok_or_else(|| {
                ActionProcessorError::new(format!(
                    "component interface export '{}' was not found",
                    export_interface.name
                ))
            })?;
        let function_export = component
            .get_export_index(Some(&instance_export), &wasm_invocation.export_name)
            .ok_or_else(|| {
                ActionProcessorError::new(format!(
                    "component export '{}.{}' was not found",
                    export_interface.name, wasm_invocation.export_name
                ))
            })?;
        let func = instance
            .get_func(&mut store, &function_export)
            .ok_or_else(|| {
                ActionProcessorError::new(format!(
                    "component function '{}.{}' was not found",
                    export_interface.name, wasm_invocation.export_name
                ))
            })?;

        let params = wasm_invocation
            .arguments
            .iter()
            .map(component_val_from_wasm_value)
            .collect::<Result<Vec<_>, _>>()?;
        let mut results = result_slots(&action_definition.result);
        func.call(&mut store, &params, &mut results)
            .map_err(|error| ActionProcessorError::new(error.to_string()))?;
        func.post_return(&mut store)
            .map_err(|error| ActionProcessorError::new(error.to_string()))?;

        let result_val = canonical_result_to_component_result(&results)?;
        let canonical_value = self
            .runtime
            .catalog()
            .wasm_adapter()
            .lift_result(&wasm_invocation.locator.canonical_action_id, &result_val)
            .map_err(|error| ActionProcessorError::new(error.to_string()))?;
        self.runtime
            .catalog()
            .model_adapter()
            .lift_result(&wasm_invocation.locator.canonical_action_id, &canonical_value)
            .map_err(|error| ActionProcessorError::new(error.to_string()))
    }

    fn resolve_action_definition(
        &self,
        action: &ActionRequest,
    ) -> Result<pera_canonical::ActionDefinition, ActionProcessorError> {
        let canonical_action_id = format!("{}.{}", action.skill.skill_name, action.action_name.as_str());
        self.runtime
            .catalog()
            .action_registry()
            .resolve_canonical_action(&canonical_action_id)
            .cloned()
            .ok_or_else(|| {
                ActionProcessorError::new(format!(
                    "unknown canonical action '{}'",
                    canonical_action_id
                ))
            })
    }

    fn resolve_skill(
        &self,
        action: &ActionRequest,
    ) -> Result<pera_canonical::CatalogSkill, ActionProcessorError> {
        self.runtime
            .resolve_skill(&action.skill)
            .cloned()
            .ok_or_else(|| {
                ActionProcessorError::new(format!(
                    "skill '{}'{}/{} is not available in the catalog",
                    action.skill.skill_name,
                    action
                        .skill
                        .skill_version
                        .as_ref()
                        .map(|version| format!(" version '{}'", version.as_str()))
                        .unwrap_or_default(),
                    action.skill.profile_name.as_deref().unwrap_or("")
                ))
            })
    }

    fn link_imports(
        &self,
        linker: &mut Linker<WasmHostState>,
        skill: &pera_canonical::CatalogSkill,
        action: &ActionRequest,
    ) -> Result<(), ActionProcessorError> {
        for import in &skill.world.imports {
            if import.functions.is_empty() {
                let _ = linker
                    .root()
                    .instance(&import.name)
                    .map_err(|error| ActionProcessorError::new(error.to_string()))?;
                continue;
            }

            if is_sqlite_import(import) {
                let sqlite = Arc::new(self.sqlite_provider_for(action)?);
                linker
                    .root()
                    .instance(&import.name)
                    .and_then(|mut instance| {
                        let sqlite = Arc::clone(&sqlite);
                        instance.func_wrap(
                            "execute",
                            move |_store, (sql, params_json): (String, Option<String>)| -> Result<(String,), anyhow::Error> {
                                let result = sqlite
                                    .execute(&sql, params_json.as_deref())
                                    .map_err(|error| anyhow!(error.to_string()))?;
                                Ok((result,))
                            },
                        )
                    })
                    .map_err(|error| ActionProcessorError::new(error.to_string()))?;
                continue;
            }

            return Err(ActionProcessorError::new(format!(
                "unsupported component import '{}'",
                import.name
            )));
        }
        Ok(())
    }

    fn sqlite_provider_for(
        &self,
        action: &ActionRequest,
    ) -> Result<SqliteCapabilityProvider, ActionProcessorError> {
        self.runtime
            .sqlite_provider(&action.skill)
            .map_err(|error| ActionProcessorError::new(error.to_string()))
    }
}

#[async_trait]
impl ActionExecutor for WasmtimeComponentActionExecutor {
    async fn execute(&self, action: ActionRequest) -> ActionExecutionUpdate {
        match self.execute_sync(&action) {
            Ok(value) => ActionExecutionUpdate::Completed(ActionResult {
                action_id: action.id,
                value,
            }),
            Err(error) => ActionExecutionUpdate::Failed {
                run_id: action.run_id,
                action_id: action.id,
                message: error.to_string(),
            },
        }
    }
}

#[async_trait]
impl<H> ActionExecutor for InProcessActionExecutor<H>
where
    H: ActionHandler + Clone,
{
    async fn execute(&self, action: ActionRequest) -> ActionExecutionUpdate {
        match self.handler.handle(&action).await {
            Ok(value) => ActionExecutionUpdate::Completed(ActionResult {
                action_id: action.id,
                value,
            }),
            Err(error) => ActionExecutionUpdate::Failed {
                run_id: action.run_id,
                action_id: action.id,
                message: error.to_string(),
            },
        }
    }
}

fn is_sqlite_import(import: &pera_canonical::CanonicalInterface) -> bool {
    import
        .functions
        .iter()
        .any(|function| function.name == "execute")
        && import.name.contains("sqlite")
}

fn result_slots(result: &pera_canonical::CanonicalFunctionResult) -> Vec<Val> {
    match result {
        pera_canonical::CanonicalFunctionResult::None => Vec::new(),
        _ => vec![Val::Bool(false)],
    }
}

fn canonical_result_to_component_result(results: &[Val]) -> Result<WasmValue, ActionProcessorError> {
    match results {
        [] => Ok(WasmValue::Option(Box::new(None))),
        [value] => component_val_to_wasm_value(value),
        _ => results
            .iter()
            .map(component_val_to_wasm_value)
            .collect::<Result<Vec<_>, _>>()
            .map(WasmValue::Tuple),
    }
}

fn component_val_from_wasm_value(value: &WasmValue) -> Result<Val, ActionProcessorError> {
    match value {
        WasmValue::Bool(value) => Ok(Val::Bool(*value)),
        WasmValue::S32(value) => Ok(Val::S32(*value)),
        WasmValue::S64(value) => Ok(Val::S64(*value)),
        WasmValue::U32(value) => Ok(Val::U32(*value)),
        WasmValue::U64(value) => Ok(Val::U64(*value)),
        WasmValue::String(value) => Ok(Val::String(value.clone().into())),
        WasmValue::List(items) => items
            .iter()
            .map(component_val_from_wasm_value)
            .collect::<Result<Vec<_>, _>>()
            .map(Val::List),
        WasmValue::Record(fields) => fields
            .iter()
            .map(|(name, value)| {
                Ok((name.clone(), component_val_from_wasm_value(value)?))
            })
            .collect::<Result<Vec<_>, ActionProcessorError>>()
            .map(Val::Record),
        WasmValue::EnumCase(case_name) => Ok(Val::Enum(case_name.clone())),
        WasmValue::Tuple(items) => items
            .iter()
            .map(component_val_from_wasm_value)
            .collect::<Result<Vec<_>, _>>()
            .map(Val::Tuple),
        WasmValue::Option(value) => value
            .as_ref()
            .as_ref()
            .map(|value| component_val_from_wasm_value(value).map(Box::new))
            .transpose()
            .map(Val::Option),
    }
}

fn component_val_to_wasm_value(value: &Val) -> Result<WasmValue, ActionProcessorError> {
    match value {
        Val::Bool(value) => Ok(WasmValue::Bool(*value)),
        Val::S32(value) => Ok(WasmValue::S32(*value)),
        Val::S64(value) => Ok(WasmValue::S64(*value)),
        Val::U32(value) => Ok(WasmValue::U32(*value)),
        Val::U64(value) => Ok(WasmValue::U64(*value)),
        Val::String(value) => Ok(WasmValue::String(value.to_string())),
        Val::List(items) => items
            .iter()
            .map(component_val_to_wasm_value)
            .collect::<Result<Vec<_>, _>>()
            .map(WasmValue::List),
        Val::Record(fields) => fields
            .iter()
            .map(|(name, value)| Ok((name.clone(), component_val_to_wasm_value(value)?)))
            .collect::<Result<Vec<_>, ActionProcessorError>>()
            .map(WasmValue::Record),
        Val::Enum(case_name) => Ok(WasmValue::EnumCase(case_name.to_string())),
        Val::Tuple(items) => items
            .iter()
            .map(component_val_to_wasm_value)
            .collect::<Result<Vec<_>, _>>()
            .map(WasmValue::Tuple),
        Val::Option(value) => value
            .as_ref()
            .map(|value| component_val_to_wasm_value(value.as_ref()))
            .transpose()
            .map(|value| WasmValue::Option(Box::new(value))),
        other => Err(ActionProcessorError::new(format!(
            "unsupported component value {other:?}"
        ))),
    }
}
