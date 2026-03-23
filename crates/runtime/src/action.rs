use std::error::Error;
use std::fmt::{Display, Formatter};
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::anyhow;
use async_trait::async_trait;
use pera_canonical::{ModelInvocation, SkillCatalog, WasmValue};
use pera_core::{ActionId, ActionRequest, ActionResult, RunId, Value};
use tokio::sync::mpsc;
use wasmtime::component::{Component, Linker, Val};
use wasmtime::{Config, Engine, Store};

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
    root: PathBuf,
    catalog: SkillCatalog,
    engine: Engine,
}

impl WasmtimeComponentActionExecutor {
    pub fn new(
        root: impl Into<PathBuf>,
        catalog: SkillCatalog,
    ) -> Result<Self, ActionProcessorError> {
        let mut config = Config::new();
        config.wasm_component_model(true);
        let engine = Engine::new(&config)
            .map_err(|error| ActionProcessorError::new(error.to_string()))?;
        Ok(Self {
            root: root.into(),
            catalog,
            engine,
        })
    }

    fn execute_sync(&self, action: &ActionRequest) -> Result<Value, ActionProcessorError> {
        let action_definition = self.resolve_action_definition(action)?;
        let skill = self.resolve_skill(action)?;
        let model_invocation = self.model_invocation(action, &action_definition)?;
        let canonical_invocation = self
            .catalog
            .model_adapter()
            .lower_invocation(&model_invocation)
            .map_err(|error| ActionProcessorError::new(error.to_string()))?;
        let wasm_invocation = self
            .catalog
            .wasm_adapter()
            .lower_invocation(&canonical_invocation)
            .map_err(|error| ActionProcessorError::new(error.to_string()))?;

        let component_path = action_definition
            .skill
            .artifact_ref
            .as_ref()
            .map(PathBuf::from)
            .ok_or_else(|| {
                ActionProcessorError::new(format!(
                    "skill '{}' has no compiled artifact reference",
                    action.skill.skill_name
                ))
            })?;
        let component_bytes = fs::read(&component_path).map_err(|error| {
            ActionProcessorError::new(format!(
                "failed to read component {}: {error}",
                component_path.display()
            ))
        })?;
        let component = Component::new(&self.engine, component_bytes)
            .map_err(|error| ActionProcessorError::new(error.to_string()))?;

        let mut linker = Linker::new(&self.engine);
        self.link_imports(&mut linker, &skill, action)?;
        let mut store = Store::new(&self.engine, ());
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
            .catalog
            .wasm_adapter()
            .lift_result(&canonical_invocation.locator.canonical_action_id, &result_val)
            .map_err(|error| ActionProcessorError::new(error.to_string()))?;
        self.catalog
            .model_adapter()
            .lift_result(&canonical_invocation.locator.canonical_action_id, &canonical_value)
            .map_err(|error| ActionProcessorError::new(error.to_string()))
    }

    fn resolve_action_definition(
        &self,
        action: &ActionRequest,
    ) -> Result<pera_canonical::ActionDefinition, ActionProcessorError> {
        let canonical_action_id = format!("{}.{}", action.skill.skill_name, action.action_name.as_str());
        self.catalog
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
        self.catalog
            .resolve_skill(
                &action.skill.skill_name,
                action.skill.skill_version.as_ref().map(|version| version.as_str()),
                action.skill.profile_name.as_deref(),
            )
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

    fn model_invocation(
        &self,
        action: &ActionRequest,
        action_definition: &pera_canonical::ActionDefinition,
    ) -> Result<ModelInvocation, ActionProcessorError> {
        if action.arguments.len() != action_definition.params.len() {
            return Err(ActionProcessorError::new(format!(
                "action '{}' expected {} argument(s) but received {}",
                action_definition.canonical_action_id,
                action_definition.params.len(),
                action.arguments.len()
            )));
        }

        let arguments = action_definition
            .params
            .iter()
            .zip(action.arguments.iter())
            .map(|(param, value)| (param.model_name.clone(), value.clone()))
            .collect();

        Ok(ModelInvocation {
            function_name: action_definition.qualified_model_name.clone(),
            arguments,
        })
    }

    fn link_imports(
        &self,
        linker: &mut Linker<()>,
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
                let sqlite = Arc::new(self.sqlite_provider_for(action, &skill.metadata)?);
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
        metadata: &pera_canonical::SkillMetadata,
    ) -> Result<SqliteCapabilityProvider, ActionProcessorError> {
        let profile_name = action
            .skill
            .profile_name
            .as_deref()
            .or(metadata.profile_name.as_deref())
            .ok_or_else(|| {
                ActionProcessorError::new(format!(
                    "skill '{}' is missing a profile name",
                    action.skill.skill_name
                ))
            })?;
        let skill_version = action
            .skill
            .skill_version
            .as_ref()
            .map(|version| version.as_str().to_owned())
            .or_else(|| metadata.skill_version.clone())
            .ok_or_else(|| {
                ActionProcessorError::new(format!(
                    "skill '{}' is missing a version",
                    action.skill.skill_name
                ))
            })?;
        let manifest_path = self
            .root
            .join("catalog")
            .join("skills")
            .join(&action.skill.skill_name)
            .join(&skill_version)
            .join(profile_name)
            .join("manifest.yaml");
        let manifest_bytes = fs::read(&manifest_path).map_err(|error| {
            ActionProcessorError::new(format!(
                "failed to read {}: {error}",
                manifest_path.display()
            ))
        })?;
        let manifest: pera_core::SkillManifest = serde_yaml::from_slice(&manifest_bytes)
            .map_err(|error| ActionProcessorError::new(error.to_string()))?;
        let sqlite_databases = manifest
            .defaults
            .databases
            .iter()
            .filter(|database| database.engine == "sqlite")
            .collect::<Vec<_>>();
        let database = match sqlite_databases.as_slice() {
            [database] => *database,
            [] => {
                return Err(ActionProcessorError::new(format!(
                    "skill '{}' does not define a sqlite database",
                    action.skill.skill_name
                )))
            }
            _ => {
                return Err(ActionProcessorError::new(format!(
                    "skill '{}' defines multiple sqlite databases; capability mapping is ambiguous",
                    action.skill.skill_name
                )))
            }
        };
        let path = self
            .root
            .join("state")
            .join("skills")
            .join(&action.skill.skill_name)
            .join(skill_version)
            .join(profile_name)
            .join("databases")
            .join(format!("{}.sqlite", database.name));
        SqliteCapabilityProvider::new(path)
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
