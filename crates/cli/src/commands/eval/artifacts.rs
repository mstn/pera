use std::fs;
use std::path::{Path, PathBuf};

use chrono::Utc;
use serde::Serialize;

use super::overrides::OverrideSet;
use super::spec::LoadedEvalSpec;
use crate::error::CliError;

#[derive(Debug, Clone)]
pub struct RunArtifacts {
    pub run_dir: PathBuf,
    pub resolved_spec_path: PathBuf,
    pub manifest_path: PathBuf,
}

#[derive(Debug, Serialize)]
struct RunManifest<'a> {
    mode: &'a str,
    spec_path: String,
    run_dir: String,
    overrides: Vec<ManifestOverride>,
    status: &'a str,
}

#[derive(Debug, Serialize)]
struct ManifestOverride {
    path: String,
    value: serde_json::Value,
}

pub fn create_run_artifacts(
    output_root: &Path,
    name: &str,
    mode: &str,
    spec_path: &Path,
    loaded: &LoadedEvalSpec,
    overrides: &OverrideSet,
) -> Result<RunArtifacts, CliError> {
    fs::create_dir_all(output_root).map_err(|source| CliError::CreateDir {
        path: output_root.to_path_buf(),
        source,
    })?;

    let run_dir = unique_run_dir(output_root, name);
    fs::create_dir_all(&run_dir).map_err(|source| CliError::CreateDir {
        path: run_dir.clone(),
        source,
    })?;

    let resolved_spec_path = run_dir.join("spec.resolved.yaml");
    let manifest_path = run_dir.join("run.json");

    let resolved_bytes = serde_yaml::to_string(&loaded.raw).map_err(|error| {
        CliError::UnexpectedStateOwned(format!("failed to serialize resolved eval spec: {error}"))
    })?;
    fs::write(&resolved_spec_path, resolved_bytes).map_err(|source| CliError::WriteFile {
        path: resolved_spec_path.clone(),
        source,
    })?;

    let manifest = RunManifest {
        mode,
        spec_path: spec_path.display().to_string(),
        run_dir: run_dir.display().to_string(),
        overrides: overrides
            .entries()
            .map(|(path, value)| ManifestOverride {
                path: path.join("."),
                value: serde_json::to_value(value).unwrap_or(serde_json::Value::Null),
            })
            .collect(),
        status: "initialized",
    };
    let manifest_bytes = serde_json::to_vec_pretty(&manifest).map_err(|error| {
        CliError::UnexpectedStateOwned(format!("failed to serialize eval run manifest: {error}"))
    })?;
    fs::write(&manifest_path, manifest_bytes).map_err(|source| CliError::WriteFile {
        path: manifest_path.clone(),
        source,
    })?;

    Ok(RunArtifacts {
        run_dir,
        resolved_spec_path,
        manifest_path,
    })
}

fn unique_run_dir(output_root: &Path, name: &str) -> PathBuf {
    let timestamp = Utc::now().format("%Y%m%dT%H%M%SZ");
    let base = format!("{timestamp}-{}", slugify(name));
    let first = output_root.join(&base);
    if !first.exists() {
        return first;
    }

    let mut counter = 2usize;
    loop {
        let candidate = output_root.join(format!("{base}-{counter}"));
        if !candidate.exists() {
            return candidate;
        }
        counter += 1;
    }
}

fn slugify(value: &str) -> String {
    let mut slug = String::new();
    let mut last_was_separator = false;

    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
            last_was_separator = false;
        } else if !last_was_separator {
            slug.push('-');
            last_was_separator = true;
        }
    }

    let slug = slug.trim_matches('-').to_owned();
    if slug.is_empty() {
        "eval".to_owned()
    } else {
        slug
    }
}
