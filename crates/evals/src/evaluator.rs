use async_trait::async_trait;
use pera_orchestrator::{
    EvalResult, Evaluator, EvaluatorError, FinishReason, ParticipantId, Trajectory, TrajectoryEvent,
};
use serde::{Deserialize, Serialize};
use serde_yaml::Value;

use crate::execution::{
    EvalJudgeResult, EvalTraceEvent, EvalTrajectoryActionRunStatus, EvalTrajectoryEvent,
    EvalTrajectoryPayload, SerializedAction, SerializedOutcome,
};
use crate::spec::{EvalCriterionSpec, EvalExpectedActionSpec, EvalSpec};

const JUDGE_SYSTEM_PROMPT: &str = include_str!("prompts/judge_system.md");
const JUDGE_USER_PROMPT: &str = include_str!("prompts/judge_user.md");
const OPTIMIZATION_SUGGESTIONS_SYSTEM_PROMPT: &str =
    include_str!("prompts/optimization_suggestions_system.md");
const OPTIMIZATION_SUGGESTIONS_USER_PROMPT: &str =
    include_str!("prompts/optimization_suggestions_user.md");

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
        Self {
            spec,
            action_adapter,
        }
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
                        action, min_count, actual_count
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
                EvalCriterionSpec::LlmJudge { .. } => {}
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
                    failures.push(
                        "final_message_required failed: missing final agent message".to_owned(),
                    );
                } else {
                    eprintln!("[eval] criterion passed: final_message_required");
                }
            }
            EvalCriterionSpec::ForbidFinishReason {
                finish_reason: forbidden,
            } => {
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
            EvalCriterionSpec::ActionSequence { .. }
            | EvalCriterionSpec::ActionCount { .. }
            | EvalCriterionSpec::LlmJudge { .. } => {}
        }
    }
    failures
}

#[derive(Debug, Serialize)]
struct JudgePromptPayload<'a> {
    purpose: &'a str,
    user_task: &'a str,
    known_info: &'a str,
    finish_reason: String,
    final_agent_message: Option<&'a str>,
    trace: &'a [EvalTraceEvent],
    trajectory: &'a [EvalTrajectoryEvent],
}

#[derive(Debug, Deserialize)]
struct JudgeVerdict {
    passed: bool,
    #[serde(default)]
    score: Option<f64>,
    reason: String,
}

#[derive(Debug, Clone)]
pub struct EvalJudgeRequest {
    pub criterion_index: usize,
    pub model: Option<String>,
    pub rubric: String,
    pub system_prompt: String,
    pub user_message: String,
}

#[derive(Debug, Clone)]
pub struct EvalOptimizationSuggestionRequest {
    pub system_prompt: String,
    pub user_message: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct EvalOptimizationSuggestionsResponse {
    pub summary: String,
    pub suggestions: Vec<EvalOptimizationTargetSuggestion>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct EvalOptimizationTargetSuggestion {
    pub action: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<EvalOptimizationTargetSuggestionValue>,
    pub reason: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct EvalOptimizationTargetSuggestionValue {
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skill: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
}

#[async_trait]
pub trait EvalJudge: Send + Sync {
    async fn evaluate(&self, requests: Vec<EvalJudgeRequest>) -> Vec<EvalJudgeResult>;
}

pub fn build_llm_judge_requests(
    spec: &EvalSpec,
    finish_reason: &FinishReason,
    final_agent_message: Option<&str>,
    trace: &[EvalTraceEvent],
    trajectory: &[EvalTrajectoryEvent],
) -> Vec<EvalJudgeRequest> {
    let mut requests = Vec::new();
    for (index, criterion) in spec.evaluation.criteria.iter().enumerate() {
        let EvalCriterionSpec::LlmJudge { rubric, model } = criterion else {
            continue;
        };
        requests.push(EvalJudgeRequest {
            criterion_index: index,
            model: model.clone(),
            rubric: rubric.clone(),
            system_prompt: judge_system_prompt(),
            user_message: build_judge_user_message(
                spec,
                finish_reason,
                final_agent_message,
                trace,
                trajectory,
                rubric,
            ),
        });
    }
    requests
}

fn judge_system_prompt() -> String {
    JUDGE_SYSTEM_PROMPT.to_owned()
}

fn build_judge_user_message(
    spec: &EvalSpec,
    finish_reason: &FinishReason,
    final_agent_message: Option<&str>,
    trace: &[EvalTraceEvent],
    trajectory: &[EvalTrajectoryEvent],
    rubric: &str,
) -> String {
    let payload = JudgePromptPayload {
        purpose: &spec.scenario.purpose,
        user_task: spec.scenario.user.task(),
        known_info: spec.scenario.user.known_info(),
        finish_reason: format!("{finish_reason:?}"),
        final_agent_message,
        trace,
        trajectory,
    };
    let payload = serde_json::to_string_pretty(&payload)
        .unwrap_or_else(|_| "{\"error\":\"failed to serialize judge payload\"}".to_owned());
    JUDGE_USER_PROMPT
        .replace("{{rubric}}", rubric)
        .replace("{{payload_json}}", &payload)
}

pub fn parse_judge_verdict(content: &str) -> Result<EvalJudgeResultPayload, serde_json::Error> {
    let verdict: JudgeVerdict = serde_json::from_str(content).or_else(|_| {
        let trimmed = content.trim();
        let start = trimmed.find('{').unwrap_or(0);
        let end = trimmed
            .rfind('}')
            .map(|idx| idx + 1)
            .unwrap_or(trimmed.len());
        serde_json::from_str(&trimmed[start..end])
    })?;
    Ok(verdict.into())
}

#[derive(Debug, Serialize)]
struct OptimizationSuggestionPrompt<'a> {
    spec_id: &'a str,
    description: Option<&'a str>,
    purpose: &'a str,
    current_targets: &'a [crate::spec::EvalOptimizationTargetSpec],
    evaluation_passed: bool,
    evaluation_summary: Option<&'a String>,
    final_agent_message: Option<&'a String>,
    judge_results: &'a [EvalJudgeResult],
    trace: &'a [EvalTraceEvent],
}

pub fn build_optimization_suggestion_request(
    spec: &EvalSpec,
    result: &crate::execution::EvalRunResult,
) -> Result<EvalOptimizationSuggestionRequest, serde_json::Error> {
    let current_targets = spec
        .optimization
        .as_ref()
        .map(|optimization| optimization.targets.as_slice())
        .unwrap_or(&[]);
    let payload = OptimizationSuggestionPrompt {
        spec_id: &spec.id,
        description: spec.description.as_deref(),
        purpose: &spec.scenario.purpose,
        current_targets,
        evaluation_passed: result.evaluation.passed,
        evaluation_summary: result.evaluation.summary.as_ref(),
        final_agent_message: result.final_agent_message.as_ref(),
        judge_results: &result.judge_results,
        trace: &result.trace,
    };
    let payload_json = serde_json::to_string_pretty(&payload)?;
    Ok(EvalOptimizationSuggestionRequest {
        system_prompt: OPTIMIZATION_SUGGESTIONS_SYSTEM_PROMPT.to_owned(),
        user_message: OPTIMIZATION_SUGGESTIONS_USER_PROMPT
            .replace("{{payload_json}}", &payload_json),
    })
}

pub fn parse_optimization_suggestions(
    content: &str,
) -> Result<EvalOptimizationSuggestionsResponse, serde_json::Error> {
    serde_json::from_str(content).or_else(|_| {
        let trimmed = content.trim();
        let start = trimmed.find('{').unwrap_or(0);
        let end = trimmed.rfind('}').map(|idx| idx + 1).unwrap_or(trimmed.len());
        serde_json::from_str(&trimmed[start..end])
    })
}

#[derive(Debug, Clone)]
pub struct EvalJudgeResultPayload {
    pub passed: bool,
    pub score: Option<f64>,
    pub reason: String,
}

impl From<JudgeVerdict> for EvalJudgeResultPayload {
    fn from(value: JudgeVerdict) -> Self {
        Self {
            passed: value.passed,
            score: value.score,
            reason: value.reason,
        }
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

pub fn serialize_trajectory_events<T, O, A, U>(
    trajectory: &Trajectory<O, A, U>,
    action_adapter: &T,
) -> Vec<EvalTrajectoryEvent>
where
    T: EvalActionAdapter<A, U>,
{
    trajectory
        .events
        .iter()
        .enumerate()
        .map(|(sequence, event)| EvalTrajectoryEvent {
            sequence,
            payload: match event {
                TrajectoryEvent::SessionStarted { task } => EvalTrajectoryPayload::SessionStarted {
                    task_id: task.id.clone(),
                    instructions: task.instructions.clone(),
                },
                TrajectoryEvent::ObservationRecorded { .. } => {
                    EvalTrajectoryPayload::ObservationRecorded
                }
                TrajectoryEvent::ParticipantMessage {
                    participant,
                    content,
                } => EvalTrajectoryPayload::ParticipantMessage {
                    participant: serialize_participant_id(participant),
                    content: content.clone(),
                },
                TrajectoryEvent::ActionRequested {
                    participant,
                    action,
                    execution,
                } => EvalTrajectoryPayload::ActionRequested {
                    participant: serialize_participant_id(participant),
                    action: action_adapter.serialize_action(action),
                    execution: serialize_action_execution(*execution),
                },
                TrajectoryEvent::ActionRunStatus {
                    participant,
                    action_id,
                    run_id,
                    status,
                } => EvalTrajectoryPayload::ActionRunStatus {
                    participant: serialize_participant_id(participant),
                    action_id: action_id.to_string(),
                    run_id: run_id.to_string(),
                    status: serialize_action_run_status(status),
                },
                TrajectoryEvent::ActionScheduled {
                    participant,
                    action_id,
                    action,
                    execution,
                } => EvalTrajectoryPayload::ActionScheduled {
                    participant: serialize_participant_id(participant),
                    action_id: action_id.to_string(),
                    action: action_adapter.serialize_action(action),
                    execution: serialize_action_execution(*execution),
                },
                TrajectoryEvent::ActionCompleted {
                    participant,
                    action_id,
                    outcome,
                } => EvalTrajectoryPayload::ActionCompleted {
                    participant: serialize_participant_id(participant),
                    action_id: action_id.to_string(),
                    outcome: action_adapter.serialize_outcome(outcome),
                },
                TrajectoryEvent::ActionFailed {
                    participant,
                    action_id,
                    error,
                } => EvalTrajectoryPayload::ActionFailed {
                    participant: serialize_participant_id(participant),
                    action_id: action_id.to_string(),
                    user_message: error.user_message.clone(),
                    detail: error.detail.clone(),
                    origin: format!("{:?}", error.origin),
                },
                TrajectoryEvent::ParticipantYielded { participant } => {
                    EvalTrajectoryPayload::ParticipantYielded {
                        participant: serialize_participant_id(participant),
                    }
                }
                TrajectoryEvent::ParticipantLoopCompleted { participant } => {
                    EvalTrajectoryPayload::ParticipantLoopCompleted {
                        participant: serialize_participant_id(participant),
                    }
                }
                TrajectoryEvent::ParticipantFinished { participant } => {
                    EvalTrajectoryPayload::ParticipantFinished {
                        participant: serialize_participant_id(participant),
                    }
                }
                TrajectoryEvent::SessionFinished { reason } => {
                    EvalTrajectoryPayload::SessionFinished {
                        reason: format!("{:?}", reason),
                    }
                }
                TrajectoryEvent::EvaluationCompleted { result } => {
                    EvalTrajectoryPayload::EvaluationCompleted {
                        passed: result.passed,
                        score: result.score,
                        summary: result.summary.clone(),
                    }
                }
            },
        })
        .collect()
}

fn serialize_participant_id(participant: &ParticipantId) -> String {
    match participant {
        ParticipantId::Agent => "agent".to_owned(),
        ParticipantId::User => "user".to_owned(),
        ParticipantId::Custom(value) => value.clone(),
    }
}

fn serialize_action_execution(execution: pera_orchestrator::ActionExecution) -> String {
    match execution {
        pera_orchestrator::ActionExecution::Immediate => "immediate".to_owned(),
        pera_orchestrator::ActionExecution::DeferredBlocking => "deferred_blocking".to_owned(),
        pera_orchestrator::ActionExecution::DeferredNonBlocking => {
            "deferred_non_blocking".to_owned()
        }
    }
}

fn serialize_action_run_status(
    status: &pera_orchestrator::ActionRunStatus,
) -> EvalTrajectoryActionRunStatus {
    match status {
        pera_orchestrator::ActionRunStatus::RunSubmitted => {
            EvalTrajectoryActionRunStatus::RunSubmitted
        }
        pera_orchestrator::ActionRunStatus::RunStarted => EvalTrajectoryActionRunStatus::RunStarted,
        pera_orchestrator::ActionRunStatus::ActionEnqueued {
            engine_action_id,
            skill_name,
            action_name,
        } => EvalTrajectoryActionRunStatus::ActionEnqueued {
            engine_action_id: engine_action_id.to_string(),
            skill_name: skill_name.clone(),
            action_name: action_name.clone(),
        },
        pera_orchestrator::ActionRunStatus::ActionClaimed {
            engine_action_id,
            skill_name,
            action_name,
            worker_id,
        } => EvalTrajectoryActionRunStatus::ActionClaimed {
            engine_action_id: engine_action_id.to_string(),
            skill_name: skill_name.clone(),
            action_name: action_name.clone(),
            worker_id: worker_id.clone(),
        },
        pera_orchestrator::ActionRunStatus::ActionCompleted {
            engine_action_id,
            skill_name,
            action_name,
        } => EvalTrajectoryActionRunStatus::ActionCompleted {
            engine_action_id: engine_action_id.to_string(),
            skill_name: skill_name.clone(),
            action_name: action_name.clone(),
        },
        pera_orchestrator::ActionRunStatus::ActionFailed {
            engine_action_id,
            skill_name,
            action_name,
            message,
        } => EvalTrajectoryActionRunStatus::ActionFailed {
            engine_action_id: engine_action_id.to_string(),
            skill_name: skill_name.clone(),
            action_name: action_name.clone(),
            message: message.clone(),
        },
        pera_orchestrator::ActionRunStatus::RunResumed => EvalTrajectoryActionRunStatus::RunResumed,
    }
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
                .all(|(expected_action, actual_action)| {
                    action_matches(expected_action, actual_action)
                })
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
    expected.action == actual.name
        && arguments_match(expected.arguments.as_ref(), actual.arguments.as_ref())
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
        actual_map
            .get(key)
            .map(|actual_value| actual_value == expected_value)
            .unwrap_or(false)
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
        FinishReason::BlockedActionWaitExceeded => "blocked_action_wait_exceeded",
        FinishReason::MessageLimitExceeded => "message_limit_exceeded",
        FinishReason::TimeLimitExceeded => "time_limit_exceeded",
        FinishReason::ParticipantError { .. } => "participant_error",
        FinishReason::EnvironmentError(_) => "environment_error",
        FinishReason::EnvironmentTerminated(_) => "environment_terminated",
        FinishReason::Deadlocked => "deadlocked",
    }
}
