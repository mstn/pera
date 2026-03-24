use std::fs;
use std::path::{Path, PathBuf};

use clap::Args;
use pera_core::{
    CodeArtifact, CodeArtifactId, CodeLanguage, ExecutionEvent, ExecutionStatus, InputValues,
    RunId, ScriptName, StartExecutionRequest,
};
use pera_runtime::{
    interpreter::MontyInterpreter, EventHub, ExecutionEngine, FileSystemEventLog,
    FileSystemRunStore, FileSystemSkillRuntimeLoader, RunExecutor,
    TeeEventPublisher,
    WasmtimeComponentActionExecutor,
};

use crate::error::CliError;

#[derive(Debug, Args)]
pub struct RunCommand {
    pub path: Option<PathBuf>,
    #[arg(long)]
    pub root: PathBuf,
    #[arg(long)]
    pub run_id: Option<String>,
}

impl RunCommand {
    pub async fn execute(&self) -> Result<(), CliError> {
        let target = self.execution_target()?;

        let interpreter = MontyInterpreter::new();
        let store = FileSystemRunStore::new(&self.root).map_err(CliError::Store)?;
        let skill_runtime = FileSystemSkillRuntimeLoader::new(&self.root)
            .load()
            .map_err(CliError::Store)?;
        let event_hub = EventHub::new();
        let event_log = FileSystemEventLog::new(&self.root).map_err(CliError::Store)?;
        let recovery_events = event_log.read_events().map_err(CliError::Store)?;
        let publisher = TeeEventPublisher::new(event_log, event_hub.publisher());
        skill_runtime.warm_components().await.map_err(CliError::Store)?;

        let run_executor =
            RunExecutor::with_skill_catalog(interpreter, skill_runtime.catalog().clone());
        let action_executor = WasmtimeComponentActionExecutor::new(skill_runtime)
            .map_err(|error| CliError::UnexpectedStateOwned(error.to_string()))?;
        let engine = ExecutionEngine::new(run_executor, store, publisher, action_executor, event_hub);
        let mut subscription = engine.subscribe();
        engine
            .recover_from_events(recovery_events)
            .await
            .map_err(CliError::Engine)?;
        let attach_mode = matches!(&target, ExecutionTarget::Attach(_));
        let run_id = match target {
            ExecutionTarget::Submit(request) => engine.submit(request).await.map_err(CliError::Engine)?,
            ExecutionTarget::Attach(run_id) => {
                if engine.run_status(run_id).is_none() {
                    return Err(CliError::UnknownRun(run_id));
                }
                run_id
            }
        };

        if attach_mode {
            if let Some(result) =
                drain_buffered_events(&mut subscription, run_id).map_err(CliError::Store)?
            {
                return result;
            }

            if let Some(status) = engine.run_status(run_id) {
                if let Some(event) = terminal_event(run_id, status) {
                    return handle_terminal_event(&event);
                }
            }
        }

        loop {
            let event = subscription.recv().await.map_err(CliError::Store)?;
            if event.run_id() != run_id {
                continue;
            }

            print_event(&event)?;

            if let Some(result) = terminal_result(&event) {
                return result;
            }
        }
    }

    fn execution_target(&self) -> Result<ExecutionTarget, CliError> {
        match (&self.path, &self.run_id) {
            (Some(_), Some(_)) => Err(CliError::InvalidArguments(
                "provide either <path> or --run-id, not both",
            )),
            (None, None) => Err(CliError::InvalidArguments(
                "provide either <path> or --run-id",
            )),
            (Some(path), None) => {
                let source = fs::read_to_string(path).map_err(|source| CliError::ReadFile {
                    path: path.clone(),
                    source,
                })?;
                Ok(ExecutionTarget::Submit(StartExecutionRequest {
                    code: CodeArtifact {
                        id: CodeArtifactId::generate(),
                        language: CodeLanguage::Python,
                        script_name: ScriptName::new(display_name(path)),
                        source,
                        inputs: Vec::new(),
                    },
                    inputs: InputValues::new(),
                }))
            }
            (None, Some(run_id)) => {
                let run_id = RunId::parse_str(run_id)
                    .map_err(|_| CliError::InvalidArguments("invalid --run-id value"))?;
                Ok(ExecutionTarget::Attach(run_id))
            }
        }
    }
}

enum ExecutionTarget {
    Submit(StartExecutionRequest),
    Attach(RunId),
}

fn drain_buffered_events(
    subscription: &mut pera_runtime::EventSubscription,
    run_id: RunId,
) -> Result<Option<Result<(), CliError>>, pera_core::StoreError> {
    loop {
        let Some(event) = subscription.try_recv()? else {
            return Ok(None);
        };

        if event.run_id() != run_id {
            continue;
        }

        print_event(&event)
            .map_err(|error| pera_core::StoreError::new(error.to_string()))?;

        if let Some(result) = terminal_result(&event) {
            return Ok(Some(result));
        }
    }
}

fn terminal_event(run_id: RunId, status: ExecutionStatus) -> Option<ExecutionEvent> {
    match status {
        ExecutionStatus::Completed(output) => Some(ExecutionEvent::RunCompleted {
            run_id,
            value: output.value,
        }),
        ExecutionStatus::Failed(message) => Some(ExecutionEvent::RunFailed { run_id, message }),
        ExecutionStatus::Running | ExecutionStatus::WaitingForAction(_) => None,
    }
}

fn handle_terminal_event(event: &ExecutionEvent) -> Result<(), CliError> {
    print_event(event)?;
    terminal_result(event).unwrap_or(Ok(()))
}

fn terminal_result(event: &ExecutionEvent) -> Option<Result<(), CliError>> {
    match event {
        ExecutionEvent::RunCompleted { .. } => Some(Ok(())),
        ExecutionEvent::RunFailed { message, .. } => {
            Some(Err(CliError::UnexpectedStateOwned(message.clone())))
        }
        ExecutionEvent::RunSubmitted { .. }
        | ExecutionEvent::RunStarted { .. }
        | ExecutionEvent::ActionEnqueued { .. }
        | ExecutionEvent::ActionClaimed { .. }
        | ExecutionEvent::ActionCompleted { .. }
        | ExecutionEvent::ActionFailed { .. }
        | ExecutionEvent::RunResumed { .. } => None,
    }
}

fn print_event(event: &ExecutionEvent) -> Result<(), CliError> {
    let line = serde_json::to_string(event)
        .map_err(|error| CliError::UnexpectedStateOwned(format!("failed to serialize event: {error}")))?;
    println!("{line}");
    Ok(())
}

fn display_name(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .map(str::to_owned)
        .unwrap_or_else(|| path.display().to_string())
}
