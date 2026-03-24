use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt::{Display, Formatter};
use std::sync::Arc;

use pera_core::{CanonicalInvocation, CanonicalValue, Value};
use wasmtime::component::Val;

use crate::ir::{
    CanonicalFunctionResult, CanonicalTypeDef, CanonicalTypeDefKind, CanonicalTypeRef,
    CanonicalWorld,
};
use crate::python::python_function_name;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillMetadata {
    pub skill_name: String,
    pub python_namespace: String,
    pub skill_version: Option<String>,
    pub profile_name: Option<String>,
    pub world_name: String,
    pub runtime_kind: Option<String>,
    pub artifact_ref: Option<String>,
}

impl SkillMetadata {
    pub fn new(skill_name: impl Into<String>, world_name: impl Into<String>) -> Self {
        let skill_name = skill_name.into();
        Self {
            python_namespace: python_function_name(&skill_name),
            skill_name,
            skill_version: None,
            profile_name: None,
            world_name: world_name.into(),
            runtime_kind: None,
            artifact_ref: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct CatalogSkill {
    pub metadata: SkillMetadata,
    pub world: CanonicalWorld,
}

#[derive(Debug, Clone)]
pub struct SkillCatalog {
    registry: Arc<ActionRegistry>,
    skills_by_key: Arc<BTreeMap<String, CatalogSkill>>,
}

impl SkillCatalog {
    pub fn from_skill(skill: CatalogSkill) -> Result<Self, BindingError> {
        Self::from_skills(vec![skill])
    }

    pub fn from_skills(skills: Vec<CatalogSkill>) -> Result<Self, BindingError> {
        let registry = ActionRegistry::from_skills(skills.clone())?;
        let mut skills_by_key = BTreeMap::new();
        for skill in skills {
            let key = skill_catalog_key(
                &skill.metadata.skill_name,
                skill.metadata.skill_version.as_deref(),
                skill.metadata.profile_name.as_deref(),
            );
            if skills_by_key.insert(key.clone(), skill).is_some() {
                return Err(BindingError::new(format!(
                    "duplicate catalog skill key '{key}'"
                )));
            }
        }
        Ok(Self {
            registry: Arc::new(registry),
            skills_by_key: Arc::new(skills_by_key),
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

    pub fn wasmtime_adapter(&self) -> WasmtimeAdapter {
        WasmtimeAdapter {
            registry: Arc::clone(&self.registry),
        }
    }

    pub fn resolve_skill(
        &self,
        skill_name: &str,
        skill_version: Option<&str>,
        profile_name: Option<&str>,
    ) -> Option<&CatalogSkill> {
        self.skills_by_key
            .get(&skill_catalog_key(skill_name, skill_version, profile_name))
    }
}

#[derive(Debug, Clone)]
pub struct CanonicalBindings {
    catalog: SkillCatalog,
}

impl CanonicalBindings {
    pub fn from_world(world: CanonicalWorld) -> Result<Self, BindingError> {
        let skill_name = world
            .package
            .as_ref()
            .map(|package| package.name.clone())
            .unwrap_or_else(|| world.name.clone());
        let metadata = SkillMetadata::new(skill_name, world.name.clone());
        let catalog = SkillCatalog::from_skill(CatalogSkill { metadata, world })?;
        Ok(Self { catalog })
    }

    pub fn action_registry(&self) -> &ActionRegistry {
        self.catalog.action_registry()
    }

    pub fn model_adapter(&self) -> ModelAdapter {
        self.catalog.model_adapter()
    }

    pub fn wasmtime_adapter(&self) -> WasmtimeAdapter {
        self.catalog.wasmtime_adapter()
    }
}

#[derive(Debug, Clone)]
pub struct ActionRegistry {
    actions_by_canonical_action_id: BTreeMap<String, ActionDefinition>,
    actions_by_qualified_model_name: BTreeMap<String, String>,
    unique_actions_by_model_name: BTreeMap<String, String>,
    unique_actions_by_local_name: BTreeMap<String, String>,
    types_by_skill_and_name: BTreeMap<(String, String), CanonicalTypeDef>,
}

impl ActionRegistry {
    pub fn from_world(world: CanonicalWorld) -> Result<Self, BindingError> {
        let skill_name = world
            .package
            .as_ref()
            .map(|package| package.name.clone())
            .unwrap_or_else(|| world.name.clone());
        let metadata = SkillMetadata::new(skill_name, world.name.clone());
        Self::from_skills(vec![CatalogSkill { metadata, world }])
    }

    pub fn from_skills(skills: Vec<CatalogSkill>) -> Result<Self, BindingError> {
        let mut actions_by_canonical_action_id = BTreeMap::new();
        let mut actions_by_qualified_model_name = BTreeMap::new();
        let mut unique_actions_by_model_name = BTreeMap::new();
        let mut unique_actions_by_local_name = BTreeMap::new();
        let mut duplicate_model_names = BTreeSet::new();
        let mut duplicate_local_names = BTreeSet::new();
        let mut types_by_skill_and_name = BTreeMap::new();

        for skill in skills {
            let exports =
                skill.world.exports.first().ok_or_else(|| {
                    BindingError::new("canonical world has no exported interface")
                })?;

            for ty in &exports.types {
                types_by_skill_and_name.insert(
                    (skill.metadata.skill_name.clone(), ty.name.clone()),
                    ty.clone(),
                );
            }

            for function in &exports.functions {
                let model_name = python_function_name(&function.name);
                let canonical_action_id =
                    format!("{}.{}", skill.metadata.skill_name, function.name);
                let qualified_model_name =
                    format!("{}.{}", skill.metadata.python_namespace, model_name);

                let definition = ActionDefinition {
                    skill: skill.metadata.clone(),
                    action_name: function.name.clone(),
                    canonical_action_id: canonical_action_id.clone(),
                    model_name: model_name.clone(),
                    qualified_model_name: qualified_model_name.clone(),
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

                if actions_by_canonical_action_id
                    .insert(canonical_action_id.clone(), definition)
                    .is_some()
                {
                    return Err(BindingError::new(format!(
                        "duplicate canonical action id '{canonical_action_id}'"
                    )));
                }

                actions_by_qualified_model_name
                    .insert(qualified_model_name, canonical_action_id.clone());

                match unique_actions_by_model_name.get(&model_name) {
                    Some(existing) if existing != &canonical_action_id => {
                        duplicate_model_names.insert(model_name.clone());
                        unique_actions_by_model_name.remove(&model_name);
                    }
                    None if !duplicate_model_names.contains(&model_name) => {
                        unique_actions_by_model_name
                            .insert(model_name.clone(), canonical_action_id.clone());
                    }
                    _ => {}
                }

                match unique_actions_by_local_name.get(&function.name) {
                    Some(existing) if existing != &canonical_action_id => {
                        duplicate_local_names.insert(function.name.clone());
                        unique_actions_by_local_name.remove(&function.name);
                    }
                    None if !duplicate_local_names.contains(&function.name) => {
                        unique_actions_by_local_name
                            .insert(function.name.clone(), canonical_action_id);
                    }
                    _ => {}
                }
            }
        }

        Ok(Self {
            actions_by_canonical_action_id,
            actions_by_qualified_model_name,
            unique_actions_by_model_name,
            unique_actions_by_local_name,
            types_by_skill_and_name,
        })
    }

    pub fn resolve_canonical_action(
        &self,
        canonical_action_id: &str,
    ) -> Option<&ActionDefinition> {
        self.actions_by_canonical_action_id.get(canonical_action_id)
    }

    pub fn resolve_model_action(&self, model_name: &str) -> Option<&ActionDefinition> {
        if let Some(canonical_action_id) = self.actions_by_qualified_model_name.get(model_name) {
            return self.actions_by_canonical_action_id.get(canonical_action_id);
        }

        if let Some(canonical_action_id) = self.unique_actions_by_model_name.get(model_name) {
            return self.actions_by_canonical_action_id.get(canonical_action_id);
        }

        if let Some(canonical_action_id) = self.unique_actions_by_local_name.get(model_name) {
            return self.actions_by_canonical_action_id.get(canonical_action_id);
        }

        None
    }

    pub fn type_by_name_in_skill(&self, skill_name: &str, name: &str) -> Option<&CanonicalTypeDef> {
        self.types_by_skill_and_name
            .get(&(skill_name.to_owned(), name.to_owned()))
    }
}

#[derive(Debug, Clone)]
pub struct ActionDefinition {
    pub skill: SkillMetadata,
    pub action_name: String,
    pub canonical_action_id: String,
    pub model_name: String,
    pub qualified_model_name: String,
    pub docs: Option<String>,
    pub params: Vec<ActionParam>,
    pub result: CanonicalFunctionResult,
}

impl ActionDefinition {
    fn locator(&self) -> ActionLocator {
        ActionLocator {
            skill: self.skill.clone(),
            action_name: self.action_name.clone(),
            canonical_action_id: self.canonical_action_id.clone(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ActionParam {
    pub canonical_name: String,
    pub model_name: String,
    pub ty: CanonicalTypeRef,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionLocator {
    pub skill: SkillMetadata,
    pub action_name: String,
    pub canonical_action_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelInvocation {
    pub function_name: String,
    pub arguments: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WasmtimeInvocation {
    pub locator: ActionLocator,
    pub export_name: String,
    pub arguments: Vec<Val>,
}

#[derive(Debug, Clone)]
pub struct ModelAdapter {
    registry: Arc<ActionRegistry>,
}

impl ModelAdapter {
    pub fn model_invocation_to_canonical_invocation(
        &self,
        invocation: &ModelInvocation,
    ) -> Result<CanonicalInvocation, BindingError> {
        let action = self
            .registry
            .resolve_model_action(&invocation.function_name)
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
            let canonical = lower_model_value(
                &self.registry,
                &action.skill.skill_name,
                &param.ty,
                model_value,
            )?;
            arguments.insert(param.canonical_name.clone(), canonical);
        }

        Ok(CanonicalInvocation {
            action_name: pera_core::ActionName::new(action.action_name.clone()),
            arguments,
        })
    }

    pub fn canonical_result_to_model_value(
        &self,
        action_name: &str,
        value: &CanonicalValue,
    ) -> Result<Value, BindingError> {
        let action = self
            .registry
            .resolve_canonical_action(action_name)
            .ok_or_else(|| BindingError::new(format!("unknown action '{action_name}'")))?;
        lift_model_result(
            &self.registry,
            &action.skill.skill_name,
            &action.result,
            value,
        )
    }
}

#[derive(Debug, Clone)]
pub struct WasmtimeAdapter {
    registry: Arc<ActionRegistry>,
}

impl WasmtimeAdapter {
    pub fn canonical_invocation_to_wasmtime_invocation(
        &self,
        skill_name: &str,
        invocation: &CanonicalInvocation,
    ) -> Result<WasmtimeInvocation, BindingError> {
        let canonical_action_id = format!("{}.{}", skill_name, invocation.action_name.as_str());
        let action = self
            .registry
            .resolve_canonical_action(&canonical_action_id)
            .ok_or_else(|| {
                BindingError::new(format!(
                    "unknown canonical action '{}'",
                    canonical_action_id
                ))
            })?;

        let mut arguments = Vec::with_capacity(action.params.len());
        if invocation.arguments.len() != action.params.len() {
            return Err(BindingError::new(format!(
                "action '{}' expected {} argument(s) but received {}",
                canonical_action_id,
                action.params.len(),
                invocation.arguments.len()
            )));
        }

        for param in &action.params {
            let value = invocation.arguments.get(&param.canonical_name).ok_or_else(|| {
                BindingError::new(format!(
                    "missing canonical argument '{}' for action '{}'",
                    param.canonical_name, canonical_action_id
                ))
            })?;
            arguments.push(canonical_value_to_wasmtime_val(
                &self.registry,
                &action.skill.skill_name,
                &param.ty,
                value,
            )?);
        }

        Ok(WasmtimeInvocation {
            locator: action.locator(),
            export_name: action.action_name.clone(),
            arguments,
        })
    }

    pub fn wasmtime_value_to_canonical_value(
        &self,
        action_name: &str,
        value: &Val,
    ) -> Result<CanonicalValue, BindingError> {
        let action = self
            .registry
            .resolve_canonical_action(action_name)
            .ok_or_else(|| BindingError::new(format!("unknown action '{action_name}'")))?;
        wasmtime_val_to_canonical_result(
            &self.registry,
            &action.skill.skill_name,
            &action.result,
            value,
        )
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

fn skill_catalog_key(
    skill_name: &str,
    skill_version: Option<&str>,
    profile_name: Option<&str>,
) -> String {
    format!(
        "{}::{}::{}",
        skill_name,
        skill_version.unwrap_or_default(),
        profile_name.unwrap_or_default()
    )
}

fn lower_model_value(
    registry: &ActionRegistry,
    skill_name: &str,
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
                .map(|item| lower_model_value(registry, skill_name, inner, item))
                .collect::<Result<Vec<_>, _>>()
                .map(CanonicalValue::List),
            _ => Err(BindingError::new("expected list")),
        },
        CanonicalTypeRef::Option(inner) => match value {
            Value::Null => Ok(CanonicalValue::Null),
            _ => lower_model_value(registry, skill_name, inner, value),
        },
        CanonicalTypeRef::Tuple(items) => match value {
            Value::List(values) if values.len() == items.len() => items
                .iter()
                .zip(values.iter())
                .map(|(ty, value)| lower_model_value(registry, skill_name, ty, value))
                .collect::<Result<Vec<_>, _>>()
                .map(CanonicalValue::Tuple),
            _ => Err(BindingError::new("expected tuple-compatible list")),
        },
        CanonicalTypeRef::Result { .. } => Err(BindingError::new(
            "model result values are not supported as direct inputs",
        )),
        CanonicalTypeRef::Named(name) => lower_named_model_value(registry, skill_name, name, value),
    }
}

fn lower_named_model_value(
    registry: &ActionRegistry,
    skill_name: &str,
    name: &str,
    value: &Value,
) -> Result<CanonicalValue, BindingError> {
    let ty = registry
        .type_by_name_in_skill(skill_name, name)
        .ok_or_else(|| {
            BindingError::new(format!("unknown canonical type '{skill_name}.{name}'"))
        })?;

    match &ty.kind {
        CanonicalTypeDefKind::Alias(alias) => lower_model_value(registry, skill_name, alias, value),
        CanonicalTypeDefKind::Enum(cases) => match value {
            Value::String(value) if cases.iter().any(|case| case.name == *value) => {
                Ok(CanonicalValue::EnumCase(value.clone()))
            }
            _ => Err(BindingError::new(format!(
                "expected enum case for type '{skill_name}.{name}'"
            ))),
        },
        CanonicalTypeDefKind::Record(record) => match value {
            Value::Map(fields) => {
                let mut lowered = BTreeMap::new();
                for field in &record.fields {
                    let model_name = python_function_name(&field.name);
                    let field_value = fields.get(&model_name).ok_or_else(|| {
                        BindingError::new(format!(
                            "missing field '{}' for record '{}.{}'",
                            model_name, skill_name, name
                        ))
                    })?;
                    lowered.insert(
                        field.name.clone(),
                        lower_model_value(registry, skill_name, &field.ty, field_value)?,
                    );
                }
                Ok(CanonicalValue::Record(lowered))
            }
            _ => Err(BindingError::new(format!(
                "expected record value for type '{skill_name}.{name}'"
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
    skill_name: &str,
    result: &CanonicalFunctionResult,
    value: &CanonicalValue,
) -> Result<Value, BindingError> {
    match result {
        CanonicalFunctionResult::None => Ok(Value::Null),
        CanonicalFunctionResult::Scalar(ty) => lift_model_value(registry, skill_name, ty, value),
        CanonicalFunctionResult::Named(params) => {
            if params.len() == 1 {
                lift_model_value(registry, skill_name, &params[0].ty, value)
            } else {
                match value {
                    CanonicalValue::Tuple(items) if items.len() == params.len() => params
                        .iter()
                        .zip(items.iter())
                        .map(|(param, item)| {
                            lift_model_value(registry, skill_name, &param.ty, item)
                        })
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
    skill_name: &str,
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
                .map(|item| lift_model_value(registry, skill_name, inner, item))
                .collect::<Result<Vec<_>, _>>()
                .map(Value::List),
            _ => Err(BindingError::new("expected canonical list")),
        },
        CanonicalTypeRef::Option(inner) => match value {
            CanonicalValue::Null => Ok(Value::Null),
            _ => lift_model_value(registry, skill_name, inner, value),
        },
        CanonicalTypeRef::Tuple(items) => match value {
            CanonicalValue::Tuple(values) if values.len() == items.len() => items
                .iter()
                .zip(values.iter())
                .map(|(ty, value)| lift_model_value(registry, skill_name, ty, value))
                .collect::<Result<Vec<_>, _>>()
                .map(Value::List),
            _ => Err(BindingError::new("expected canonical tuple")),
        },
        CanonicalTypeRef::Result { .. } => Err(BindingError::new(
            "canonical result values are not supported yet",
        )),
        CanonicalTypeRef::Named(name) => lift_named_model_value(registry, skill_name, name, value),
    }
}

fn lift_named_model_value(
    registry: &ActionRegistry,
    skill_name: &str,
    name: &str,
    value: &CanonicalValue,
) -> Result<Value, BindingError> {
    let ty = registry
        .type_by_name_in_skill(skill_name, name)
        .ok_or_else(|| {
            BindingError::new(format!("unknown canonical type '{skill_name}.{name}'"))
        })?;

    match &ty.kind {
        CanonicalTypeDefKind::Alias(alias) => lift_model_value(registry, skill_name, alias, value),
        CanonicalTypeDefKind::Enum(_) => match value {
            CanonicalValue::EnumCase(value) => Ok(Value::String(value.clone())),
            _ => Err(BindingError::new(format!(
                "expected canonical enum value for '{skill_name}.{name}'"
            ))),
        },
        CanonicalTypeDefKind::Record(record) => match value {
            CanonicalValue::Record(fields) => {
                let mut lifted = BTreeMap::new();
                for field in &record.fields {
                    let field_value = fields.get(&field.name).ok_or_else(|| {
                        BindingError::new(format!(
                            "missing canonical field '{}' for record '{}.{}'",
                            field.name, skill_name, name
                        ))
                    })?;
                    lifted.insert(
                        python_function_name(&field.name),
                        lift_model_value(registry, skill_name, &field.ty, field_value)?,
                    );
                }
                Ok(Value::Map(lifted))
            }
            _ => Err(BindingError::new(format!(
                "expected canonical record for '{skill_name}.{name}'"
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

fn canonical_value_to_wasmtime_val(
    registry: &ActionRegistry,
    skill_name: &str,
    ty: &CanonicalTypeRef,
    value: &CanonicalValue,
) -> Result<Val, BindingError> {
    match ty {
        CanonicalTypeRef::Primitive(crate::CanonicalPrimitiveType::Bool) => match value {
            CanonicalValue::Bool(value) => Ok(Val::Bool(*value)),
            _ => Err(BindingError::new("expected canonical bool")),
        },
        CanonicalTypeRef::Primitive(crate::CanonicalPrimitiveType::S32) => match value {
            CanonicalValue::S32(value) => Ok(Val::S32(*value)),
            _ => Err(BindingError::new("expected canonical s32")),
        },
        CanonicalTypeRef::Primitive(crate::CanonicalPrimitiveType::S64)
        | CanonicalTypeRef::Primitive(crate::CanonicalPrimitiveType::S16)
        | CanonicalTypeRef::Primitive(crate::CanonicalPrimitiveType::S8) => match value {
            CanonicalValue::S64(value) => Ok(Val::S64(*value)),
            CanonicalValue::S32(value) => Ok(Val::S64((*value).into())),
            _ => Err(BindingError::new("expected canonical signed integer")),
        },
        CanonicalTypeRef::Primitive(crate::CanonicalPrimitiveType::U32) => match value {
            CanonicalValue::U32(value) => Ok(Val::U32(*value)),
            _ => Err(BindingError::new("expected canonical u32")),
        },
        CanonicalTypeRef::Primitive(crate::CanonicalPrimitiveType::U64)
        | CanonicalTypeRef::Primitive(crate::CanonicalPrimitiveType::U16)
        | CanonicalTypeRef::Primitive(crate::CanonicalPrimitiveType::U8) => match value {
            CanonicalValue::U64(value) => Ok(Val::U64(*value)),
            CanonicalValue::U32(value) => Ok(Val::U64((*value).into())),
            _ => Err(BindingError::new("expected canonical unsigned integer")),
        },
        CanonicalTypeRef::Primitive(crate::CanonicalPrimitiveType::String)
        | CanonicalTypeRef::Primitive(crate::CanonicalPrimitiveType::Char) => match value {
            CanonicalValue::String(value) => Ok(Val::String(value.clone().into())),
            _ => Err(BindingError::new("expected canonical string")),
        },
        CanonicalTypeRef::Primitive(crate::CanonicalPrimitiveType::Float32)
        | CanonicalTypeRef::Primitive(crate::CanonicalPrimitiveType::Float64) => {
            Err(BindingError::new("float wasm values are not supported yet"))
        }
        CanonicalTypeRef::List(inner) => match value {
            CanonicalValue::List(items) => items
                .iter()
                .map(|item| canonical_value_to_wasmtime_val(registry, skill_name, inner, item))
                .collect::<Result<Vec<_>, _>>()
                .map(Val::List),
            _ => Err(BindingError::new("expected canonical list")),
        },
        CanonicalTypeRef::Option(inner) => match value {
            CanonicalValue::Null => Ok(Val::Option(None)),
            _ => canonical_value_to_wasmtime_val(registry, skill_name, inner, value)
                .map(|value| Val::Option(Some(Box::new(value)))),
        },
        CanonicalTypeRef::Tuple(items) => match value {
            CanonicalValue::Tuple(values) if values.len() == items.len() => items
                .iter()
                .zip(values.iter())
                .map(|(ty, value)| canonical_value_to_wasmtime_val(registry, skill_name, ty, value))
                .collect::<Result<Vec<_>, _>>()
                .map(Val::Tuple),
            _ => Err(BindingError::new("expected canonical tuple")),
        },
        CanonicalTypeRef::Result { .. } => Err(BindingError::new(
            "canonical result types are not supported as direct invocation inputs",
        )),
        CanonicalTypeRef::Named(name) => {
            canonical_named_value_to_wasmtime_val(registry, skill_name, name, value)
        }
    }
}

fn canonical_named_value_to_wasmtime_val(
    registry: &ActionRegistry,
    skill_name: &str,
    name: &str,
    value: &CanonicalValue,
) -> Result<Val, BindingError> {
    let ty = registry
        .type_by_name_in_skill(skill_name, name)
        .ok_or_else(|| {
            BindingError::new(format!("unknown canonical type '{skill_name}.{name}'"))
        })?;

    match &ty.kind {
        CanonicalTypeDefKind::Alias(alias) => {
            canonical_value_to_wasmtime_val(registry, skill_name, alias, value)
        }
        CanonicalTypeDefKind::Enum(_) => match value {
            CanonicalValue::EnumCase(value) => Ok(Val::Enum(value.clone())),
            _ => Err(BindingError::new(format!(
                "expected canonical enum for '{skill_name}.{name}'"
            ))),
        },
        CanonicalTypeDefKind::Record(record) => match value {
            CanonicalValue::Record(fields) => {
                let mut lowered = Vec::with_capacity(record.fields.len());
                for field in &record.fields {
                    let field_value = fields.get(&field.name).ok_or_else(|| {
                        BindingError::new(format!(
                            "missing canonical field '{}' for '{}.{}'",
                            field.name, skill_name, name
                        ))
                    })?;
                    lowered.push((
                        field.name.clone(),
                        canonical_value_to_wasmtime_val(
                            registry,
                            skill_name,
                            &field.ty,
                            field_value,
                        )?,
                    ));
                }
                Ok(Val::Record(lowered))
            }
            _ => Err(BindingError::new(format!(
                "expected canonical record for '{skill_name}.{name}'"
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

fn wasmtime_val_to_canonical_result(
    registry: &ActionRegistry,
    skill_name: &str,
    result: &CanonicalFunctionResult,
    value: &Val,
) -> Result<CanonicalValue, BindingError> {
    match result {
        CanonicalFunctionResult::None => Ok(CanonicalValue::Null),
        CanonicalFunctionResult::Scalar(ty) => {
            wasmtime_val_to_canonical_value(registry, skill_name, ty, value)
        }
        CanonicalFunctionResult::Named(params) => {
            if params.len() == 1 {
                wasmtime_val_to_canonical_value(registry, skill_name, &params[0].ty, value)
            } else {
                match value {
                    Val::Tuple(items) if items.len() == params.len() => params
                        .iter()
                        .zip(items.iter())
                        .map(|(param, item)| {
                            wasmtime_val_to_canonical_value(
                                registry,
                                skill_name,
                                &param.ty,
                                item,
                            )
                        })
                        .collect::<Result<Vec<_>, _>>()
                        .map(CanonicalValue::Tuple),
                    _ => Err(BindingError::new("expected wasm tuple result")),
                }
            }
        }
    }
}

fn wasmtime_val_to_canonical_value(
    registry: &ActionRegistry,
    skill_name: &str,
    ty: &CanonicalTypeRef,
    value: &Val,
) -> Result<CanonicalValue, BindingError> {
    match ty {
        CanonicalTypeRef::Primitive(crate::CanonicalPrimitiveType::Bool) => match value {
            Val::Bool(value) => Ok(CanonicalValue::Bool(*value)),
            _ => Err(BindingError::new("expected wasm bool")),
        },
        CanonicalTypeRef::Primitive(crate::CanonicalPrimitiveType::S32) => match value {
            Val::S32(value) => Ok(CanonicalValue::S32(*value)),
            _ => Err(BindingError::new("expected wasm s32")),
        },
        CanonicalTypeRef::Primitive(crate::CanonicalPrimitiveType::S64)
        | CanonicalTypeRef::Primitive(crate::CanonicalPrimitiveType::S16)
        | CanonicalTypeRef::Primitive(crate::CanonicalPrimitiveType::S8) => match value {
            Val::S64(value) => Ok(CanonicalValue::S64(*value)),
            Val::S32(value) => Ok(CanonicalValue::S64((*value).into())),
            _ => Err(BindingError::new("expected wasm signed integer")),
        },
        CanonicalTypeRef::Primitive(crate::CanonicalPrimitiveType::U32) => match value {
            Val::U32(value) => Ok(CanonicalValue::U32(*value)),
            _ => Err(BindingError::new("expected wasm u32")),
        },
        CanonicalTypeRef::Primitive(crate::CanonicalPrimitiveType::U64)
        | CanonicalTypeRef::Primitive(crate::CanonicalPrimitiveType::U16)
        | CanonicalTypeRef::Primitive(crate::CanonicalPrimitiveType::U8) => match value {
            Val::U64(value) => Ok(CanonicalValue::U64(*value)),
            Val::U32(value) => Ok(CanonicalValue::U64((*value).into())),
            _ => Err(BindingError::new("expected wasm unsigned integer")),
        },
        CanonicalTypeRef::Primitive(crate::CanonicalPrimitiveType::String)
        | CanonicalTypeRef::Primitive(crate::CanonicalPrimitiveType::Char) => match value {
            Val::String(value) => Ok(CanonicalValue::String(value.to_string())),
            _ => Err(BindingError::new("expected wasm string")),
        },
        CanonicalTypeRef::Primitive(crate::CanonicalPrimitiveType::Float32)
        | CanonicalTypeRef::Primitive(crate::CanonicalPrimitiveType::Float64) => {
            Err(BindingError::new("float wasm values are not supported yet"))
        }
        CanonicalTypeRef::List(inner) => match value {
            Val::List(items) => items
                .iter()
                .map(|item| wasmtime_val_to_canonical_value(registry, skill_name, inner, item))
                .collect::<Result<Vec<_>, _>>()
                .map(CanonicalValue::List),
            _ => Err(BindingError::new("expected wasm list")),
        },
        CanonicalTypeRef::Option(inner) => match value {
            Val::Option(option) => option
                .as_ref()
                .as_ref()
                .map(|value| wasmtime_val_to_canonical_value(registry, skill_name, inner, value))
                .transpose()
                .map(|value: Option<CanonicalValue>| value.unwrap_or(CanonicalValue::Null)),
            _ => Err(BindingError::new("expected wasm option")),
        },
        CanonicalTypeRef::Tuple(items) => match value {
            Val::Tuple(values) if values.len() == items.len() => items
                .iter()
                .zip(values.iter())
                .map(|(ty, value)| wasmtime_val_to_canonical_value(registry, skill_name, ty, value))
                .collect::<Result<Vec<_>, _>>()
                .map(CanonicalValue::Tuple),
            _ => Err(BindingError::new("expected wasm tuple")),
        },
        CanonicalTypeRef::Result { .. } => Err(BindingError::new(
            "wasm result wrappers are not supported yet",
        )),
        CanonicalTypeRef::Named(name) => {
            wasmtime_named_value_to_canonical_value(registry, skill_name, name, value)
        }
    }
}

fn wasmtime_named_value_to_canonical_value(
    registry: &ActionRegistry,
    skill_name: &str,
    name: &str,
    value: &Val,
) -> Result<CanonicalValue, BindingError> {
    let ty = registry
        .type_by_name_in_skill(skill_name, name)
        .ok_or_else(|| {
            BindingError::new(format!("unknown canonical type '{skill_name}.{name}'"))
        })?;

    match &ty.kind {
        CanonicalTypeDefKind::Alias(alias) => {
            wasmtime_val_to_canonical_value(registry, skill_name, alias, value)
        }
        CanonicalTypeDefKind::Enum(_) => match value {
            Val::Enum(value) => Ok(CanonicalValue::EnumCase(value.clone())),
            _ => Err(BindingError::new(format!(
                "expected wasm enum for '{skill_name}.{name}'"
            ))),
        },
        CanonicalTypeDefKind::Record(record) => match value {
            Val::Record(fields) => {
                let by_name = fields.iter().cloned().collect::<BTreeMap<_, _>>();
                let mut lifted = BTreeMap::new();
                for field in &record.fields {
                    let field_value = by_name.get(&field.name).ok_or_else(|| {
                        BindingError::new(format!(
                            "missing wasm field '{}' for '{}.{}'",
                            field.name, skill_name, name
                        ))
                    })?;
                    lifted.insert(
                        field.name.clone(),
                        wasmtime_val_to_canonical_value(
                            registry,
                            skill_name,
                            &field.ty,
                            field_value,
                        )?,
                    );
                }
                Ok(CanonicalValue::Record(lifted))
            }
            _ => Err(BindingError::new(format!(
                "expected wasm record for '{skill_name}.{name}'"
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

    use pera_core::{CanonicalValue, Value};
    use wasmtime::component::Val;

    use crate::{CanonicalWorld, load_canonical_world_from_wit};

    use super::{
        CanonicalBindings, CatalogSkill, ModelInvocation, SkillCatalog, SkillMetadata,
    };

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

        let canonical = adapter
            .model_invocation_to_canonical_invocation(&invocation)
            .unwrap();
        assert_eq!(canonical.action_name.as_str(), "resolve-mission");
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
        let wasm = bindings.wasmtime_adapter();

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

        let canonical = model
            .model_invocation_to_canonical_invocation(&invocation)
            .unwrap();
        let lowered = wasm
            .canonical_invocation_to_wasmtime_invocation("secret-service", &canonical)
            .unwrap();
        assert_eq!(lowered.locator.skill.skill_name, "secret-service");
        assert_eq!(lowered.export_name, "resolve-mission");
        assert_eq!(lowered.arguments.len(), 3);
        assert_eq!(lowered.arguments[0], Val::String("m-1".to_owned().into()));
        assert_eq!(lowered.arguments[1], Val::Enum("failure".to_owned()));
        assert_eq!(
            lowered.arguments[2],
            Val::Option(Some(Box::new(Val::String(
                "extract failed".to_owned().into()
            ))))
        );
    }

    #[test]
    fn wasm_adapter_lowers_action_request_directly() {
        let bindings = bindings();
        let wasm = bindings.wasmtime_adapter();

        let lowered = wasm
            .canonical_invocation_to_wasmtime_invocation(
                "secret-service",
                &pera_core::CanonicalInvocation {
                    action_name: pera_core::ActionName::new("resolve-mission"),
                    arguments: BTreeMap::from([
                        ("mission-id".to_owned(), CanonicalValue::String("m-1".to_owned())),
                        ("outcome".to_owned(), CanonicalValue::EnumCase("failure".to_owned())),
                        (
                            "notes".to_owned(),
                            CanonicalValue::String("extract failed".to_owned()),
                        ),
                    ]),
                },
            )
            .unwrap();

        assert_eq!(lowered.locator.skill.skill_name, "secret-service");
        assert_eq!(lowered.export_name, "resolve-mission");
        assert_eq!(lowered.arguments.len(), 3);
        assert_eq!(lowered.arguments[0], Val::String("m-1".to_owned().into()));
        assert_eq!(lowered.arguments[1], Val::Enum("failure".to_owned()));
        assert_eq!(
            lowered.arguments[2],
            Val::Option(Some(Box::new(Val::String(
                "extract failed".to_owned().into()
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

        let lifted = adapter
            .canonical_result_to_model_value("secret-service.create-mission", &value)
            .unwrap();
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

    #[test]
    fn skill_catalog_resolves_both_unique_and_namespaced_model_names() {
        let secret_world = load_canonical_world_from_wit(
            "../../skills/examples/secret-service/world.wit",
            "secret-service-default",
        )
        .unwrap();
        let weather_world = load_canonical_world_from_wit(
            "../../skills/examples/weather-brief/world.wit",
            "weather-brief-default",
        )
        .unwrap();

        let catalog = SkillCatalog::from_skills(vec![
            CatalogSkill {
                metadata: SkillMetadata::new("secret-service", "secret-service-default"),
                world: secret_world,
            },
            CatalogSkill {
                metadata: SkillMetadata::new("weather-brief", "weather-brief-default"),
                world: weather_world,
            },
        ])
        .unwrap();

        let registry = catalog.action_registry();
        assert_eq!(
            registry
                .resolve_model_action("resolve_mission")
                .map(|action| action.canonical_action_id.as_str()),
            Some("secret-service.resolve-mission")
        );
        assert_eq!(
            registry
                .resolve_model_action("weather_brief.get_forecast")
                .map(|action| action.canonical_action_id.as_str()),
            Some("weather-brief.get-forecast")
        );
        assert_eq!(
            registry
                .resolve_canonical_action("secret-service.resolve-mission")
                .map(|action| action.canonical_action_id.as_str()),
            Some("secret-service.resolve-mission")
        );
    }

    #[test]
    fn conflicting_unqualified_model_names_require_namespace() {
        let world_a = test_world("alpha", "alpha-default", "ping");
        let world_b = test_world("beta", "beta-default", "ping");

        let catalog = SkillCatalog::from_skills(vec![
            CatalogSkill {
                metadata: SkillMetadata::new("alpha", "alpha-default"),
                world: world_a,
            },
            CatalogSkill {
                metadata: SkillMetadata::new("beta", "beta-default"),
                world: world_b,
            },
        ])
        .unwrap();

        let registry = catalog.action_registry();
        assert!(registry.resolve_model_action("ping").is_none());
        assert_eq!(
            registry
                .resolve_model_action("alpha.ping")
                .map(|action| action.canonical_action_id.as_str()),
            Some("alpha.ping")
        );
        assert_eq!(
            registry
                .resolve_model_action("beta.ping")
                .map(|action| action.canonical_action_id.as_str()),
            Some("beta.ping")
        );
    }

    fn test_world(skill_name: &str, world_name: &str, action_name: &str) -> CanonicalWorld {
        CanonicalWorld {
            package: Some(crate::CanonicalPackageRef {
                namespace: "tests".to_owned(),
                name: skill_name.to_owned(),
                version: None,
            }),
            name: world_name.to_owned(),
            docs: None,
            imports: Vec::new(),
            exports: vec![crate::CanonicalInterface {
                name: format!("{skill_name}-exports"),
                docs: None,
                functions: vec![crate::CanonicalFunction {
                    name: action_name.to_owned(),
                    docs: None,
                    params: vec![crate::CanonicalParam {
                        name: "value".to_owned(),
                        ty: crate::CanonicalTypeRef::Primitive(
                            crate::CanonicalPrimitiveType::String,
                        ),
                    }],
                    result: crate::CanonicalFunctionResult::Scalar(
                        crate::CanonicalTypeRef::Primitive(crate::CanonicalPrimitiveType::String),
                    ),
                }],
                types: Vec::new(),
            }],
        }
    }
}
