use std::path::PathBuf;

use pera_orchestrator::{
    Environment, EvaluatorError, NoopParticipantOutput, Orchestrator, Participant, ParticipantId,
    RunLimits, RunRequest, TerminationCondition, TrajectoryEvent,
};

use crate::error::EvalError;
use crate::evaluator::{
    EvalActionAdapter, EvalJudge, SpecEvaluator, build_llm_judge_requests, evaluate_run_criteria,
    serialize_trajectory_events, trajectory_trace_events,
};
use crate::execution::{EvalPreparation, EvalRunResult, EvalRunWorkspace};
use crate::overrides::OverrideSet;
use crate::runner::EvalRunner;
use crate::spec::{EvalUserSpec, LoadedEvalSpec, load_eval_spec};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvalMode {
    Run,
    Optimize,
}

impl EvalMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Run => "run",
            Self::Optimize => "optimize",
        }
    }
}

#[derive(Debug, Clone)]
pub struct EvalRequest {
    pub spec_path: PathBuf,
    pub output_folder: Option<PathBuf>,
    pub overrides: OverrideSet,
    pub user: Option<String>,
}

#[derive(Debug, Clone)]
pub struct EvalSession {
    pub mode: EvalMode,
    pub loaded_spec: LoadedEvalSpec,
    pub preparation: Option<EvalPreparation>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct EvalEngine;

impl EvalEngine {
    pub fn resolve(&self, mode: EvalMode, request: EvalRequest) -> Result<EvalSession, EvalError> {
        eprintln!(
            "[eval] resolving spec mode={} path={}",
            mode.as_str(),
            request.spec_path.display()
        );
        let mut loaded_spec =
            load_eval_spec(&request.spec_path, &request.overrides, request.user.as_deref())?;
        if let Some(path) = request.output_folder {
            eprintln!("[eval] overriding output folder to {}", path.display());
            loaded_spec.override_output_folder(path)?;
        }
        eprintln!(
            "[eval] resolved spec id={} output_folder={}",
            loaded_spec.spec.id,
            loaded_spec.spec.runtime.output_folder.display()
        );

        Ok(EvalSession {
            mode,
            loaded_spec,
            preparation: None,
        })
    }

    pub async fn prepare(&self, session: &mut EvalSession) -> Result<(), EvalError> {
        eprintln!(
            "[eval] preparing runtime for spec id={}",
            session.loaded_spec.spec.id
        );
        let preparation = EvalRunner::new().prepare(&session.loaded_spec.spec).await?;
        eprintln!(
            "[eval] preparation complete project_root={} skills={}",
            preparation.project.root.display(),
            preparation.skills.len()
        );
        session.preparation = Some(preparation);
        Ok(())
    }

    pub async fn run_with<E, O, A, U, T>(
        &self,
        session: &EvalSession,
        run_dir: PathBuf,
        environment: E,
        participants: Vec<Box<dyn Participant<Observation = O, Action = A, Outcome = U>>>,
        action_adapter: T,
        judge: Option<&dyn EvalJudge>,
    ) -> Result<EvalRunResult, EvalError>
    where
        E: Environment<Observation = O, Action = A, Outcome = U>,
        O: Clone + Send + Sync + 'static,
        A: Clone + Send + Sync + 'static,
        U: Clone + Send + Sync + 'static,
        T: EvalActionAdapter<A, U>,
    {
        eprintln!(
            "[eval] starting orchestrator run spec_id={} run_dir={}",
            session.loaded_spec.spec.id,
            run_dir.display()
        );
        let evaluator = SpecEvaluator::new(session.loaded_spec.spec.clone(), action_adapter.clone());
        let mut orchestrator =
            Orchestrator::with_participants_and_evaluator(participants, environment, evaluator);
        let termination_condition =
            termination_condition_for_user(&session.loaded_spec.spec.scenario.user);
        let result = orchestrator
            .run_with_output(
                RunRequest {
                    task: pera_orchestrator::TaskSpec {
                        id: session.loaded_spec.spec.id.clone(),
                        instructions: session.loaded_spec.spec.scenario.purpose.clone(),
                    },
                    limits: RunLimits {
                        max_steps: 256,
                        max_failed_actions: Some(5),
                        max_consecutive_failed_actions: Some(3),
                        max_blocked_action_wait: Some(std::time::Duration::from_secs(30)),
                        max_duration: Some(std::time::Duration::from_secs(90)),
                        ..RunLimits::default()
                    },
                    termination_condition,
                    initial_messages: vec![pera_orchestrator::InitialInboxMessage {
                        to: ParticipantId::User,
                        from: ParticipantId::Custom("system".to_owned()),
                        content: "start".to_owned(),
                    }],
                },
                &mut NoopParticipantOutput,
            )
            .await
            .map_err(|error: EvaluatorError| EvalError::Internal(error.to_string()))?;
        eprintln!(
            "[eval] orchestrator finished finish_reason={:?} trajectory_events={}",
            result.finish_reason,
            result.trajectory.events.len()
        );

        let mut evaluation = result.evaluation.unwrap_or_else(|| pera_orchestrator::EvalResult {
            passed: false,
            score: Some(0.0),
            summary: Some("missing evaluation result".to_owned()),
        });
        let final_agent_message = result
            .trajectory
            .events
            .iter()
            .rev()
            .find_map(|event| match event {
                TrajectoryEvent::ParticipantMessage {
                    participant: ParticipantId::Agent,
                    content,
                } => Some(content.clone()),
                _ => None,
            });
        let trace = trajectory_trace_events(&result.trajectory, &action_adapter);
        let serialized_trajectory = serialize_trajectory_events(&result.trajectory, &action_adapter);
        let run_level_failures = evaluate_run_criteria(
            &session.loaded_spec.spec,
            &result.finish_reason,
            final_agent_message.as_deref(),
        );
        let judge_requests = build_llm_judge_requests(
            &session.loaded_spec.spec,
            &result.finish_reason,
            final_agent_message.as_deref(),
            &trace,
            &serialized_trajectory,
        );
        let judge_results = if let Some(judge) = judge {
            judge.evaluate(judge_requests).await
        } else {
            judge_requests
                .into_iter()
                .map(|request| crate::execution::EvalJudgeResult {
                    criterion_index: request.criterion_index,
                    model: request.model,
                    passed: false,
                    score: Some(0.0),
                    summary: "llm_judge could not run because no judge executor was configured"
                        .to_owned(),
                    rubric: request.rubric,
                    response: String::new(),
                })
                .collect()
        };
        if !run_level_failures.is_empty() {
            eprintln!(
                "[eval] run-level criteria failed count={}",
                run_level_failures.len()
            );
            let mut failure_messages = Vec::new();
            if let Some(summary) = evaluation.summary.take() {
                if !summary.trim().is_empty() {
                    failure_messages.push(summary);
                }
            }
            failure_messages.extend(run_level_failures);
            evaluation.passed = false;
            evaluation.score = Some(0.0);
            evaluation.summary = Some(failure_messages.join("\n"));
        }
        let judge_failures = judge_results
            .iter()
            .filter(|result| !result.passed)
            .map(|result| format!("llm_judge failed: {}", result.summary))
            .collect::<Vec<_>>();
        if !judge_failures.is_empty() {
            let mut failure_messages = Vec::new();
            if let Some(summary) = evaluation.summary.take() {
                if !summary.trim().is_empty() {
                    failure_messages.push(summary);
                }
            }
            failure_messages.extend(judge_failures);
            evaluation.passed = false;
            evaluation.score = Some(0.0);
            evaluation.summary = Some(failure_messages.join("\n"));
        }
        eprintln!(
            "[eval] evaluation passed={} score={:?} trace_events={}",
            evaluation.passed,
            evaluation.score,
            trace.len()
        );

        Ok(EvalRunResult {
            passed: evaluation.passed,
            finish_reason: result.finish_reason,
            evaluation,
            final_agent_message,
            judge_results,
            trace,
            trajectory: serialized_trajectory,
            workspace: EvalRunWorkspace {
                root: run_dir.join("project"),
                run_dir,
            },
        })
    }
}

fn termination_condition_for_user(user: &EvalUserSpec) -> TerminationCondition {
    if user.is_multi_turn() {
        TerminationCondition::AnyParticipantFinished
    } else {
        TerminationCondition::AnyOfParticipantsCompletedLoop(vec![ParticipantId::Agent])
    }
}

#[cfg(test)]
mod tests {
    use super::termination_condition_for_user;
    use crate::spec::EvalUserSpec;
    use pera_orchestrator::{ParticipantId, TerminationCondition};

    #[test]
    fn scripted_users_use_single_answer_termination() {
        let condition = termination_condition_for_user(&EvalUserSpec::Scripted {
            task: "task".to_owned(),
            known_info: "known".to_owned(),
            initial_message: "hello".to_owned(),
        });

        assert_eq!(
            condition,
            TerminationCondition::AnyOfParticipantsCompletedLoop(vec![ParticipantId::Agent])
        );
    }

    #[test]
    fn simulated_users_use_multi_turn_termination() {
        let condition = termination_condition_for_user(&EvalUserSpec::Simulated {
            task: "task".to_owned(),
            reason: "reason".to_owned(),
            known_info: "known".to_owned(),
            unknown_info: "unknown".to_owned(),
        });

        assert_eq!(condition, TerminationCondition::AnyParticipantFinished);
    }
}
