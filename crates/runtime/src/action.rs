use std::error::Error;
use std::fmt::{Display, Formatter};
use std::sync::Arc;

use anyhow::anyhow;
use async_trait::async_trait;
use pera_core::{ActionId, ActionRequest, ActionResult, CanonicalValue, RunId};
use tokio::sync::mpsc;
use wasmtime::component::{Linker, ResourceTable, Val};
use wasmtime::{Config, Engine, Store};
use wasmtime_wasi::p2::{IoView, WasiCtx, WasiCtxBuilder, WasiView};

use crate::capabilities::SqliteCapabilityProvider;
use crate::catalog::SkillRuntime;

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
    async fn handle(&self, action: &ActionRequest) -> Result<CanonicalValue, ActionProcessorError>;
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
    async fn handle(&self, action: &ActionRequest) -> Result<CanonicalValue, ActionProcessorError> {
        Err(ActionProcessorError::new(format!(
            "no action processor is configured for '{}'",
            action.invocation.action_name.as_str()
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
    pub fn new(runtime: SkillRuntime) -> Result<Self, ActionProcessorError> {
        let mut config = Config::new();
        config.wasm_component_model(true);
        let engine =
            Engine::new(&config).map_err(|error| ActionProcessorError::new(error.to_string()))?;
        Ok(Self { runtime, engine })
    }

    fn execute_sync(&self, action: &ActionRequest) -> Result<CanonicalValue, ActionProcessorError> {
        let action_definition = self.resolve_action_definition(action)?;
        let skill = self.resolve_skill(action)?;
        let wasm_invocation = self
            .runtime
            .catalog()
            .wasmtime_adapter()
            .canonical_invocation_to_wasmtime_invocation(
                &action.skill.skill_name,
                &action.invocation,
            )
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

        let mut results = match &action_definition.result {
            pera_canonical::CanonicalFunctionResult::None => Vec::new(),
            _ => vec![Val::Bool(false)],
        };
        func.call(&mut store, &wasm_invocation.arguments, &mut results)
            .map_err(|error| ActionProcessorError::new(error.to_string()))?;
        func.post_return(&mut store)
            .map_err(|error| ActionProcessorError::new(error.to_string()))?;

        let result_val = match results.as_slice() {
            [] => Val::Option(None),
            [value] => value.clone(),
            _ => Val::Tuple(results),
        };
        self.runtime
            .catalog()
            .wasmtime_adapter()
            .wasmtime_value_to_canonical_value(
                &wasm_invocation.locator.canonical_action_id,
                &result_val,
            )
            .map_err(|error| ActionProcessorError::new(error.to_string()))
    }

    fn resolve_action_definition(
        &self,
        action: &ActionRequest,
    ) -> Result<pera_canonical::ActionDefinition, ActionProcessorError> {
        let canonical_action_id = format!(
            "{}.{}",
            action.skill.skill_name,
            action.invocation.action_name.as_str()
        );
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
                            move |_store,
                                  (sql, params_json): (String, Option<String>)|
                                  -> Result<(String,), anyhow::Error> {
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
