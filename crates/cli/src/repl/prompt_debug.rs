use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::Mutex;

use chrono::Local;
use pera_agents::{LlmRequest, PromptDebugMetadata, PromptDebugSink};
use pera_core::{RunId, WorkItemId};
use pera_runtime::FileSystemLayout;

use crate::error::CliError;

pub struct FilePromptDebugSink {
    layout: FileSystemLayout,
    model: Option<String>,
    run_directories: Mutex<BTreeMap<RunId, PathBuf>>,
    loop_directories: Mutex<BTreeMap<(RunId, WorkItemId), PathBuf>>,
}

impl FilePromptDebugSink {
    pub fn new(root: PathBuf, model: Option<String>) -> Self {
        let layout = FileSystemLayout::new(root)
            .expect("prompt debug layout initialization must succeed");
        Self {
            layout,
            model,
            run_directories: Mutex::new(BTreeMap::new()),
            loop_directories: Mutex::new(BTreeMap::new()),
        }
    }

    fn resolve_run_directory(&self, run_id: RunId) -> Result<PathBuf, CliError> {
        let mut run_directories = self
            .run_directories
            .lock()
            .map_err(|_| CliError::UnexpectedStateOwned("prompt debug lock poisoned".to_owned()))?;
        if let Some(path) = run_directories.get(&run_id) {
            return Ok(path.clone());
        }

        let path = self
            .layout
            .orchestration_runs_dir()
            .join(format!("{}-{}", timestamp_prefix(), run_id.as_hyphenated()));
        std::fs::create_dir_all(&path).map_err(|source| CliError::CreateDir {
            path: path.clone(),
            source,
        })?;
        run_directories.insert(run_id, path.clone());
        Ok(path)
    }

    fn resolve_loop_directory(
        &self,
        metadata: &PromptDebugMetadata,
    ) -> Result<PathBuf, CliError> {
        let key = (metadata.run_id, metadata.agent_loop_id);
        let mut loop_directories = self
            .loop_directories
            .lock()
            .map_err(|_| CliError::UnexpectedStateOwned("prompt debug lock poisoned".to_owned()))?;
        if let Some(path) = loop_directories.get(&key) {
            return Ok(path.clone());
        }

        let run_directory = self.resolve_run_directory(metadata.run_id)?;
        let path = run_directory.join(format!(
            "{}-{}",
            timestamp_prefix(),
            metadata.agent_loop_id.as_hyphenated()
        ));
        std::fs::create_dir_all(&path).map_err(|source| CliError::CreateDir {
            path: path.clone(),
            source,
        })?;
        loop_directories.insert(key, path.clone());
        Ok(path)
    }
}

impl PromptDebugSink for FilePromptDebugSink {
    fn record_prompt(
        &self,
        metadata: &PromptDebugMetadata,
        request: &LlmRequest,
    ) -> Result<(), pera_orchestrator::ParticipantError> {
        let loop_directory = self
            .resolve_loop_directory(metadata)
            .map_err(|error| pera_orchestrator::ParticipantError::new(error.to_string()))?;
        let iteration_id = format!("{:04}", metadata.agent_loop_iteration);
        let prompt_path = loop_directory.join(format!("{iteration_id}.prompt.md"));
        let tools_path = loop_directory.join(format!("{iteration_id}.tools.json"));

        std::fs::write(
            &prompt_path,
            render_prompt_markdown(self.model.as_deref(), metadata, request),
        )
        .map_err(|source| {
            pera_orchestrator::ParticipantError::new(
                CliError::WriteFile {
                    path: prompt_path,
                    source,
                }
                .to_string(),
            )
        })?;

        let tools_json = serde_json::to_vec_pretty(&request.tools).map_err(|error| {
            pera_orchestrator::ParticipantError::new(
                CliError::UnexpectedStateOwned(error.to_string()).to_string(),
            )
        })?;
        std::fs::write(&tools_path, tools_json).map_err(|source| {
            pera_orchestrator::ParticipantError::new(
                CliError::WriteFile {
                    path: tools_path,
                    source,
                }
                .to_string(),
            )
        })?;

        Ok(())
    }
}

fn render_prompt_markdown(
    model: Option<&str>,
    metadata: &PromptDebugMetadata,
    request: &LlmRequest,
) -> String {
    let mut output = String::new();
    let _ = writeln!(&mut output, "---");
    let _ = writeln!(&mut output, "generated_at: {}", Local::now().to_rfc3339());
    let _ = writeln!(&mut output, "run_id: {}", metadata.run_id);
    let _ = writeln!(&mut output, "agent_loop_id: {}", metadata.agent_loop_id);
    let _ = writeln!(
        &mut output,
        "agent_loop_iteration: {}",
        metadata.agent_loop_iteration
    );
    let _ = writeln!(&mut output, "participant: {:?}", metadata.participant);
    let _ = writeln!(&mut output, "task_id: {}", metadata.task_id);
    if let Some(model) = model {
        let _ = writeln!(&mut output, "model: {model}");
    }
    let _ = writeln!(&mut output, "---");
    let _ = writeln!(&mut output);

    let _ = writeln!(&mut output, "<system>");
    let _ = writeln!(&mut output, "{}", request.system_prompt);
    let _ = writeln!(&mut output, "</system>");
    let _ = writeln!(&mut output);

    for message in &request.messages {
        let tag = match message.role.as_str() {
            "system" => "system",
            "assistant" => "assistant",
            "developer" => "developer",
            "user" => "user",
            _ => "message",
        };
        let _ = writeln!(&mut output, "<{tag}>");
        let _ = writeln!(&mut output, "{}", message.content);
        let _ = writeln!(&mut output, "</{tag}>");
        let _ = writeln!(&mut output);
    }

    output
}

fn timestamp_prefix() -> String {
    Local::now().format("%Y%m%d-%H%M%S").to_string()
}
