use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_yaml::{Mapping, Value};

use crate::error::EvalError;
use crate::overrides::OverrideSet;

#[derive(Debug, Clone)]
pub struct LoadedEvalSpec {
    pub raw: Value,
    pub spec: EvalSpec,
}

impl LoadedEvalSpec {
    pub fn override_output_folder(&mut self, output_folder: PathBuf) -> Result<(), EvalError> {
        self.spec.runtime.output_folder = output_folder.clone();

        let root = self.raw.as_mapping_mut().ok_or_else(|| {
            EvalError::Internal("resolved eval spec root must be an object".to_owned())
        })?;
        let runtime = root
            .entry(Value::String("runtime".to_owned()))
            .or_insert_with(|| Value::Mapping(Mapping::new()));
        let runtime_mapping = runtime.as_mapping_mut().ok_or_else(|| {
            EvalError::Internal("resolved eval spec runtime must be an object".to_owned())
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
    #[serde(default)]
    pub schema_version: Option<u32>,
    pub id: String,
    #[serde(default)]
    pub description: Option<String>,
    pub runtime: EvalRuntimeSpec,
    pub scenario: EvalScenarioSpec,
    pub evaluation: EvalEvaluationSpec,
    #[serde(default)]
    pub optimization: Option<EvalOptimizationSpec>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct EvalRuntimeSpec {
    pub output_folder: PathBuf,
    #[serde(default)]
    pub skill_sources: Vec<EvalSkillSourceSpec>,
    #[serde(default)]
    pub catalog: Vec<EvalCatalogSkillSpec>,
    #[serde(default)]
    pub active_skills: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct EvalSkillSourceSpec {
    pub id: String,
    pub path: PathBuf,
}

#[derive(Debug, Clone, Deserialize)]
pub struct EvalCatalogSkillSpec {
    pub skill: String,
    pub source: String,
    #[serde(default)]
    pub profile: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct EvalScenarioSpec {
    pub purpose: String,
    pub user: EvalUserSpec,
    pub agent: EvalAgentSpec,
    #[serde(default)]
    pub history: Vec<EvalHistoryMessage>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct EvalUserSpec {
    #[serde(default)]
    pub mode: EvalUserMode,
    pub task: String,
    pub reason: String,
    pub known_info: String,
    pub unknown_info: String,
    pub example_messages: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EvalUserMode {
    #[default]
    Scripted,
    Simulated,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct EvalAgentSpec {
    #[serde(default)]
    pub persona: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct EvalHistoryMessage {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct EvalEvaluationSpec {
    pub criteria: Vec<EvalCriterionSpec>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EvalCriterionSpec {
    ActionSequence {
        actions: Vec<EvalExpectedActionSpec>,
        #[serde(default = "default_true")]
        ordered: bool,
        #[serde(default)]
        allow_extra_actions: bool,
    },
    ActionCount {
        action: String,
        min_count: usize,
    },
    LlmJudge {
        rubric: String,
        #[serde(default)]
        model: Option<String>,
    },
    FinalMessageRequired,
    ForbidFinishReason {
        finish_reason: String,
    },
}

#[derive(Debug, Clone, Deserialize)]
pub struct EvalExpectedActionSpec {
    pub action: String,
    #[serde(default)]
    pub arguments: Option<Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct EvalOptimizationSpec {
    #[serde(default)]
    pub targets: Vec<EvalOptimizationTargetSpec>,
    #[serde(default)]
    pub max_epochs: Option<usize>,
    #[serde(default)]
    pub early_stop_on_pass: Option<bool>,
}

#[derive(Debug, Clone, Deserialize, serde::Serialize)]
pub struct EvalOptimizationTargetSpec {
    pub kind: String,
    #[serde(default)]
    pub prompt: Option<String>,
    #[serde(default)]
    pub skill: Option<String>,
    #[serde(default)]
    pub field: Option<String>,
}

fn default_true() -> bool {
    true
}

pub fn load_eval_spec(path: &Path, overrides: &OverrideSet) -> Result<LoadedEvalSpec, EvalError> {
    let source = fs::read_to_string(path).map_err(|source| EvalError::ReadFile {
        path: path.to_path_buf(),
        source,
    })?;
    let mut raw: Value = serde_yaml::from_str(&source)
        .map_err(|error| EvalError::InvalidSpec(format!("{}: {error}", path.display())))?;
    overrides.apply(&mut raw)?;
    let spec: EvalSpec = serde_yaml::from_value(raw.clone())
        .map_err(|error| EvalError::InvalidSpec(format!("{}: {error}", path.display())))?;
    validate_eval_spec(&spec)?;
    Ok(LoadedEvalSpec { raw, spec })
}

fn validate_eval_spec(spec: &EvalSpec) -> Result<(), EvalError> {
    if spec.id.trim().is_empty() {
        return Err(EvalError::InvalidSpec("spec id cannot be empty".to_owned()));
    }

    if spec.runtime.output_folder.as_os_str().is_empty() {
        return Err(EvalError::InvalidSpec(
            "spec runtime.output_folder cannot be empty".to_owned(),
        ));
    }
    if spec.scenario.purpose.trim().is_empty() {
        return Err(EvalError::InvalidSpec(
            "scenario.purpose cannot be empty".to_owned(),
        ));
    }
    if spec.scenario.user.task.trim().is_empty() {
        return Err(EvalError::InvalidSpec(
            "scenario.user.task cannot be empty".to_owned(),
        ));
    }
    if spec.evaluation.criteria.is_empty() {
        return Err(EvalError::InvalidSpec(
            "evaluation.criteria cannot be empty".to_owned(),
        ));
    }

    let mut source_ids = std::collections::BTreeSet::new();
    for source in &spec.runtime.skill_sources {
        if source.id.trim().is_empty() {
            return Err(EvalError::InvalidSpec(
                "runtime.skill_sources.id cannot be empty".to_owned(),
            ));
        }
        if !source_ids.insert(source.id.clone()) {
            return Err(EvalError::InvalidSpec(format!(
                "duplicate runtime.skill_sources id '{}'",
                source.id
            )));
        }
    }

    for skill in &spec.runtime.catalog {
        if skill.skill.trim().is_empty() {
            return Err(EvalError::InvalidSpec(
                "runtime.catalog.skill cannot be empty".to_owned(),
            ));
        }
        if !source_ids.contains(&skill.source) {
            return Err(EvalError::InvalidSpec(format!(
                "runtime.catalog skill '{}' references unknown source '{}'",
                skill.skill, skill.source
            )));
        }
    }

    let catalog_skill_names = spec
        .runtime
        .catalog
        .iter()
        .map(|skill| skill.skill.clone())
        .collect::<std::collections::BTreeSet<_>>();
    for skill_name in &spec.runtime.active_skills {
        if !catalog_skill_names.contains(skill_name) {
            return Err(EvalError::InvalidSpec(format!(
                "runtime.active_skills contains '{}' which is not present in runtime.catalog",
                skill_name
            )));
        }
    }

    for criterion in &spec.evaluation.criteria {
        match criterion {
            EvalCriterionSpec::ActionSequence { actions, .. } => {
                if actions.is_empty() {
                    return Err(EvalError::InvalidSpec(
                        "action_sequence criteria require at least one action".to_owned(),
                    ));
                }
                for action in actions {
                    if action.action.trim().is_empty() {
                        return Err(EvalError::InvalidSpec(
                            "action_sequence action name cannot be empty".to_owned(),
                        ));
                    }
                }
            }
            EvalCriterionSpec::ActionCount { action, min_count } => {
                if action.trim().is_empty() {
                    return Err(EvalError::InvalidSpec(
                        "action_count action name cannot be empty".to_owned(),
                    ));
                }
                if *min_count == 0 {
                    return Err(EvalError::InvalidSpec(
                        "action_count min_count must be greater than zero".to_owned(),
                    ));
                }
            }
            EvalCriterionSpec::LlmJudge { rubric, model } => {
                if rubric.trim().is_empty() {
                    return Err(EvalError::InvalidSpec(
                        "llm_judge rubric cannot be empty".to_owned(),
                    ));
                }
                if let Some(model) = model {
                    if model.trim().is_empty() {
                        return Err(EvalError::InvalidSpec(
                            "llm_judge model cannot be empty".to_owned(),
                        ));
                    }
                }
            }
            EvalCriterionSpec::FinalMessageRequired => {}
            EvalCriterionSpec::ForbidFinishReason { finish_reason } => {
                if finish_reason.trim().is_empty() {
                    return Err(EvalError::InvalidSpec(
                        "forbid_finish_reason finish_reason cannot be empty".to_owned(),
                    ));
                }
            }
        }
    }

    Ok(())
}
