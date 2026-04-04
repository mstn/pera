use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_yaml::{Mapping, Value};

use super::overrides::OverrideSet;
use crate::error::CliError;

#[derive(Debug, Clone)]
pub struct LoadedEvalSpec {
    pub raw: Value,
    pub spec: EvalSpec,
}

impl LoadedEvalSpec {
    pub fn override_output_folder(&mut self, output_folder: PathBuf) -> Result<(), CliError> {
        self.spec.runtime.output_folder = output_folder.clone();

        let root = self.raw.as_mapping_mut().ok_or_else(|| {
            CliError::UnexpectedStateOwned("resolved eval spec root must be an object".to_owned())
        })?;
        let runtime = root
            .entry(Value::String("runtime".to_owned()))
            .or_insert_with(|| Value::Mapping(Mapping::new()));
        let runtime_mapping = runtime.as_mapping_mut().ok_or_else(|| {
            CliError::UnexpectedStateOwned(
                "resolved eval spec runtime must be an object".to_owned(),
            )
        })?;
        runtime_mapping.insert(
            Value::String("output_folder".to_owned()),
            Value::String(output_folder.display().to_string()),
        );

        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct EvalSpec {
    pub id: String,
    pub runtime: EvalRuntimeSpec,
}

#[derive(Debug, Clone, Deserialize)]
pub struct EvalRuntimeSpec {
    pub output_folder: PathBuf,
}

pub fn load_eval_spec(path: &Path, overrides: &OverrideSet) -> Result<LoadedEvalSpec, CliError> {
    let source = fs::read_to_string(path).map_err(|source| CliError::ReadFile {
        path: path.to_path_buf(),
        source,
    })?;
    let mut raw: Value = serde_yaml::from_str(&source).map_err(|error| {
        CliError::UnexpectedStateOwned(format!("invalid eval spec {}: {error}", path.display()))
    })?;
    overrides.apply(&mut raw)?;
    let spec: EvalSpec = serde_yaml::from_value(raw.clone()).map_err(|error| {
        CliError::UnexpectedStateOwned(format!(
            "invalid resolved eval spec {}: {error}",
            path.display()
        ))
    })?;
    validate_eval_spec(&spec)?;
    Ok(LoadedEvalSpec { raw, spec })
}

fn validate_eval_spec(spec: &EvalSpec) -> Result<(), CliError> {
    if spec.id.trim().is_empty() {
        return Err(CliError::InvalidArguments("spec id cannot be empty"));
    }

    if spec.runtime.output_folder.as_os_str().is_empty() {
        return Err(CliError::InvalidArguments(
            "spec runtime.output_folder cannot be empty",
        ));
    }

    Ok(())
}
