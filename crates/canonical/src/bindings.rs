use std::collections::BTreeMap;
use std::error::Error;
use std::fmt::{Display, Formatter};
use std::sync::Arc;

use pera_core::Value;

use crate::ir::{
    CanonicalFunctionResult, CanonicalTypeDef, CanonicalTypeDefKind, CanonicalTypeRef,
    CanonicalWorld,
};
use crate::python::python_function_name;

#[derive(Debug, Clone)]
pub struct CanonicalBindings {
    registry: Arc<ActionRegistry>,
}

impl CanonicalBindings {
    pub fn from_world(world: CanonicalWorld) -> Result<Self, BindingError> {
        let registry = ActionRegistry::from_world(world)?;
        Ok(Self {
            registry: Arc::new(registry),
        })
    }

    pub fn action_registry(&self) -> &ActionRegistry {
        &self.registry
    }

    pub fn model_adapter(&self) -> ModelAdapter {
        ModelAdapter {
            registry: Arc::clone(&self.registry),
        }
    }

    pub fn wasm_adapter(&self) -> WasmAdapter {
        WasmAdapter {
            registry: Arc::clone(&self.registry),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ActionRegistry {
    actions_by_canonical_name: BTreeMap<String, ActionDefinition>,
    actions_by_model_name: BTreeMap<String, String>,
    types: BTreeMap<String, CanonicalTypeDef>,
}

impl ActionRegistry {
    pub fn from_world(world: CanonicalWorld) -> Result<Self, BindingError> {
        let exports = world
            .exports
            .first()
            .ok_or_else(|| BindingError::new("canonical world has no exported interface"))?;

        let mut types = BTreeMap::new();
        for ty in &exports.types {
            types.insert(ty.name.clone(), ty.clone());
        }

        let mut actions_by_canonical_name = BTreeMap::new();
        let mut actions_by_model_name = BTreeMap::new();
        for function in &exports.functions {
            let model_name = python_function_name(&function.name);
            let action = ActionDefinition {
                canonical_name: function.name.clone(),
                model_name: model_name.clone(),
                docs: function.docs.clone(),
                params: function
                    .params
                    .iter()
                    .map(|param| ActionParam {
                        canonical_name: param.name.clone(),
                        model_name: python_function_name(&param.name),
                        ty: param.ty.clone(),
                    })
                    .collect(),
                result: function.result.clone(),
            };

            actions_by_model_name.insert(model_name, function.name.clone());
            actions_by_canonical_name.insert(function.name.clone(), action);
        }

        Ok(Self {
            actions_by_canonical_name,
            actions_by_model_name,
            types,
        })
    }

    pub fn action_by_canonical_name(&self, name: &str) -> Option<&ActionDefinition> {
        self.actions_by_canonical_name.get(name)
    }

    pub fn action_by_model_name(&self, name: &str) -> Option<&ActionDefinition> {
        self.actions_by_model_name
            .get(name)
            .and_then(|canonical_name| self.actions_by_canonical_name.get(canonical_name))
    }

    pub fn type_by_name(&self, name: &str) -> Option<&CanonicalTypeDef> {
        self.types.get(name)
    }
}

#[derive(Debug, Clone)]
pub struct ActionDefinition {
    pub canonical_name: String,
    pub model_name: String,
    pub docs: Option<String>,
    pub params: Vec<ActionParam>,
    pub result: CanonicalFunctionResult,
}

#[derive(Debug, Clone)]
pub struct ActionParam {
    pub canonical_name: String,
    pub model_name: String,
    pub ty: CanonicalTypeRef,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelInvocation {
    pub function_name: String,
    pub arguments: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanonicalInvocation {
    pub action_name: String,
    pub arguments: BTreeMap<String, CanonicalValue>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WasmInvocation {
    pub export_name: String,
    pub arguments: Vec<WasmValue>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CanonicalValue {
    Null,
    Bool(bool),
    S32(i32),
    S64(i64),
    U32(u32),
    U64(u64),
    String(String),
    List(Vec<CanonicalValue>),
    Record(BTreeMap<String, CanonicalValue>),
    EnumCase(String),
    Tuple(Vec<CanonicalValue>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WasmValue {
    Bool(bool),
    S32(i32),
    S64(i64),
    U32(u32),
    U64(u64),
    String(String),
    List(Vec<WasmValue>),
    Record(Vec<(String, WasmValue)>),
    EnumCase(String),
    Tuple(Vec<WasmValue>),
    Option(Box<Option<WasmValue>>),
}

#[derive(Debug, Clone)]
pub struct ModelAdapter {
    registry: Arc<ActionRegistry>,
}

impl ModelAdapter {
    pub fn lower_invocation(
        &self,
        invocation: &ModelInvocation,
    ) -> Result<CanonicalInvocation, BindingError> {
        let action = self
            .registry
            .action_by_model_name(&invocation.function_name)
            .ok_or_else(|| {
                BindingError::new(format!(
                    "unknown model action '{}'",
                    invocation.function_name
                ))
            })?;

        let mut arguments = BTreeMap::new();
        for param in &action.params {
            let model_value = invocation.arguments.get(&param.model_name).ok_or_else(|| {
                BindingError::new(format!(
                    "missing argument '{}' for action '{}'",
                    param.model_name, invocation.function_name
                ))
            })?;
            let canonical = lower_model_value(&self.registry, &param.ty, model_value)?;
            arguments.insert(param.canonical_name.clone(), canonical);
        }

        Ok(CanonicalInvocation {
            action_name: action.canonical_name.clone(),
            arguments,
        })
    }

    pub fn lift_result(
        &self,
        action_name: &str,
        value: &CanonicalValue,
    ) -> Result<Value, BindingError> {
        let action = self
            .registry
            .action_by_canonical_name(action_name)
            .ok_or_else(|| {
                BindingError::new(format!("unknown canonical action '{action_name}'"))
            })?;
        lift_model_result(&self.registry, &action.result, value)
    }
}

#[derive(Debug, Clone)]
pub struct WasmAdapter {
    registry: Arc<ActionRegistry>,
}

impl WasmAdapter {
    pub fn lower_invocation(
        &self,
        invocation: &CanonicalInvocation,
    ) -> Result<WasmInvocation, BindingError> {
        let action = self
            .registry
            .action_by_canonical_name(&invocation.action_name)
            .ok_or_else(|| {
                BindingError::new(format!(
                    "unknown canonical action '{}'",
                    invocation.action_name
                ))
            })?;

        let mut arguments = Vec::with_capacity(action.params.len());
        for param in &action.params {
            let value = invocation
                .arguments
                .get(&param.canonical_name)
                .ok_or_else(|| {
                    BindingError::new(format!(
                        "missing canonical argument '{}' for action '{}'",
                        param.canonical_name, invocation.action_name
                    ))
                })?;
            arguments.push(lower_wasm_value(&self.registry, &param.ty, value)?);
        }

        Ok(WasmInvocation {
            export_name: action.canonical_name.clone(),
            arguments,
        })
    }

    pub fn lift_result(
        &self,
        action_name: &str,
        value: &WasmValue,
    ) -> Result<CanonicalValue, BindingError> {
        let action = self
            .registry
            .action_by_canonical_name(action_name)
            .ok_or_else(|| {
                BindingError::new(format!("unknown canonical action '{action_name}'"))
            })?;
        lift_wasm_result(&self.registry, &action.result, value)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BindingError {
    message: String,
}

impl BindingError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl Display for BindingError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl Error for BindingError {}

fn lower_model_value(
    registry: &ActionRegistry,
    ty: &CanonicalTypeRef,
    value: &Value,
) -> Result<CanonicalValue, BindingError> {
    match ty {
        CanonicalTypeRef::Primitive(crate::CanonicalPrimitiveType::Bool) => match value {
            Value::Bool(value) => Ok(CanonicalValue::Bool(*value)),
            _ => Err(BindingError::new("expected bool")),
        },
        CanonicalTypeRef::Primitive(crate::CanonicalPrimitiveType::S32) => match value {
            Value::Int(value) => i32::try_from(*value)
                .map(CanonicalValue::S32)
                .map_err(|_| BindingError::new("expected signed 32-bit integer")),
            _ => Err(BindingError::new("expected signed 32-bit integer")),
        },
        CanonicalTypeRef::Primitive(crate::CanonicalPrimitiveType::S64)
        | CanonicalTypeRef::Primitive(crate::CanonicalPrimitiveType::S16)
        | CanonicalTypeRef::Primitive(crate::CanonicalPrimitiveType::S8) => match value {
            Value::Int(value) => Ok(CanonicalValue::S64(*value)),
            _ => Err(BindingError::new("expected signed integer")),
        },
        CanonicalTypeRef::Primitive(crate::CanonicalPrimitiveType::U32) => match value {
            Value::Int(value) => u32::try_from(*value)
                .map(CanonicalValue::U32)
                .map_err(|_| BindingError::new("expected unsigned 32-bit integer")),
            _ => Err(BindingError::new("expected unsigned 32-bit integer")),
        },
        CanonicalTypeRef::Primitive(crate::CanonicalPrimitiveType::U64)
        | CanonicalTypeRef::Primitive(crate::CanonicalPrimitiveType::U16)
        | CanonicalTypeRef::Primitive(crate::CanonicalPrimitiveType::U8) => match value {
            Value::Int(value) => u64::try_from(*value)
                .map(CanonicalValue::U64)
                .map_err(|_| BindingError::new("expected unsigned integer")),
            _ => Err(BindingError::new("expected unsigned integer")),
        },
        CanonicalTypeRef::Primitive(crate::CanonicalPrimitiveType::String)
        | CanonicalTypeRef::Primitive(crate::CanonicalPrimitiveType::Char) => match value {
            Value::String(value) => Ok(CanonicalValue::String(value.clone())),
            _ => Err(BindingError::new("expected string")),
        },
        CanonicalTypeRef::Primitive(crate::CanonicalPrimitiveType::Float32)
        | CanonicalTypeRef::Primitive(crate::CanonicalPrimitiveType::Float64) => Err(
            BindingError::new("float values are not supported by the model value surface yet"),
        ),
        CanonicalTypeRef::List(inner) => match value {
            Value::List(items) => items
                .iter()
                .map(|item| lower_model_value(registry, inner, item))
                .collect::<Result<Vec<_>, _>>()
                .map(CanonicalValue::List),
            _ => Err(BindingError::new("expected list")),
        },
        CanonicalTypeRef::Option(inner) => match value {
            Value::Null => Ok(CanonicalValue::Null),
            _ => lower_model_value(registry, inner, value),
        },
        CanonicalTypeRef::Tuple(items) => match value {
            Value::List(values) if values.len() == items.len() => items
                .iter()
                .zip(values.iter())
                .map(|(ty, value)| lower_model_value(registry, ty, value))
                .collect::<Result<Vec<_>, _>>()
                .map(CanonicalValue::Tuple),
            _ => Err(BindingError::new("expected tuple-compatible list")),
        },
        CanonicalTypeRef::Result { .. } => Err(BindingError::new(
            "model result values are not supported as direct inputs",
        )),
        CanonicalTypeRef::Named(name) => lower_named_model_value(registry, name, value),
    }
}

fn lower_named_model_value(
    registry: &ActionRegistry,
    name: &str,
    value: &Value,
) -> Result<CanonicalValue, BindingError> {
    let ty = registry
        .type_by_name(name)
        .ok_or_else(|| BindingError::new(format!("unknown canonical type '{name}'")))?;

    match &ty.kind {
        CanonicalTypeDefKind::Alias(alias) => lower_model_value(registry, alias, value),
        CanonicalTypeDefKind::Enum(cases) => match value {
            Value::String(value) if cases.iter().any(|case| case.name == *value) => {
                Ok(CanonicalValue::EnumCase(value.clone()))
            }
            _ => Err(BindingError::new(format!(
                "expected enum case for type '{name}'"
            ))),
        },
        CanonicalTypeDefKind::Record(record) => match value {
            Value::Map(fields) => {
                let mut lowered = BTreeMap::new();
                for field in &record.fields {
                    let model_name = python_function_name(&field.name);
                    let field_value = fields.get(&model_name).ok_or_else(|| {
                        BindingError::new(format!(
                            "missing field '{}' for record '{}'",
                            model_name, name
                        ))
                    })?;
                    lowered.insert(
                        field.name.clone(),
                        lower_model_value(registry, &field.ty, field_value)?,
                    );
                }
                Ok(CanonicalValue::Record(lowered))
            }
            _ => Err(BindingError::new(format!(
                "expected record value for type '{name}'"
            ))),
        },
        CanonicalTypeDefKind::Variant(_) => Err(BindingError::new(
            "variant model values are not supported yet",
        )),
        CanonicalTypeDefKind::Flags(_) => {
            Err(BindingError::new("flag model values are not supported yet"))
        }
    }
}

fn lift_model_result(
    registry: &ActionRegistry,
    result: &CanonicalFunctionResult,
    value: &CanonicalValue,
) -> Result<Value, BindingError> {
    match result {
        CanonicalFunctionResult::None => Ok(Value::Null),
        CanonicalFunctionResult::Scalar(ty) => lift_model_value(registry, ty, value),
        CanonicalFunctionResult::Named(params) => {
            if params.len() == 1 {
                lift_model_value(registry, &params[0].ty, value)
            } else {
                match value {
                    CanonicalValue::Tuple(items) if items.len() == params.len() => params
                        .iter()
                        .zip(items.iter())
                        .map(|(param, item)| lift_model_value(registry, &param.ty, item))
                        .collect::<Result<Vec<_>, _>>()
                        .map(Value::List),
                    _ => Err(BindingError::new("expected tuple result")),
                }
            }
        }
    }
}

fn lift_model_value(
    registry: &ActionRegistry,
    ty: &CanonicalTypeRef,
    value: &CanonicalValue,
) -> Result<Value, BindingError> {
    match ty {
        CanonicalTypeRef::Primitive(crate::CanonicalPrimitiveType::Bool) => match value {
            CanonicalValue::Bool(value) => Ok(Value::Bool(*value)),
            _ => Err(BindingError::new("expected canonical bool")),
        },
        CanonicalTypeRef::Primitive(crate::CanonicalPrimitiveType::S32) => match value {
            CanonicalValue::S32(value) => Ok(Value::Int((*value).into())),
            _ => Err(BindingError::new("expected canonical s32")),
        },
        CanonicalTypeRef::Primitive(crate::CanonicalPrimitiveType::S64)
        | CanonicalTypeRef::Primitive(crate::CanonicalPrimitiveType::S16)
        | CanonicalTypeRef::Primitive(crate::CanonicalPrimitiveType::S8) => match value {
            CanonicalValue::S64(value) => Ok(Value::Int(*value)),
            CanonicalValue::S32(value) => Ok(Value::Int((*value).into())),
            _ => Err(BindingError::new("expected canonical signed integer")),
        },
        CanonicalTypeRef::Primitive(crate::CanonicalPrimitiveType::U32) => match value {
            CanonicalValue::U32(value) => Ok(Value::Int((*value).into())),
            _ => Err(BindingError::new("expected canonical u32")),
        },
        CanonicalTypeRef::Primitive(crate::CanonicalPrimitiveType::U64)
        | CanonicalTypeRef::Primitive(crate::CanonicalPrimitiveType::U16)
        | CanonicalTypeRef::Primitive(crate::CanonicalPrimitiveType::U8) => match value {
            CanonicalValue::U64(value) => i64::try_from(*value).map(Value::Int).map_err(|_| {
                BindingError::new("unsigned integer does not fit model value surface")
            }),
            CanonicalValue::U32(value) => Ok(Value::Int((*value).into())),
            _ => Err(BindingError::new("expected canonical unsigned integer")),
        },
        CanonicalTypeRef::Primitive(crate::CanonicalPrimitiveType::String)
        | CanonicalTypeRef::Primitive(crate::CanonicalPrimitiveType::Char) => match value {
            CanonicalValue::String(value) => Ok(Value::String(value.clone())),
            _ => Err(BindingError::new("expected canonical string")),
        },
        CanonicalTypeRef::Primitive(crate::CanonicalPrimitiveType::Float32)
        | CanonicalTypeRef::Primitive(crate::CanonicalPrimitiveType::Float64) => Err(
            BindingError::new("float values are not supported by the model value surface yet"),
        ),
        CanonicalTypeRef::List(inner) => match value {
            CanonicalValue::List(items) => items
                .iter()
                .map(|item| lift_model_value(registry, inner, item))
                .collect::<Result<Vec<_>, _>>()
                .map(Value::List),
            _ => Err(BindingError::new("expected canonical list")),
        },
        CanonicalTypeRef::Option(inner) => match value {
            CanonicalValue::Null => Ok(Value::Null),
            _ => lift_model_value(registry, inner, value),
        },
        CanonicalTypeRef::Tuple(items) => match value {
            CanonicalValue::Tuple(values) if values.len() == items.len() => items
                .iter()
                .zip(values.iter())
                .map(|(ty, value)| lift_model_value(registry, ty, value))
                .collect::<Result<Vec<_>, _>>()
                .map(Value::List),
            _ => Err(BindingError::new("expected canonical tuple")),
        },
        CanonicalTypeRef::Result { .. } => Err(BindingError::new(
            "canonical result values are not supported yet",
        )),
        CanonicalTypeRef::Named(name) => lift_named_model_value(registry, name, value),
    }
}

fn lift_named_model_value(
    registry: &ActionRegistry,
    name: &str,
    value: &CanonicalValue,
) -> Result<Value, BindingError> {
    let ty = registry
        .type_by_name(name)
        .ok_or_else(|| BindingError::new(format!("unknown canonical type '{name}'")))?;

    match &ty.kind {
        CanonicalTypeDefKind::Alias(alias) => lift_model_value(registry, alias, value),
        CanonicalTypeDefKind::Enum(_) => match value {
            CanonicalValue::EnumCase(value) => Ok(Value::String(value.clone())),
            _ => Err(BindingError::new(format!(
                "expected canonical enum value for '{name}'"
            ))),
        },
        CanonicalTypeDefKind::Record(record) => match value {
            CanonicalValue::Record(fields) => {
                let mut lifted = BTreeMap::new();
                for field in &record.fields {
                    let field_value = fields.get(&field.name).ok_or_else(|| {
                        BindingError::new(format!(
                            "missing canonical field '{}' for record '{}'",
                            field.name, name
                        ))
                    })?;
                    lifted.insert(
                        python_function_name(&field.name),
                        lift_model_value(registry, &field.ty, field_value)?,
                    );
                }
                Ok(Value::Map(lifted))
            }
            _ => Err(BindingError::new(format!(
                "expected canonical record for '{name}'"
            ))),
        },
        CanonicalTypeDefKind::Variant(_) => Err(BindingError::new(
            "variant model values are not supported yet",
        )),
        CanonicalTypeDefKind::Flags(_) => {
            Err(BindingError::new("flag model values are not supported yet"))
        }
    }
}

fn lower_wasm_value(
    registry: &ActionRegistry,
    ty: &CanonicalTypeRef,
    value: &CanonicalValue,
) -> Result<WasmValue, BindingError> {
    match ty {
        CanonicalTypeRef::Primitive(crate::CanonicalPrimitiveType::Bool) => match value {
            CanonicalValue::Bool(value) => Ok(WasmValue::Bool(*value)),
            _ => Err(BindingError::new("expected canonical bool")),
        },
        CanonicalTypeRef::Primitive(crate::CanonicalPrimitiveType::S32) => match value {
            CanonicalValue::S32(value) => Ok(WasmValue::S32(*value)),
            _ => Err(BindingError::new("expected canonical s32")),
        },
        CanonicalTypeRef::Primitive(crate::CanonicalPrimitiveType::S64)
        | CanonicalTypeRef::Primitive(crate::CanonicalPrimitiveType::S16)
        | CanonicalTypeRef::Primitive(crate::CanonicalPrimitiveType::S8) => match value {
            CanonicalValue::S64(value) => Ok(WasmValue::S64(*value)),
            CanonicalValue::S32(value) => Ok(WasmValue::S64((*value).into())),
            _ => Err(BindingError::new("expected canonical signed integer")),
        },
        CanonicalTypeRef::Primitive(crate::CanonicalPrimitiveType::U32) => match value {
            CanonicalValue::U32(value) => Ok(WasmValue::U32(*value)),
            _ => Err(BindingError::new("expected canonical u32")),
        },
        CanonicalTypeRef::Primitive(crate::CanonicalPrimitiveType::U64)
        | CanonicalTypeRef::Primitive(crate::CanonicalPrimitiveType::U16)
        | CanonicalTypeRef::Primitive(crate::CanonicalPrimitiveType::U8) => match value {
            CanonicalValue::U64(value) => Ok(WasmValue::U64(*value)),
            CanonicalValue::U32(value) => Ok(WasmValue::U64((*value).into())),
            _ => Err(BindingError::new("expected canonical unsigned integer")),
        },
        CanonicalTypeRef::Primitive(crate::CanonicalPrimitiveType::String)
        | CanonicalTypeRef::Primitive(crate::CanonicalPrimitiveType::Char) => match value {
            CanonicalValue::String(value) => Ok(WasmValue::String(value.clone())),
            _ => Err(BindingError::new("expected canonical string")),
        },
        CanonicalTypeRef::Primitive(crate::CanonicalPrimitiveType::Float32)
        | CanonicalTypeRef::Primitive(crate::CanonicalPrimitiveType::Float64) => {
            Err(BindingError::new("float wasm values are not supported yet"))
        }
        CanonicalTypeRef::List(inner) => match value {
            CanonicalValue::List(items) => items
                .iter()
                .map(|item| lower_wasm_value(registry, inner, item))
                .collect::<Result<Vec<_>, _>>()
                .map(WasmValue::List),
            _ => Err(BindingError::new("expected canonical list")),
        },
        CanonicalTypeRef::Option(inner) => match value {
            CanonicalValue::Null => Ok(WasmValue::Option(Box::new(None))),
            _ => lower_wasm_value(registry, inner, value)
                .map(|value| WasmValue::Option(Box::new(Some(value)))),
        },
        CanonicalTypeRef::Tuple(items) => match value {
            CanonicalValue::Tuple(values) if values.len() == items.len() => items
                .iter()
                .zip(values.iter())
                .map(|(ty, value)| lower_wasm_value(registry, ty, value))
                .collect::<Result<Vec<_>, _>>()
                .map(WasmValue::Tuple),
            _ => Err(BindingError::new("expected canonical tuple")),
        },
        CanonicalTypeRef::Result { .. } => Err(BindingError::new(
            "canonical result types are not supported as direct invocation inputs",
        )),
        CanonicalTypeRef::Named(name) => lower_named_wasm_value(registry, name, value),
    }
}

fn lower_named_wasm_value(
    registry: &ActionRegistry,
    name: &str,
    value: &CanonicalValue,
) -> Result<WasmValue, BindingError> {
    let ty = registry
        .type_by_name(name)
        .ok_or_else(|| BindingError::new(format!("unknown canonical type '{name}'")))?;

    match &ty.kind {
        CanonicalTypeDefKind::Alias(alias) => lower_wasm_value(registry, alias, value),
        CanonicalTypeDefKind::Enum(_) => match value {
            CanonicalValue::EnumCase(value) => Ok(WasmValue::EnumCase(value.clone())),
            _ => Err(BindingError::new(format!(
                "expected canonical enum for '{name}'"
            ))),
        },
        CanonicalTypeDefKind::Record(record) => match value {
            CanonicalValue::Record(fields) => {
                let mut lowered = Vec::with_capacity(record.fields.len());
                for field in &record.fields {
                    let field_value = fields.get(&field.name).ok_or_else(|| {
                        BindingError::new(format!(
                            "missing canonical field '{}' for '{}'",
                            field.name, name
                        ))
                    })?;
                    lowered.push((
                        field.name.clone(),
                        lower_wasm_value(registry, &field.ty, field_value)?,
                    ));
                }
                Ok(WasmValue::Record(lowered))
            }
            _ => Err(BindingError::new(format!(
                "expected canonical record for '{name}'"
            ))),
        },
        CanonicalTypeDefKind::Variant(_) => Err(BindingError::new(
            "variant wasm values are not supported yet",
        )),
        CanonicalTypeDefKind::Flags(_) => {
            Err(BindingError::new("flag wasm values are not supported yet"))
        }
    }
}

fn lift_wasm_result(
    registry: &ActionRegistry,
    result: &CanonicalFunctionResult,
    value: &WasmValue,
) -> Result<CanonicalValue, BindingError> {
    match result {
        CanonicalFunctionResult::None => Ok(CanonicalValue::Null),
        CanonicalFunctionResult::Scalar(ty) => lift_wasm_value(registry, ty, value),
        CanonicalFunctionResult::Named(params) => {
            if params.len() == 1 {
                lift_wasm_value(registry, &params[0].ty, value)
            } else {
                match value {
                    WasmValue::Tuple(items) if items.len() == params.len() => params
                        .iter()
                        .zip(items.iter())
                        .map(|(param, item)| lift_wasm_value(registry, &param.ty, item))
                        .collect::<Result<Vec<_>, _>>()
                        .map(CanonicalValue::Tuple),
                    _ => Err(BindingError::new("expected wasm tuple result")),
                }
            }
        }
    }
}

fn lift_wasm_value(
    registry: &ActionRegistry,
    ty: &CanonicalTypeRef,
    value: &WasmValue,
) -> Result<CanonicalValue, BindingError> {
    match ty {
        CanonicalTypeRef::Primitive(crate::CanonicalPrimitiveType::Bool) => match value {
            WasmValue::Bool(value) => Ok(CanonicalValue::Bool(*value)),
            _ => Err(BindingError::new("expected wasm bool")),
        },
        CanonicalTypeRef::Primitive(crate::CanonicalPrimitiveType::S32) => match value {
            WasmValue::S32(value) => Ok(CanonicalValue::S32(*value)),
            _ => Err(BindingError::new("expected wasm s32")),
        },
        CanonicalTypeRef::Primitive(crate::CanonicalPrimitiveType::S64)
        | CanonicalTypeRef::Primitive(crate::CanonicalPrimitiveType::S16)
        | CanonicalTypeRef::Primitive(crate::CanonicalPrimitiveType::S8) => match value {
            WasmValue::S64(value) => Ok(CanonicalValue::S64(*value)),
            WasmValue::S32(value) => Ok(CanonicalValue::S64((*value).into())),
            _ => Err(BindingError::new("expected wasm signed integer")),
        },
        CanonicalTypeRef::Primitive(crate::CanonicalPrimitiveType::U32) => match value {
            WasmValue::U32(value) => Ok(CanonicalValue::U32(*value)),
            _ => Err(BindingError::new("expected wasm u32")),
        },
        CanonicalTypeRef::Primitive(crate::CanonicalPrimitiveType::U64)
        | CanonicalTypeRef::Primitive(crate::CanonicalPrimitiveType::U16)
        | CanonicalTypeRef::Primitive(crate::CanonicalPrimitiveType::U8) => match value {
            WasmValue::U64(value) => Ok(CanonicalValue::U64(*value)),
            WasmValue::U32(value) => Ok(CanonicalValue::U64((*value).into())),
            _ => Err(BindingError::new("expected wasm unsigned integer")),
        },
        CanonicalTypeRef::Primitive(crate::CanonicalPrimitiveType::String)
        | CanonicalTypeRef::Primitive(crate::CanonicalPrimitiveType::Char) => match value {
            WasmValue::String(value) => Ok(CanonicalValue::String(value.clone())),
            _ => Err(BindingError::new("expected wasm string")),
        },
        CanonicalTypeRef::Primitive(crate::CanonicalPrimitiveType::Float32)
        | CanonicalTypeRef::Primitive(crate::CanonicalPrimitiveType::Float64) => {
            Err(BindingError::new("float wasm values are not supported yet"))
        }
        CanonicalTypeRef::List(inner) => match value {
            WasmValue::List(items) => items
                .iter()
                .map(|item| lift_wasm_value(registry, inner, item))
                .collect::<Result<Vec<_>, _>>()
                .map(CanonicalValue::List),
            _ => Err(BindingError::new("expected wasm list")),
        },
        CanonicalTypeRef::Option(inner) => match value {
            WasmValue::Option(option) => option
                .as_ref()
                .as_ref()
                .map(|value| lift_wasm_value(registry, inner, value))
                .transpose()
                .map(|value| value.unwrap_or(CanonicalValue::Null)),
            _ => Err(BindingError::new("expected wasm option")),
        },
        CanonicalTypeRef::Tuple(items) => match value {
            WasmValue::Tuple(values) if values.len() == items.len() => items
                .iter()
                .zip(values.iter())
                .map(|(ty, value)| lift_wasm_value(registry, ty, value))
                .collect::<Result<Vec<_>, _>>()
                .map(CanonicalValue::Tuple),
            _ => Err(BindingError::new("expected wasm tuple")),
        },
        CanonicalTypeRef::Result { .. } => Err(BindingError::new(
            "wasm result wrappers are not supported yet",
        )),
        CanonicalTypeRef::Named(name) => lift_named_wasm_value(registry, name, value),
    }
}

fn lift_named_wasm_value(
    registry: &ActionRegistry,
    name: &str,
    value: &WasmValue,
) -> Result<CanonicalValue, BindingError> {
    let ty = registry
        .type_by_name(name)
        .ok_or_else(|| BindingError::new(format!("unknown canonical type '{name}'")))?;

    match &ty.kind {
        CanonicalTypeDefKind::Alias(alias) => lift_wasm_value(registry, alias, value),
        CanonicalTypeDefKind::Enum(_) => match value {
            WasmValue::EnumCase(value) => Ok(CanonicalValue::EnumCase(value.clone())),
            _ => Err(BindingError::new(format!(
                "expected wasm enum for '{name}'"
            ))),
        },
        CanonicalTypeDefKind::Record(record) => match value {
            WasmValue::Record(fields) => {
                let by_name = fields.iter().cloned().collect::<BTreeMap<_, _>>();
                let mut lifted = BTreeMap::new();
                for field in &record.fields {
                    let field_value = by_name.get(&field.name).ok_or_else(|| {
                        BindingError::new(format!(
                            "missing wasm field '{}' for '{}'",
                            field.name, name
                        ))
                    })?;
                    lifted.insert(
                        field.name.clone(),
                        lift_wasm_value(registry, &field.ty, field_value)?,
                    );
                }
                Ok(CanonicalValue::Record(lifted))
            }
            _ => Err(BindingError::new(format!(
                "expected wasm record for '{name}'"
            ))),
        },
        CanonicalTypeDefKind::Variant(_) => Err(BindingError::new(
            "variant wasm values are not supported yet",
        )),
        CanonicalTypeDefKind::Flags(_) => {
            Err(BindingError::new("flag wasm values are not supported yet"))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use pera_core::Value;

    use crate::{CanonicalValue, load_canonical_world_from_wit};

    use super::{CanonicalBindings, ModelInvocation, WasmValue};

    fn bindings() -> CanonicalBindings {
        let world = load_canonical_world_from_wit(
            "../../skills/examples/secret-service/world.wit",
            "secret-service-default",
        )
        .unwrap();
        CanonicalBindings::from_world(world).unwrap()
    }

    #[test]
    fn model_adapter_lowers_python_names_into_canonical_invocation() {
        let bindings = bindings();
        let adapter = bindings.model_adapter();

        let invocation = ModelInvocation {
            function_name: "resolve_mission".to_owned(),
            arguments: BTreeMap::from([
                ("mission_id".to_owned(), Value::String("m-1".to_owned())),
                ("outcome".to_owned(), Value::String("success".to_owned())),
                ("notes".to_owned(), Value::Null),
            ]),
        };

        let canonical = adapter.lower_invocation(&invocation).unwrap();
        assert_eq!(canonical.action_name, "resolve-mission");
        assert_eq!(
            canonical.arguments.get("mission-id"),
            Some(&CanonicalValue::String("m-1".to_owned()))
        );
        assert_eq!(
            canonical.arguments.get("outcome"),
            Some(&CanonicalValue::EnumCase("success".to_owned()))
        );
    }

    #[test]
    fn wasm_adapter_lowers_canonical_invocation_into_ordered_arguments() {
        let bindings = bindings();
        let model = bindings.model_adapter();
        let wasm = bindings.wasm_adapter();

        let invocation = ModelInvocation {
            function_name: "resolve_mission".to_owned(),
            arguments: BTreeMap::from([
                ("mission_id".to_owned(), Value::String("m-1".to_owned())),
                ("outcome".to_owned(), Value::String("failure".to_owned())),
                (
                    "notes".to_owned(),
                    Value::String("extract failed".to_owned()),
                ),
            ]),
        };

        let canonical = model.lower_invocation(&invocation).unwrap();
        let lowered = wasm.lower_invocation(&canonical).unwrap();
        assert_eq!(lowered.export_name, "resolve-mission");
        assert_eq!(lowered.arguments.len(), 3);
        assert_eq!(lowered.arguments[0], WasmValue::String("m-1".to_owned()));
        assert_eq!(
            lowered.arguments[1],
            WasmValue::EnumCase("failure".to_owned())
        );
        assert_eq!(
            lowered.arguments[2],
            WasmValue::Option(Box::new(Some(WasmValue::String(
                "extract failed".to_owned()
            ))))
        );
    }

    #[test]
    fn model_adapter_lifts_canonical_record_result_back_to_python_surface() {
        let bindings = bindings();
        let adapter = bindings.model_adapter();

        let value = CanonicalValue::Record(BTreeMap::from([
            (
                "id".to_owned(),
                CanonicalValue::String("mission-7".to_owned()),
            ),
            (
                "objective".to_owned(),
                CanonicalValue::String("Observe harbor".to_owned()),
            ),
            (
                "difficulty".to_owned(),
                CanonicalValue::EnumCase("high".to_owned()),
            ),
            (
                "region".to_owned(),
                CanonicalValue::String("mediterranean".to_owned()),
            ),
            (
                "required-skills".to_owned(),
                CanonicalValue::List(vec![CanonicalValue::String("surveillance".to_owned())]),
            ),
            (
                "status".to_owned(),
                CanonicalValue::EnumCase("resolved".to_owned()),
            ),
            ("assigned-agent-id".to_owned(), CanonicalValue::Null),
            (
                "result-notes".to_owned(),
                CanonicalValue::String("clean exit".to_owned()),
            ),
        ]));

        let lifted = adapter.lift_result("create-mission", &value).unwrap();
        match lifted {
            Value::Map(fields) => {
                assert_eq!(
                    fields.get("objective"),
                    Some(&Value::String("Observe harbor".to_owned()))
                );
                assert_eq!(
                    fields.get("difficulty"),
                    Some(&Value::String("high".to_owned()))
                );
                assert_eq!(fields.get("assigned_agent_id"), Some(&Value::Null));
            }
            _ => panic!("expected record-shaped model value"),
        }
    }
}
