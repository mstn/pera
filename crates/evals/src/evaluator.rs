use async_trait::async_trait;
use pera_orchestrator::{
    EvalResult, Evaluator, EvaluatorError, FinishReason, ParticipantId, Trajectory, TrajectoryEvent,
};
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
                    ordered,
                    allow_extra_actions,
                } => {
                    eprintln!(
                        "[eval] criterion action_sequence expected_actions={} ordered={} allow_extra_actions={}",
                        actions.len(),
                        ordered,
                        allow_extra_actions
                    );
                    if !matches_action_sequence(
                        actions,
                        &requested_actions,
                        *ordered,
                        *allow_extra_actions,
                    ) {
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
                EvalCriterionSpec::FinalMessageRequired => {}
                EvalCriterionSpec::ForbidFinishReason { .. } => {}
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

pub fn evaluate_run_criteria(
    spec: &EvalSpec,
    finish_reason: &FinishReason,
    final_agent_message: Option<&str>,
) -> Vec<String> {
    let mut failures = Vec::new();
    for criterion in &spec.evaluation.criteria {
        match criterion {
            EvalCriterionSpec::FinalMessageRequired => {
                eprintln!("[eval] criterion final_message_required");
                let has_final_message = final_agent_message
                    .map(str::trim)
                    .map(|content| !content.is_empty())
                    .unwrap_or(false);
                if !has_final_message {
                    failures.push("final_message_required failed: missing final agent message".to_owned());
                } else {
                    eprintln!("[eval] criterion passed: final_message_required");
                }
            }
            EvalCriterionSpec::ForbidFinishReason { finish_reason: forbidden } => {
                let actual = finish_reason_name(finish_reason);
                eprintln!(
                    "[eval] criterion forbid_finish_reason forbidden={} actual={}",
                    forbidden, actual
                );
                if actual == forbidden {
                    failures.push(format!(
                        "forbid_finish_reason failed: actual finish reason was '{}'",
                        actual
                    ));
                } else {
                    eprintln!("[eval] criterion passed: forbid_finish_reason");
                }
            }
            EvalCriterionSpec::ActionSequence { .. } | EvalCriterionSpec::ActionCount { .. } => {}
        }
    }
    failures
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
    ordered: bool,
    allow_extra_actions: bool,
) -> bool {
    if !ordered {
        return matches_action_set(expected, actual, allow_extra_actions);
    }

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

fn matches_action_set(
    expected: &[EvalExpectedActionSpec],
    actual: &[SerializedAction],
    allow_extra_actions: bool,
) -> bool {
    if !allow_extra_actions && expected.len() != actual.len() {
        return false;
    }

    let mut used = vec![false; actual.len()];
    for expected_action in expected {
        let mut matched_index = None;
        for (index, actual_action) in actual.iter().enumerate() {
            if used[index] {
                continue;
            }
            if action_matches(expected_action, actual_action) {
                matched_index = Some(index);
                break;
            }
        }
        let Some(index) = matched_index else {
            return false;
        };
        used[index] = true;
    }

    allow_extra_actions || used.into_iter().all(|matched| matched)
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

fn finish_reason_name(finish_reason: &FinishReason) -> &'static str {
    match finish_reason {
        FinishReason::ParticipantsFinished => "participants_finished",
        FinishReason::ParticipantFinished { .. } => "participant_finished",
        FinishReason::ParticipantCompletedLoop { .. } => "participant_completed_loop",
        FinishReason::StepLimitExceeded => "step_limit_exceeded",
        FinishReason::AgentLoopStepLimitExceeded { .. } => "agent_loop_step_limit_exceeded",
        FinishReason::ActionLimitExceeded => "action_limit_exceeded",
        FinishReason::FailedActionLimitExceeded { .. } => "failed_action_limit_exceeded",
        FinishReason::MessageLimitExceeded => "message_limit_exceeded",
        FinishReason::TimeLimitExceeded => "time_limit_exceeded",
        FinishReason::ParticipantError { .. } => "participant_error",
        FinishReason::EnvironmentError(_) => "environment_error",
        FinishReason::EnvironmentTerminated(_) => "environment_terminated",
        FinishReason::Deadlocked => "deadlocked",
    }
}
