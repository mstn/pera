use async_trait::async_trait;
use pera_orchestrator::{EvalResult, Evaluator, EvaluatorError, ParticipantId, Trajectory, TrajectoryEvent};
use serde_yaml::Value;

use crate::execution::{SerializedAction, SerializedOutcome};
use crate::spec::{EvalCriterionSpec, EvalExpectedActionSpec, EvalSpec};

pub trait EvalActionAdapter<A, U>: Clone + Send + Sync + 'static {
    fn serialize_action(&self, action: &A) -> SerializedAction;
    fn serialize_outcome(&self, outcome: &U) -> SerializedOutcome;
}

#[derive(Debug, Clone)]
pub struct SpecEvaluator<T> {
    spec: EvalSpec,
    action_adapter: T,
}

impl<T> SpecEvaluator<T> {
    pub fn new(spec: EvalSpec, action_adapter: T) -> Self {
        Self { spec, action_adapter }
    }
}

#[async_trait]
impl<T, O, A, U> Evaluator<O, A, U> for SpecEvaluator<T>
where
    O: Clone + Send + Sync + 'static,
    A: Clone + Send + Sync + 'static,
    U: Clone + Send + Sync + 'static,
    T: EvalActionAdapter<A, U>,
{
    async fn evaluate(
        &self,
        _task: &pera_orchestrator::TaskSpec,
        trajectory: &Trajectory<O, A, U>,
    ) -> Result<EvalResult, EvaluatorError> {
        let requested_actions = trajectory
            .events
            .iter()
            .filter_map(|event| match event {
                TrajectoryEvent::ActionRequested {
                    participant: ParticipantId::Agent,
                    action,
                    ..
                } => Some(self.action_adapter.serialize_action(action)),
                _ => None,
            })
            .collect::<Vec<_>>();
        eprintln!(
            "[eval] evaluating trajectory criteria={} requested_actions={}",
            self.spec.evaluation.criteria.len(),
            requested_actions.len()
        );

        let mut failures = Vec::new();
        for criterion in &self.spec.evaluation.criteria {
            match criterion {
                EvalCriterionSpec::ActionSequence {
                    actions,
                    allow_extra_actions,
                } => {
                    eprintln!(
                        "[eval] criterion action_sequence expected_actions={} allow_extra_actions={}",
                        actions.len(),
                        allow_extra_actions
                    );
                    if !matches_action_sequence(actions, &requested_actions, *allow_extra_actions) {
                        eprintln!("[eval] criterion failed: action_sequence mismatch");
                        failures.push(format_action_sequence_failure(actions, &requested_actions));
                    } else {
                        eprintln!("[eval] criterion passed: action_sequence");
                    }
                }
                EvalCriterionSpec::ActionCount { action, min_count } => {
                    let actual_count = requested_actions
                        .iter()
                        .filter(|requested_action| requested_action.name == *action)
                        .count();
                    eprintln!(
                        "[eval] criterion action_count action={} min_count={} actual_count={}",
                        action,
                        min_count,
                        actual_count
                    );
                    if actual_count < *min_count {
                        failures.push(format!(
                            "action_count mismatch for '{}': expected at least {}, actual {}",
                            action, min_count, actual_count
                        ));
                    } else {
                        eprintln!("[eval] criterion passed: action_count");
                    }
                }
            }
        }

        Ok(EvalResult {
            passed: failures.is_empty(),
            score: Some(if failures.is_empty() { 1.0 } else { 0.0 }),
            summary: if failures.is_empty() {
                Some("all criteria passed".to_owned())
            } else {
                Some(failures.join("\n"))
            },
        })
    }
}

pub fn trajectory_trace_events<T, O, A, U>(
    trajectory: &Trajectory<O, A, U>,
    action_adapter: &T,
) -> Vec<crate::execution::EvalTraceEvent>
where
    T: EvalActionAdapter<A, U>,
{
    trajectory
        .events
        .iter()
        .filter_map(|event| match event {
            TrajectoryEvent::ParticipantMessage {
                participant: ParticipantId::User,
                content,
            } => Some(crate::execution::EvalTraceEvent::UserMessage {
                content: content.clone(),
            }),
            TrajectoryEvent::ParticipantMessage {
                participant: ParticipantId::Agent,
                content,
            } => Some(crate::execution::EvalTraceEvent::AgentMessage {
                content: content.clone(),
            }),
            TrajectoryEvent::ActionRequested { action, .. } => {
                Some(crate::execution::EvalTraceEvent::ActionRequested {
                    action: action_adapter.serialize_action(action),
                })
            }
            TrajectoryEvent::ActionCompleted { outcome, .. } => {
                Some(crate::execution::EvalTraceEvent::ActionCompleted {
                    outcome: action_adapter.serialize_outcome(outcome),
                })
            }
            TrajectoryEvent::ActionFailed { error, .. } => {
                Some(crate::execution::EvalTraceEvent::ActionFailed {
                    message: error.detail.clone(),
                })
            }
            _ => None,
        })
        .collect()
}

fn matches_action_sequence(
    expected: &[EvalExpectedActionSpec],
    actual: &[SerializedAction],
    allow_extra_actions: bool,
) -> bool {
    if allow_extra_actions {
        let mut cursor = 0usize;
        for expected_action in expected {
            let mut matched = false;
            while cursor < actual.len() {
                if action_matches(expected_action, &actual[cursor]) {
                    matched = true;
                    cursor += 1;
                    break;
                }
                cursor += 1;
            }
            if !matched {
                return false;
            }
        }
        true
    } else {
        expected.len() == actual.len()
            && expected
                .iter()
                .zip(actual.iter())
                .all(|(expected_action, actual_action)| action_matches(expected_action, actual_action))
    }
}

fn action_matches(expected: &EvalExpectedActionSpec, actual: &SerializedAction) -> bool {
    expected.action == actual.name && arguments_match(expected.arguments.as_ref(), actual.arguments.as_ref())
}

fn arguments_match(expected: Option<&Value>, actual: Option<&Value>) -> bool {
    let Some(expected) = expected else {
        return true;
    };
    let Some(expected_map) = expected.as_mapping() else {
        return actual == Some(expected);
    };
    let Some(actual_map) = actual.and_then(Value::as_mapping) else {
        return false;
    };
    expected_map.iter().all(|(key, expected_value)| {
        actual_map.get(key).map(|actual_value| actual_value == expected_value).unwrap_or(false)
    })
}

fn format_action_sequence_failure(
    expected: &[EvalExpectedActionSpec],
    actual: &[SerializedAction],
) -> String {
    format!(
        "action_sequence mismatch: expected [{}], actual [{}]",
        expected
            .iter()
            .map(|item| item.action.as_str())
            .collect::<Vec<_>>()
            .join(", "),
        actual
            .iter()
            .map(|item| item.name.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    )
}
