use std::path::PathBuf;

use pera_orchestrator::{
    Environment, EvaluatorError, NoopParticipantOutput, Orchestrator, Participant, ParticipantId,
    RunLimits, RunRequest, TerminationCondition, TrajectoryEvent,
};

use crate::error::EvalError;
use crate::evaluator::{EvalActionAdapter, SpecEvaluator, trajectory_trace_events};
use crate::execution::{EvalPreparation, EvalRunResult, EvalRunWorkspace};
use crate::overrides::OverrideSet;
use crate::runner::EvalRunner;
use crate::spec::{LoadedEvalSpec, load_eval_spec};

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
        let mut loaded_spec = load_eval_spec(&request.spec_path, &request.overrides)?;
        if let Some(path) = request.output_folder {
            loaded_spec.override_output_folder(path)?;
        }

        Ok(EvalSession {
            mode,
            loaded_spec,
            preparation: None,
        })
    }

    pub async fn prepare(&self, session: &mut EvalSession) -> Result<(), EvalError> {
        let preparation = EvalRunner::new().prepare(&session.loaded_spec.spec).await?;
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
    ) -> Result<EvalRunResult, EvalError>
    where
        E: Environment<Observation = O, Action = A, Outcome = U>,
        O: Clone + Send + Sync + 'static,
        A: Clone + Send + Sync + 'static,
        U: Clone + Send + Sync + 'static,
        T: EvalActionAdapter<A, U>,
    {
        let evaluator = SpecEvaluator::new(session.loaded_spec.spec.clone(), action_adapter.clone());
        let mut orchestrator =
            Orchestrator::with_participants_and_evaluator(participants, environment, evaluator);
        let result = orchestrator
            .run_with_output(
                RunRequest {
                    task: pera_orchestrator::TaskSpec {
                        id: session.loaded_spec.spec.id.clone(),
                        instructions: session.loaded_spec.spec.scenario.purpose.clone(),
                    },
                    limits: RunLimits::default(),
                    termination_condition: TerminationCondition::AnyOfParticipantsFinished(vec![
                        ParticipantId::Agent,
                    ]),
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

        let evaluation = result.evaluation.unwrap_or_else(|| pera_orchestrator::EvalResult {
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

        Ok(EvalRunResult {
            passed: evaluation.passed,
            finish_reason: result.finish_reason,
            evaluation,
            final_agent_message,
            trace,
            workspace: EvalRunWorkspace {
                root: run_dir.join("project"),
                run_dir,
            },
        })
    }
}
