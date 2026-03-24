use std::collections::BTreeMap;
use std::error::Error;
use std::fmt::{Display, Formatter};
use std::sync::{Arc, Mutex};

use anyhow::anyhow;
use async_trait::async_trait;
use pera_core::{ActionId, ActionRequest, ActionResult, CanonicalValue, RunId};
use tokio::sync::mpsc;
use tracing::{debug, error, trace};
use wasmtime::component::{Component, ComponentExportIndex, Instance, Linker, ResourceTable, Val};
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

            debug!(
                run_id = %action.run_id,
                action_id = %action.id,
                worker_id = %self.worker_id,
                "worker executing action",
            );
            let update = self.action_executor.execute(action).await;
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

#[derive(Debug, Clone)]
pub struct InProcessActionExecutor<H> {
    handler: H,
}

impl<H> InProcessActionExecutor<H> {
    pub fn new(handler: H) -> Self {
        Self { handler }
    }
}

#[derive(Clone)]
pub struct WasmtimeComponentActionExecutor {
    runtime: Arc<SkillRuntime>,
    engine: Engine,
    warm_instances: Arc<tokio::sync::Mutex<BTreeMap<String, Arc<Mutex<WarmInstance>>>>>,
}

struct WasmHostState {
    table: ResourceTable,
    wasi: WasiCtx,
}

struct WarmInstance {
    store: Store<WasmHostState>,
    instance: Instance,
    function_exports: BTreeMap<String, ComponentExportIndex>,
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
        Ok(Self {
            runtime: Arc::new(runtime),
            engine,
            warm_instances: Arc::new(tokio::sync::Mutex::new(BTreeMap::new())),
        })
    }

    fn execute_sync(
        runtime: &SkillRuntime,
        action: &ActionRequest,
        warm_instance: Arc<Mutex<WarmInstance>>,
    ) -> Result<CanonicalValue, ActionProcessorError> {
        let action_definition = resolve_action_definition(runtime, action)?;
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

        let mut warm_instance = warm_instance
            .lock()
            .map_err(|_| ActionProcessorError::new("warm instance mutex is poisoned"))?;
        let function_export = warm_instance
            .function_exports
            .get(&wasm_invocation.export_name)
            .cloned()
            .ok_or_else(|| {
                ActionProcessorError::new(format!(
                    "component function '{}' was not found",
                    wasm_invocation.export_name
                ))
            })?;
        let instance = warm_instance.instance;
        let func = instance
            .get_func(&mut warm_instance.store, &function_export)
            .ok_or_else(|| {
                ActionProcessorError::new(format!(
                    "component function '{}' was not found",
                    wasm_invocation.export_name
                ))
            })?;

        let mut results = match &action_definition.result {
            pera_canonical::CanonicalFunctionResult::None => Vec::new(),
            _ => vec![Val::Bool(false)],
        };
        func.call(
            &mut warm_instance.store,
            &wasm_invocation.arguments,
            &mut results,
        )
        .map_err(|error| {
            ActionProcessorError::new(format!(
                "component call failed for '{}': {error}",
                wasm_invocation.locator.canonical_action_id
            ))
        })?;
        func.post_return(&mut warm_instance.store)
            .map_err(|error| {
                ActionProcessorError::new(format!(
                    "component post-return failed for '{}': {error}",
                    wasm_invocation.locator.canonical_action_id
                ))
            })?;

        let result_val = match results.as_slice() {
            [] => Val::Option(None),
            [value] => value.clone(),
            _ => Val::Tuple(results),
        };
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
        debug!(
            run_id = %action.run_id,
            action_id = %action.id,
            skill = %action.skill.skill_name,
            export = %wasm_invocation.export_name,
            "action complete",
        );
        Ok(canonical_value)
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
        linker: &mut Linker<WasmHostState>,
        skill: &pera_canonical::CatalogSkill,
        sqlite_provider: Arc<SqliteCapabilityProvider>,
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
                let sqlite = Arc::clone(&sqlite_provider);
                linker
                    .root()
                    .instance(&import.name)
                    .and_then(|mut instance| {
                        let sqlite = Arc::clone(&sqlite);
                        let import_name = import.name.clone();
                        instance.func_wrap(
                            "execute",
                            move |_store,
                                  (sql, params_json): (String, Option<String>)|
                                  -> Result<(String,), anyhow::Error> {
                                trace!(
                                    import = %import_name,
                                    db = %sqlite.database_path().display(),
                                    sql = ?sql,
                                    params_json = ?params_json,
                                    "sqlite import call",
                                );
                                let result = sqlite.execute(&sql, params_json.as_deref()).map_err(
                                    |error| {
                                        error!(
                                            import = %import_name,
                                            db = %sqlite.database_path().display(),
                                            sql = ?sql,
                                            params_json = ?params_json,
                                            error = %error,
                                            "sqlite import error",
                                        );
                                        anyhow!(error.to_string())
                                    },
                                )?;
                                trace!(
                                    import = %import_name,
                                    db = %sqlite.database_path().display(),
                                    result_json = %result,
                                    "sqlite import ok",
                                );
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

    async fn warm_instance(
        &self,
        action: &ActionRequest,
        component: Arc<Component>,
    ) -> Result<Arc<Mutex<WarmInstance>>, ActionProcessorError> {
        let cache_key = warm_instance_key(&action.skill);
        if let Some(instance) = self.warm_instances.lock().await.get(&cache_key).cloned() {
            return Ok(instance);
        }

        let skill = self.resolve_skill(action)?;
        let sqlite_provider = self
            .runtime
            .sqlite_provider(&action.skill)
            .await
            .map_err(|error| ActionProcessorError::new(error.to_string()))?;
        let engine = self.engine.clone();
        let skill_for_build = skill.clone();
        let component_for_build = Arc::clone(&component);
        let warm_instance = tokio::task::spawn_blocking(move || {
            Self::build_warm_instance(
                &skill_for_build,
                component_for_build,
                sqlite_provider,
                &engine,
            )
        })
        .await
        .map_err(|error| ActionProcessorError::new(error.to_string()))??;

        let warm_instance = Arc::new(Mutex::new(warm_instance));
        let mut cache = self.warm_instances.lock().await;
        Ok(cache
            .entry(cache_key)
            .or_insert_with(|| Arc::clone(&warm_instance))
            .clone())
    }

    fn build_warm_instance(
        skill: &pera_canonical::CatalogSkill,
        component: Arc<Component>,
        sqlite_provider: SqliteCapabilityProvider,
        engine: &Engine,
    ) -> Result<WarmInstance, ActionProcessorError> {
        let mut linker = Linker::new(engine);
        wasmtime_wasi::p2::add_to_linker_sync(&mut linker)
            .map_err(|error| ActionProcessorError::new(error.to_string()))?;
        Self::link_imports(&mut linker, skill, Arc::new(sqlite_provider))?;
        let mut store = Store::new(engine, WasmHostState::new());
        let instance = linker
            .instantiate(&mut store, &component)
            .map_err(|error| ActionProcessorError::new(error.to_string()))?;
        let mut function_exports = BTreeMap::new();
        for export_interface in &skill.world.exports {
            let instance_export = component
                .get_export_index(None, &export_interface.name)
                .ok_or_else(|| {
                    ActionProcessorError::new(format!(
                        "component interface export '{}' was not found",
                        export_interface.name
                    ))
                })?;
            for function in &export_interface.functions {
                let function_export = component
                    .get_export_index(Some(&instance_export), &function.name)
                    .ok_or_else(|| {
                        ActionProcessorError::new(format!(
                            "component export '{}.{}' was not found",
                            export_interface.name, function.name
                        ))
                    })?;
                function_exports.insert(function.name.clone(), function_export);
            }
        }
        Ok(WarmInstance {
            store,
            instance,
            function_exports,
        })
    }
}

#[async_trait]
impl ActionExecutor for WasmtimeComponentActionExecutor {
    async fn execute(&self, action: ActionRequest) -> ActionExecutionUpdate {
        let component = match self
            .runtime
            .load_component(&action.skill, &self.engine)
            .await
        {
            Ok(component) => component,
            Err(error) => {
                return ActionExecutionUpdate::Failed {
                    run_id: action.run_id,
                    action_id: action.id,
                    message: error.to_string(),
                };
            }
        };
        let warm_instance = match self.warm_instance(&action, component).await {
            Ok(instance) => instance,
            Err(error) => {
                return ActionExecutionUpdate::Failed {
                    run_id: action.run_id,
                    action_id: action.id,
                    message: error.to_string(),
                };
            }
        };
        let run_id = action.run_id;
        let action_id = action.id;
        let warm_instance_key = warm_instance_key(&action.skill);
        let warm_instances = Arc::clone(&self.warm_instances);
        let runtime = Arc::clone(&self.runtime);
        let result =
            tokio::task::spawn_blocking(move || {
                Self::execute_sync(runtime.as_ref(), &action, warm_instance)
            })
                .await;
        trace!(
            run_id = %run_id,
            action_id = %action_id,
            join_result = if result.is_ok() { "ok" } else { "join-error" },
            "action task joined",
        );
        match result {
            Ok(Ok(value)) => {
                debug!(run_id = %run_id, action_id = %action_id, "action executor returning completed");
                ActionExecutionUpdate::Completed(ActionResult { action_id, value })
            }
            Ok(Err(error)) => {
                warm_instances.lock().await.remove(&warm_instance_key);
                error!(run_id = %run_id, action_id = %action_id, error = %error, "action executor returning failed");
                ActionExecutionUpdate::Failed {
                    run_id,
                    action_id,
                    message: error.to_string(),
                }
            }
            Err(error) => {
                error!(run_id = %run_id, action_id = %action_id, error = %error, "action executor join failed");
                ActionExecutionUpdate::Failed {
                    run_id,
                    action_id,
                    message: error.to_string(),
                }
            }
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

fn resolve_action_definition(
    runtime: &SkillRuntime,
    action: &ActionRequest,
) -> Result<pera_canonical::ActionDefinition, ActionProcessorError> {
    let canonical_action_id = format!(
        "{}.{}",
        action.skill.skill_name,
        action.invocation.action_name.as_str()
    );
    runtime
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

fn warm_instance_key(skill_ref: &pera_core::ActionSkillRef) -> String {
    format!(
        "{}::{}::{}",
        skill_ref.skill_name,
        skill_ref
            .skill_version
            .as_ref()
            .map(|version| version.as_str())
            .unwrap_or_default(),
        skill_ref.profile_name.as_deref().unwrap_or_default()
    )
}
