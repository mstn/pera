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
    #[serde(alias = "agent")]
    pub agent_profile: EvalAgentSpec,
    #[serde(default)]
    pub history: Vec<EvalHistoryMessage>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EvalUserSpec {
    Scripted {
        task: String,
        known_info: String,
        initial_message: String,
    },
    Simulated {
        task: String,
        reason: String,
        known_info: String,
        unknown_info: String,
    },
}

impl EvalUserSpec {
    pub fn task(&self) -> &str {
        match self {
            Self::Scripted { task, .. } | Self::Simulated { task, .. } => task,
        }
    }

    pub fn known_info(&self) -> &str {
        match self {
            Self::Scripted { known_info, .. } | Self::Simulated { known_info, .. } => known_info,
        }
    }
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

pub fn load_eval_spec(
    path: &Path,
    overrides: &OverrideSet,
    selected_user: Option<&str>,
) -> Result<LoadedEvalSpec, EvalError> {
    let source = fs::read_to_string(path).map_err(|source| EvalError::ReadFile {
        path: path.to_path_buf(),
        source,
    })?;
    let mut raw: Value = serde_yaml::from_str(&source)
        .map_err(|error| EvalError::InvalidSpec(format!("{}: {error}", path.display())))?;
    overrides.apply(&mut raw)?;
    resolve_selected_user(&mut raw, selected_user)?;
    let spec: EvalSpec = serde_yaml::from_value(raw.clone())
        .map_err(|error| EvalError::InvalidSpec(format!("{}: {error}", path.display())))?;
    validate_eval_spec(&spec)?;
    Ok(LoadedEvalSpec { raw, spec })
}

fn resolve_selected_user(raw: &mut Value, selected_user: Option<&str>) -> Result<(), EvalError> {
    let root = raw.as_mapping_mut().ok_or_else(|| {
        EvalError::InvalidSpec("eval spec root must be a mapping".to_owned())
    })?;
    let Some(scenario) = root.get_mut(Value::String("scenario".to_owned())) else {
        return Ok(());
    };
    let scenario = scenario.as_mapping_mut().ok_or_else(|| {
        EvalError::InvalidSpec("scenario must be a mapping".to_owned())
    })?;

    let user_key = Value::String("user".to_owned());
    if scenario.contains_key(&user_key) {
        return Ok(());
    }

    let user_profile_key = Value::String("user_profile".to_owned());
    let interaction_modes_key = Value::String("interaction_modes".to_owned());
    if let Some(interaction_modes_value) = scenario.get(&interaction_modes_key) {
        let interaction_modes = interaction_modes_value.as_mapping().ok_or_else(|| {
            EvalError::InvalidSpec("scenario.interaction_modes must be a mapping".to_owned())
        })?;
        if interaction_modes.is_empty() {
            return Err(EvalError::InvalidSpec(
                "scenario.interaction_modes cannot be empty".to_owned(),
            ));
        }
        let user_profile = scenario
            .get(&user_profile_key)
            .and_then(Value::as_mapping)
            .ok_or_else(|| {
                EvalError::InvalidSpec(
                    "scenario.user_profile must be provided when using scenario.interaction_modes"
                        .to_owned(),
                )
            })?;

        let chosen_name = choose_named_variant(
            interaction_modes,
            selected_user,
            "scenario.interaction_modes",
        )?;
        let selected_mode = interaction_modes
            .get(Value::String(chosen_name.clone()))
            .and_then(Value::as_mapping)
            .ok_or_else(|| {
                EvalError::InvalidSpec(format!(
                    "scenario.interaction_modes.{} must be a mapping",
                    chosen_name
                ))
            })?;

        let mut merged = user_profile.clone();
        for (key, value) in selected_mode {
            merged.insert(key.clone(), value.clone());
        }
        scenario.insert(user_key, Value::Mapping(merged));
        scenario.insert(
            Value::String("selected_user".to_owned()),
            Value::String(chosen_name),
        );
        return Ok(());
    }

    let users_key = Value::String("users".to_owned());
    let Some(users_value) = scenario.get(&users_key) else {
        return Ok(());
    };
    let users = users_value.as_mapping().ok_or_else(|| {
        EvalError::InvalidSpec("scenario.users must be a mapping".to_owned())
    })?;
    if users.is_empty() {
        return Err(EvalError::InvalidSpec(
            "scenario.users cannot be empty".to_owned(),
        ));
    }

    let chosen_name = choose_named_variant(users, selected_user, "scenario.users")?;

    let chosen_value = users
        .get(Value::String(chosen_name.clone()))
        .cloned()
        .ok_or_else(|| {
            EvalError::InvalidSpec(format!(
                "failed to resolve scenario.users.{}",
                chosen_name
            ))
        })?;
    scenario.insert(user_key, chosen_value);
    scenario.insert(
        Value::String("selected_user".to_owned()),
        Value::String(chosen_name),
    );

    Ok(())
}

fn choose_named_variant(
    values: &Mapping,
    selected_user: Option<&str>,
    field_name: &str,
) -> Result<String, EvalError> {
    if let Some(name) = selected_user {
        let key = Value::String(name.to_owned());
        if !values.contains_key(&key) {
            let available = values
                .keys()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(", ");
            return Err(EvalError::InvalidSpec(format!(
                "{} does not contain '{}' (available: {})",
                field_name, name, available
            )));
        }
        return Ok(name.to_owned());
    }

    if values.len() == 1 {
        return values
            .keys()
            .next()
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
            .ok_or_else(|| {
                EvalError::InvalidSpec(format!(
                    "{} keys must be strings",
                    field_name
                ))
            });
    }

    let available = values
        .keys()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>()
        .join(", ");
    Err(EvalError::InvalidSpec(format!(
        "spec defines multiple {}; select one with --user (available: {})",
        field_name, available
    )))
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
    if spec.scenario.user.task().trim().is_empty() {
        return Err(EvalError::InvalidSpec(
            "scenario.user.task cannot be empty".to_owned(),
        ));
    }
    if let EvalUserSpec::Scripted {
        initial_message, ..
    } = &spec.scenario.user
    {
        if initial_message.trim().is_empty() {
            return Err(EvalError::InvalidSpec(
                "scenario.user.initial_message cannot be empty".to_owned(),
            ));
        }
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

#[cfg(test)]
mod tests {
    use std::fs;

    use super::load_eval_spec;
    use crate::overrides::OverrideSet;

    #[test]
    fn load_eval_spec_selects_requested_user_variant() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("eval.yaml");
        fs::write(
            &path,
            r#"
schema_version: 1
id: demo
runtime:
  output_folder: .pera
scenario:
  purpose: demo
  user_profile:
    task: shared task
    reason: because
    known_info: known
    unknown_info: unknown
  interaction_modes:
    scripted:
      kind: scripted
      initial_message: hi
    simulated:
      kind: simulated
  agent: {}
evaluation:
  criteria:
    - type: final_message_required
"#,
        )
        .unwrap();

        let loaded =
            load_eval_spec(&path, &OverrideSet::default(), Some("simulated")).unwrap();
        assert_eq!(loaded.spec.scenario.user.task(), "shared task");
        assert_eq!(
            loaded
                .raw
                .as_mapping()
                .unwrap()
                .get(serde_yaml::Value::String("scenario".to_owned()))
                .and_then(|value| value.as_mapping())
                .and_then(|mapping| mapping.get(serde_yaml::Value::String("selected_user".to_owned())))
                .and_then(|value| value.as_str()),
            Some("simulated")
        );
    }

    #[test]
    fn load_eval_spec_requires_user_selection_when_multiple_variants_exist() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("eval.yaml");
        fs::write(
            &path,
            r#"
schema_version: 1
id: demo
runtime:
  output_folder: .pera
scenario:
  purpose: demo
  user_profile:
    task: shared task
    reason: because
    known_info: known
    unknown_info: unknown
  interaction_modes:
    scripted:
      kind: scripted
      initial_message: hi
    simulated:
      kind: simulated
  agent: {}
evaluation:
  criteria:
    - type: final_message_required
"#,
        )
        .unwrap();

        let error = load_eval_spec(&path, &OverrideSet::default(), None).unwrap_err();
        assert!(error
            .to_string()
            .contains("select one with --user"));
    }
}
