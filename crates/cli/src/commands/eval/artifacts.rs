use std::fs;
use std::path::{Path, PathBuf};

use chrono::Utc;
use pera_evals::{EvalJudgeResult, EvalRunResult, LoadedEvalSpec, OverrideSet};
use serde::Serialize;

use crate::error::CliError;

#[derive(Debug, Clone)]
pub struct RunArtifacts {
    pub run_dir: PathBuf,
    pub resolved_spec_path: PathBuf,
    pub manifest_path: PathBuf,
    pub result_path: PathBuf,
    pub trajectory_path: PathBuf,
    mode: String,
    spec_path: String,
    overrides: Vec<ManifestOverride>,
}

#[derive(Debug, Clone, Serialize)]
struct RunManifest<'a> {
    mode: &'a str,
    spec_path: String,
    run_dir: String,
    overrides: &'a [ManifestOverride],
    status: &'a str,
}

#[derive(Debug, Clone, Serialize)]
struct ManifestOverride {
    path: String,
    value: serde_json::Value,
}

pub fn create_run_artifacts(
    project_root: &Path,
    name: &str,
    mode: &str,
    spec_path: &Path,
    loaded: &LoadedEvalSpec,
    overrides: &OverrideSet,
) -> Result<RunArtifacts, CliError> {
    let output_root = project_root.join("evals");
    fs::create_dir_all(&output_root).map_err(|source| CliError::CreateDir {
        path: output_root.clone(),
        source,
    })?;

    let run_dir = unique_run_dir(&output_root, name);
    fs::create_dir_all(&run_dir).map_err(|source| CliError::CreateDir {
        path: run_dir.clone(),
        source,
    })?;

    let resolved_spec_path = run_dir.join("spec.resolved.yaml");
    let manifest_path = run_dir.join("run.json");
    let result_path = run_dir.join("result.json");
    let trajectory_path = run_dir.join("trajectory.jsonl");

    let resolved_bytes = serde_yaml::to_string(&loaded.raw).map_err(|error| {
        CliError::UnexpectedStateOwned(format!("failed to serialize resolved eval spec: {error}"))
    })?;
    fs::write(&resolved_spec_path, resolved_bytes).map_err(|source| CliError::WriteFile {
        path: resolved_spec_path.clone(),
        source,
    })?;

    let manifest_overrides: Vec<ManifestOverride> = overrides
        .entries()
        .map(|(path, value)| ManifestOverride {
            path: path.join("."),
            value: serde_json::to_value(value).unwrap_or(serde_json::Value::Null),
        })
        .collect();

    let artifacts = RunArtifacts {
        run_dir,
        resolved_spec_path,
        manifest_path,
        result_path,
        trajectory_path,
        mode: mode.to_owned(),
        spec_path: spec_path.display().to_string(),
        overrides: manifest_overrides,
    };
    write_run_manifest(&artifacts, "initialized")?;

    Ok(artifacts)
}

#[derive(Debug, Serialize)]
struct PersistedRunResult<'a> {
    passed: bool,
    finish_reason: String,
    evaluation: PersistedEvalResult<'a>,
    final_agent_message: &'a Option<String>,
    judge_results: &'a [EvalJudgeResult],
    trace: &'a [pera_evals::EvalTraceEvent],
    workspace_root: String,
}

#[derive(Debug, Serialize)]
struct PersistedEvalResult<'a> {
    passed: bool,
    score: Option<f64>,
    summary: &'a Option<String>,
}

pub fn write_run_result(
    artifacts: &RunArtifacts,
    result: &EvalRunResult,
) -> Result<(), CliError> {
    let value = PersistedRunResult {
        passed: result.passed,
        finish_reason: format!("{:?}", result.finish_reason),
        evaluation: PersistedEvalResult {
            passed: result.evaluation.passed,
            score: result.evaluation.score,
            summary: &result.evaluation.summary,
        },
        final_agent_message: &result.final_agent_message,
        judge_results: &result.judge_results,
        trace: &result.trace,
        workspace_root: result.workspace.root.display().to_string(),
    };
    let bytes = serde_json::to_vec_pretty(&value).map_err(|error| {
        CliError::UnexpectedStateOwned(format!("failed to serialize eval run result: {error}"))
    })?;
    fs::write(&artifacts.result_path, bytes).map_err(|source| CliError::WriteFile {
        path: artifacts.result_path.clone(),
        source,
    })?;
    write_trajectory(artifacts, result)?;
    write_run_manifest(artifacts, "completed")
}

pub fn write_run_failed(artifacts: &RunArtifacts) -> Result<(), CliError> {
    write_run_manifest(artifacts, "failed")
}

fn write_run_manifest(artifacts: &RunArtifacts, status: &str) -> Result<(), CliError> {
    let manifest = RunManifest {
        mode: &artifacts.mode,
        spec_path: artifacts.spec_path.clone(),
        run_dir: artifacts.run_dir.display().to_string(),
        overrides: &artifacts.overrides,
        status,
    };
    let manifest_bytes = serde_json::to_vec_pretty(&manifest).map_err(|error| {
        CliError::UnexpectedStateOwned(format!("failed to serialize eval run manifest: {error}"))
    })?;
    fs::write(&artifacts.manifest_path, manifest_bytes).map_err(|source| CliError::WriteFile {
        path: artifacts.manifest_path.clone(),
        source,
    })
}

fn write_trajectory(
    artifacts: &RunArtifacts,
    result: &EvalRunResult,
) -> Result<(), CliError> {
    let mut bytes = Vec::new();
    for event in &result.trajectory {
        serde_json::to_writer(&mut bytes, event).map_err(|error| {
            CliError::UnexpectedStateOwned(format!(
                "failed to serialize eval trajectory event: {error}"
            ))
        })?;
        bytes.push(b'\n');
    }

    fs::write(&artifacts.trajectory_path, bytes).map_err(|source| CliError::WriteFile {
        path: artifacts.trajectory_path.clone(),
        source,
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
