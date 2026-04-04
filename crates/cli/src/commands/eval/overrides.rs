use serde_json::Value as JsonValue;
use serde_yaml::{Mapping, Value};

use crate::error::CliError;

#[derive(Debug, Clone, Default)]
pub struct OverrideSet {
    entries: Vec<OverrideEntry>,
}

#[derive(Debug, Clone)]
struct OverrideEntry {
    path: Vec<String>,
    value: Value,
}

impl OverrideSet {
    pub fn from_cli(set_values: &[String], set_json_values: &[String]) -> Result<Self, CliError> {
        let mut entries = Vec::new();

        for raw in set_values {
            let (path, value) = split_assignment(raw)?;
            entries.push(OverrideEntry {
                path: parse_path(path)?,
                value: parse_scalar_value(value),
            });
        }

        for raw in set_json_values {
            let (path, value) = split_assignment(raw)?;
            let parsed: JsonValue = serde_json::from_str(value).map_err(|error| {
                CliError::UnexpectedStateOwned(format!(
                    "invalid JSON override for '{path}': {error}"
                ))
            })?;
            entries.push(OverrideEntry {
                path: parse_path(path)?,
                value: serde_yaml::to_value(parsed).map_err(|error| {
                    CliError::UnexpectedStateOwned(format!(
                        "failed to convert JSON override for '{path}': {error}"
                    ))
                })?,
            });
        }

        Ok(Self { entries })
    }

    pub fn apply(&self, root: &mut Value) -> Result<(), CliError> {
        for entry in &self.entries {
            apply_override(root, &entry.path, entry.value.clone())?;
        }
        Ok(())
    }

    pub fn entries(&self) -> impl Iterator<Item = (&[String], &Value)> {
        self.entries.iter().map(|entry| (entry.path.as_slice(), &entry.value))
    }
}

fn split_assignment(raw: &str) -> Result<(&str, &str), CliError> {
    raw.split_once('=').ok_or(CliError::InvalidArguments(
        "override values must use PATH=VALUE syntax",
    ))
}

fn parse_path(raw: &str) -> Result<Vec<String>, CliError> {
    let path = raw
        .split('.')
        .map(str::trim)
        .filter(|segment| !segment.is_empty())
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();

    if path.is_empty() {
        return Err(CliError::InvalidArguments("override path cannot be empty"));
    }

    Ok(path)
}

fn parse_scalar_value(raw: &str) -> Value {
    match raw {
        "null" => Value::Null,
        "true" => Value::Bool(true),
        "false" => Value::Bool(false),
        _ => {
            if let Ok(value) = raw.parse::<i64>() {
                return serde_yaml::to_value(value).unwrap_or(Value::String(raw.to_owned()));
            }
            if let Ok(value) = raw.parse::<f64>() {
                return serde_yaml::to_value(value).unwrap_or(Value::String(raw.to_owned()));
            }
            Value::String(raw.to_owned())
        }
    }
}

fn apply_override(root: &mut Value, path: &[String], value: Value) -> Result<(), CliError> {
    let mut current = root;

    for segment in &path[..path.len() - 1] {
        if current.is_null() {
            *current = Value::Mapping(Mapping::new());
        }

        let mapping = current.as_mapping_mut().ok_or_else(|| {
            CliError::UnexpectedStateOwned(format!(
                "override path '{}' traverses a non-object value",
                path.join(".")
            ))
        })?;

        current = mapping
            .entry(Value::String(segment.clone()))
            .or_insert_with(|| Value::Mapping(Mapping::new()));
    }

    let mapping = current.as_mapping_mut().ok_or_else(|| {
        CliError::UnexpectedStateOwned(format!(
            "override path '{}' does not target an object field",
            path.join(".")
        ))
    })?;
    mapping.insert(Value::String(path[path.len() - 1].clone()), value);
    Ok(())
}
