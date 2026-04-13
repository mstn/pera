use std::sync::Arc;

use pera_canonical::CanonicalInterface;
use pera_core::StoreError;
use serde_json::{Map, Value, json};
use wasmtime::component::Linker;
use wasmtime::{Error as WasmtimeError, StoreContextMut};

use super::{CapabilityProvider, CapabilityProviderError};
use crate::catalog::{InvocationErrorSource, InvocationEventSource, WasmHostState};

#[derive(Debug, Default)]
pub struct UiCapabilityProvider;

impl UiCapabilityProvider {
    pub fn new() -> Self {
        Self
    }
}

impl CapabilityProvider for UiCapabilityProvider {
    fn capability_name(&self) -> &'static str {
        "ui"
    }
}

pub(crate) fn matches_import(import: &CanonicalInterface) -> bool {
    import.name.contains("ui-builder")
}

impl UiCapabilityProvider {
    pub(crate) fn link_import(
        self: Arc<Self>,
        linker: &mut Linker<WasmHostState>,
        import: &CanonicalInterface,
    ) -> Result<(), StoreError> {
        linker
            .root()
            .instance(&import.name)
            .and_then(|mut instance| {
                let provider = Arc::clone(&self);
                let import_name = import.name.clone();
                instance.func_wrap(
                    "prop",
                    move |mut store: StoreContextMut<'_, WasmHostState>,
                          (name, encoded_value): (String, String)|
                          -> Result<(String,), WasmtimeError> {
                        let result = provider.prop(&name, &encoded_value).map_err(|error| {
                            record_provider_error(&mut store, &import_name, "prop", &error);
                            WasmtimeError::msg(error.to_string())
                        })?;
                        store.data_mut().record_event(
                            InvocationEventSource::Provider {
                                name: import_name.clone(),
                                operation: "prop".to_owned(),
                            },
                            format!("name={name}"),
                        );
                        Ok((result,))
                    },
                )?;

                let provider = Arc::clone(&self);
                let import_name = import.name.clone();
                instance.func_wrap(
                    "handler",
                    move |mut store: StoreContextMut<'_, WasmHostState>,
                          (event, action_name, encoded_action_args, result_path): (
                        String,
                        String,
                        String,
                        Option<String>,
                    )|
                          -> Result<(String,), WasmtimeError> {
                        let result = provider
                            .event(&event, &action_name, &encoded_action_args, result_path.as_deref())
                            .map_err(|error| {
                                record_provider_error(&mut store, &import_name, "handler", &error);
                                WasmtimeError::msg(error.to_string())
                            })?;
                        store.data_mut().record_event(
                            InvocationEventSource::Provider {
                                name: import_name.clone(),
                                operation: "handler".to_owned(),
                            },
                            format!("event={event} action={action_name}"),
                        );
                        Ok((result,))
                    },
                )?;

                let provider = Arc::clone(&self);
                let import_name = import.name.clone();
                instance.func_wrap(
                    "element",
                    move |mut store: StoreContextMut<'_, WasmHostState>,
                          (component, props_json, children_json, handlers_json): (
                        String,
                        Vec<String>,
                        Vec<String>,
                        Vec<String>,
                    )|
                          -> Result<(String,), WasmtimeError> {
                        let result = provider
                            .node(&component, &props_json, &children_json, &handlers_json)
                            .map_err(|error| {
                                record_provider_error(&mut store, &import_name, "element", &error);
                                WasmtimeError::msg(error.to_string())
                            })?;
                        store.data_mut().record_event(
                            InvocationEventSource::Provider {
                                name: import_name.clone(),
                                operation: "element".to_owned(),
                            },
                            format!("component={component}"),
                        );
                        Ok((result,))
                    },
                )?;

                let provider = Arc::clone(&self);
                let import_name = import.name.clone();
                instance.func_wrap(
                    "screen",
                    move |mut store: StoreContextMut<'_, WasmHostState>,
                          (id, title, root_json): (String, Option<String>, String)|
                          -> Result<(String,), WasmtimeError> {
                        let result = provider.spec(&id, title.as_deref(), &root_json).map_err(|error| {
                            record_provider_error(&mut store, &import_name, "screen", &error);
                            WasmtimeError::msg(error.to_string())
                        })?;
                        store.data_mut().record_event(
                            InvocationEventSource::Provider {
                                name: import_name.clone(),
                                operation: "screen".to_owned(),
                            },
                            format!("id={id}"),
                        );
                        Ok((result,))
                    },
                )?;

                Ok(())
            })
            .map_err(|error| StoreError::new(error.to_string()))
    }

    fn prop(&self, name: &str, value_json: &str) -> Result<String, CapabilityProviderError> {
        let value: Value = serde_json::from_str(value_json)?;
        serde_json::to_string(&json!({
            "name": name,
            "value": value,
        }))
        .map_err(Into::into)
    }

    fn event(
        &self,
        event: &str,
        action_name: &str,
        action_args_json: &str,
        result_path: Option<&str>,
    ) -> Result<String, CapabilityProviderError> {
        let action_args: Value = serde_json::from_str(action_args_json)?;
        serde_json::to_string(&json!({
            "event": event,
            "action": {
                "name": action_name,
                "args": action_args,
            },
            "result": result_path.map(|path| json!({ "path": path })),
        }))
        .map_err(Into::into)
    }

    fn node(
        &self,
        component: &str,
        props_json: &[String],
        children_json: &[String],
        events_json: &[String],
    ) -> Result<String, CapabilityProviderError> {
        let mut props = Map::new();
        for prop_json in props_json {
            let prop: Value = serde_json::from_str(prop_json)?;
            let name = prop
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| CapabilityProviderError::new("ui prop is missing 'name'"))?;
            let value = prop
                .get("value")
                .cloned()
                .ok_or_else(|| CapabilityProviderError::new("ui prop is missing 'value'"))?;
            props.insert(name.to_owned(), value);
        }

        let children = children_json
            .iter()
            .map(|value| serde_json::from_str::<Value>(value).map_err(CapabilityProviderError::from))
            .collect::<Result<Vec<_>, _>>()?;
        let events = events_json
            .iter()
            .map(|value| serde_json::from_str::<Value>(value).map_err(CapabilityProviderError::from))
            .collect::<Result<Vec<_>, _>>()?;

        serde_json::to_string(&json!({
            "component": component,
            "props": props,
            "children": children,
            "events": events,
        }))
        .map_err(Into::into)
    }

    fn spec(
        &self,
        id: &str,
        title: Option<&str>,
        root_json: &str,
    ) -> Result<String, CapabilityProviderError> {
        let root: Value = serde_json::from_str(root_json)?;
        let mut nodes = Vec::new();
        let mut next_id = 0usize;
        let root_id = flatten_node(&root, &mut next_id, &mut nodes)?;

        serde_json::to_string(&json!({
            "id": id,
            "version": "1",
            "title": title,
            "root": root_id,
            "nodes": nodes,
        }))
        .map_err(Into::into)
    }
}

fn record_provider_error(
    store: &mut StoreContextMut<'_, WasmHostState>,
    import_name: &str,
    operation: &str,
    error: &CapabilityProviderError,
) {
    store.data_mut().fail(
        InvocationErrorSource::Provider {
            name: import_name.to_owned(),
            operation: operation.to_owned(),
        },
        error.to_string(),
    );
}

fn flatten_node(
    node: &Value,
    next_id: &mut usize,
    nodes: &mut Vec<Value>,
) -> Result<String, CapabilityProviderError> {
    let component = node
        .get("component")
        .and_then(Value::as_str)
        .ok_or_else(|| CapabilityProviderError::new("ui node is missing 'component'"))?;
    let props = node
        .get("props")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let children = node
        .get("children")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let events = node
        .get("events")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    *next_id += 1;
    let node_id = format!("node-{next_id}");
    let mut child_ids = Vec::new();
    for child in children {
        child_ids.push(flatten_node(&child, next_id, nodes)?);
    }

    nodes.push(json!({
        "id": node_id,
        "component": component,
        "props": props,
        "children": child_ids,
        "events": events,
    }));

    Ok(node_id)
}
