use std::error::Error;
use std::fmt::{Display, Formatter};

use pera_core::{
    ActionId, ActionRecord, ActionRequest, ActionResult, ActionStatus, CodeArtifactId,
    ExecutionSession, ExecutionStatus, Interpreter, InterpreterStep, RunId, StartExecutionRequest,
};

#[derive(Debug)]
pub enum RunExecutorError {
    Interpreter(pera_core::InterpreterError),
    InvalidState(&'static str),
}

impl Display for RunExecutorError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Interpreter(error) => write!(f, "interpreter error: {error}"),
            Self::InvalidState(message) => f.write_str(message),
        }
    }
}

impl Error for RunExecutorError {}

impl From<pera_core::InterpreterError> for RunExecutorError {
    fn from(value: pera_core::InterpreterError) -> Self {
        Self::Interpreter(value)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunTransition {
    pub trigger: RunTransitionTrigger,
    pub session: ExecutionSession,
    pub action_records: Vec<ActionRecord>,
    pub action_to_enqueue: Option<ActionRequest>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunTransitionTrigger {
    Started,
    Resumed { completed_action_id: ActionId },
    Failed,
}

#[derive(Debug)]
pub struct RunExecutor<I> {
    interpreter: I,
}

impl<I> RunExecutor<I>
where
    I: Interpreter,
{
    pub fn new(interpreter: I) -> Self {
        Self { interpreter }
    }

    pub fn start_run(
        &self,
        mut request: StartExecutionRequest,
        run_id: RunId,
        code_id: CodeArtifactId,
        next_action_id: impl FnOnce() -> pera_core::ActionId,
    ) -> Result<RunTransition, RunExecutorError> {
        request.code.id = code_id;

        // TODO we could cache the compiled program
        let program = self.interpreter.compile(&request.code)?;
        let session = ExecutionSession {
            id: run_id,
            code: request.code,
            program: program.clone(),
            status: ExecutionStatus::Running,
            snapshot: None,
        };

        let step = self.interpreter.start(&program, &request.inputs)?;
        self.apply_step(session, step, next_action_id, RunTransitionTrigger::Started)
    }

    pub fn complete_action(
        &self,
        session: ExecutionSession,
        action_request: ActionRequest,
        result: ActionResult,
        next_action_id: impl FnOnce() -> pera_core::ActionId,
    ) -> Result<RunTransition, RunExecutorError> {
        let snapshot = session
            .snapshot
            .clone()
            .ok_or(RunExecutorError::InvalidState(
                "cannot resume a run without a snapshot",
            ))?;

        let completed_action = ActionRecord {
            request: action_request,
            status: ActionStatus::Completed(result.clone()),
        };

        let step = self.interpreter.resume(&snapshot, &result.value)?;
        let mut transition = self.apply_step(
            session,
            step,
            next_action_id,
            RunTransitionTrigger::Resumed {
                completed_action_id: result.action_id,
            },
        )?;
        transition.action_records.insert(0, completed_action);
        Ok(transition)
    }

    pub fn fail_run(
        &self,
        mut session: ExecutionSession,
        message: impl Into<String>,
    ) -> RunTransition {
        let message = message.into();
        session.snapshot = None;
        session.status = ExecutionStatus::Failed(message.clone());

        RunTransition {
            trigger: RunTransitionTrigger::Failed,
            session,
            action_records: Vec::new(),
            action_to_enqueue: None,
        }
    }

    fn apply_step(
        &self,
        mut session: ExecutionSession,
        step: InterpreterStep,
        next_action_id: impl FnOnce() -> pera_core::ActionId,
        trigger: RunTransitionTrigger,
    ) -> Result<RunTransition, RunExecutorError> {
        match step {
            InterpreterStep::Suspended(suspension) => {
                let action_id = next_action_id();
                let action_request = ActionRequest {
                    id: action_id,
                    run_id: session.id,
                    action_name: suspension.call.action_name,
                    arguments: suspension.call.arguments,
                };
                let action_record = ActionRecord {
                    request: action_request.clone(),
                    status: ActionStatus::Pending,
                };

                session.snapshot = Some(suspension.snapshot);
                session.status = ExecutionStatus::WaitingForAction(action_id);

                Ok(RunTransition {
                    trigger,
                    session,
                    action_records: vec![action_record],
                    action_to_enqueue: Some(action_request),
                })
            }
            InterpreterStep::Completed(output) => {
                session.snapshot = None;
                session.status = ExecutionStatus::Completed(output);

                Ok(RunTransition {
                    trigger,
                    session,
                    action_records: Vec::new(),
                    action_to_enqueue: None,
                })
            }
        }
    }
}
