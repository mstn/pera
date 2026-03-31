use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use pera_core::{
    ActionId, ActionRecord, CodeArtifact, CodeLanguage, EventPublisher, ExecutionEvent,
    ExecutionSession, RunId, RunStore, StoreError,
};

#[derive(Debug, Clone)]
pub struct FileSystemLayout {
    root: PathBuf,
}

impl FileSystemLayout {
    pub fn new(root: impl Into<PathBuf>) -> Result<Self, StoreError> {
        let layout = Self { root: root.into() };
        layout.ensure_directories()?;
        Ok(layout)
    }

    fn ensure_directories(&self) -> Result<(), StoreError> {
        create_dir_all(self.runs_dir())?;
        create_dir_all(self.orchestration_runs_dir())?;
        create_dir_all(self.system_actions_dir())?;
        create_dir_all(self.system_dir())?;
        Ok(())
    }

    pub fn runs_dir(&self) -> PathBuf {
        self.execution_dir().join("runs")
    }

    pub fn execution_dir(&self) -> PathBuf {
        self.root.join("execution")
    }

    pub fn orchestration_dir(&self) -> PathBuf {
        self.root.join("orchestration")
    }

    pub fn orchestration_runs_dir(&self) -> PathBuf {
        self.orchestration_dir().join("runs")
    }

    fn system_dir(&self) -> PathBuf {
        self.root.join("system")
    }

    fn system_actions_dir(&self) -> PathBuf {
        self.system_dir().join("actions")
    }

    fn run_dir(&self, run_id: RunId) -> PathBuf {
        self.runs_dir().join(run_id.as_hyphenated())
    }

    fn run_record_path(&self, run_id: RunId) -> PathBuf {
        self.run_dir(run_id).join("run.json")
    }

    fn system_action_path(&self, action_id: ActionId) -> PathBuf {
        self.system_actions_dir()
            .join(format!("{}.json", action_id.as_hyphenated()))
    }

    fn run_actions_dir(&self, run_id: RunId) -> PathBuf {
        self.run_dir(run_id).join("actions")
    }

    fn run_artifacts_dir(&self, run_id: RunId) -> PathBuf {
        self.run_dir(run_id).join("artifacts")
    }

    fn run_code_artifacts_dir(&self, run_id: RunId) -> PathBuf {
        self.run_artifacts_dir(run_id).join("code")
    }

    fn run_code_artifact_path(&self, run_id: RunId, artifact: &CodeArtifact) -> PathBuf {
        self.run_code_artifacts_dir(run_id)
            .join(format!("{}.{}", artifact.id.as_hyphenated(), code_extension(artifact.language)))
    }

    fn run_code_artifact_metadata_path(
        &self,
        run_id: RunId,
        artifact: &CodeArtifact,
    ) -> PathBuf {
        self.run_code_artifacts_dir(run_id)
            .join(format!("{}.meta.json", artifact.id.as_hyphenated()))
    }

    fn run_action_path(&self, run_id: RunId, action_id: ActionId) -> PathBuf {
        self.run_actions_dir(run_id)
            .join(format!("{}.json", action_id.as_hyphenated()))
    }

    fn system_events_path(&self) -> PathBuf {
        self.system_dir().join("events.jsonl")
    }

    fn run_events_path(&self, run_id: RunId) -> PathBuf {
        self.run_dir(run_id).join("events.jsonl")
    }
}

#[derive(Debug, Clone)]
pub struct FileSystemRunStore {
    layout: FileSystemLayout,
}

impl FileSystemRunStore {
    pub fn new(root: impl Into<PathBuf>) -> Result<Self, StoreError> {
        Ok(Self {
            layout: FileSystemLayout::new(root)?,
        })
    }
}

impl RunStore for FileSystemRunStore {
    fn create_run(&mut self, session: ExecutionSession) -> Result<(), StoreError> {
        let run_dir = self.layout.run_dir(session.id);
        create_dir_all(&run_dir)?;
        create_dir_all(self.layout.run_actions_dir(session.id))?;
        write_json(self.layout.run_record_path(session.id), &session)
    }

    fn save_run(&mut self, session: ExecutionSession) -> Result<(), StoreError> {
        let run_dir = self.layout.run_dir(session.id);
        create_dir_all(&run_dir)?;
        create_dir_all(self.layout.run_actions_dir(session.id))?;
        write_json(self.layout.run_record_path(session.id), &session)
    }

    fn load_run(&self, run_id: RunId) -> Result<ExecutionSession, StoreError> {
        read_json(self.layout.run_record_path(run_id))
    }

    fn list_runs(&self) -> Result<Vec<RunId>, StoreError> {
        let mut run_ids = Vec::new();
        for entry in fs::read_dir(self.layout.runs_dir()).map_err(io_error)? {
            let entry = entry.map_err(io_error)?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let run_id = RunId::parse_str(&name)
                .map_err(|_| StoreError::new(format!("invalid id entry '{name}'")))?;
            if self.layout.run_record_path(run_id).exists() {
                run_ids.push(run_id);
            }
        }
        Ok(run_ids)
    }

    fn save_code_artifact(
        &mut self,
        run_id: RunId,
        artifact: &CodeArtifact,
    ) -> Result<(), StoreError> {
        let source_path = self.layout.run_code_artifact_path(run_id, artifact);
        if let Some(parent) = source_path.parent() {
            create_dir_all(parent)?;
        }
        fs::write(&source_path, artifact.source.as_bytes()).map_err(io_error)?;
        write_json(
            self.layout.run_code_artifact_metadata_path(run_id, artifact),
            &serde_json::json!({
                "artifact_id": artifact.id,
                "language": artifact.language,
                "script_name": artifact.script_name.as_str(),
                "path": source_path.file_name().and_then(|name| name.to_str()).unwrap_or_default(),
            }),
        )
    }

    fn save_action(&mut self, action: ActionRecord) -> Result<(), StoreError> {
        write_json(self.layout.system_action_path(action.request.id), &action)?;
        write_json(
            self.layout
                .run_action_path(action.request.run_id, action.request.id),
            &action,
        )
    }

    fn load_action(&self, action_id: ActionId) -> Result<ActionRecord, StoreError> {
        read_json(self.layout.system_action_path(action_id))
    }

    fn list_actions(&self) -> Result<Vec<ActionId>, StoreError> {
        list_id_entries(self.layout.system_actions_dir(), ActionId::parse_str)
    }
}

#[derive(Debug, Clone)]
pub struct FileSystemEventLog {
    layout: FileSystemLayout,
}

impl FileSystemEventLog {
    pub fn new(root: impl Into<PathBuf>) -> Result<Self, StoreError> {
        Ok(Self {
            layout: FileSystemLayout::new(root)?,
        })
    }

    pub fn read_events(&self) -> Result<Vec<ExecutionEvent>, StoreError> {
        let path = self.layout.system_events_path();
        if !path.exists() {
            return Ok(Vec::new());
        }

        let content = fs::read_to_string(path).map_err(io_error)?;
        content
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| serde_json::from_str(line).map_err(json_error))
            .collect()
    }
}

impl EventPublisher for FileSystemEventLog {
    fn publish(&mut self, event: ExecutionEvent) -> Result<(), StoreError> {
        let line = serde_json::to_string(&event).map_err(json_error)?;
        append_line(self.layout.system_events_path(), &line)?;
        append_line(self.layout.run_events_path(event.run_id()), &line)
    }
}

fn list_id_entries<T, E>(
    dir: impl AsRef<Path>,
    parse_id: impl Fn(&str) -> Result<T, E>,
) -> Result<Vec<T>, StoreError> {
    let dir = dir.as_ref();
    if !dir.exists() {
        return Ok(Vec::new());
    }

    let mut values = Vec::new();
    for entry in fs::read_dir(dir).map_err(io_error)? {
        let entry = entry.map_err(io_error)?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let raw_id = name
            .split('.')
            .next()
            .ok_or_else(|| StoreError::new(format!("invalid entry name: {name}")))?;
        let id =
            parse_id(raw_id).map_err(|_| StoreError::new(format!("invalid id entry '{name}'")))?;
        values.push(id);
    }
    Ok(values)
}

fn write_json(path: impl AsRef<Path>, value: &impl serde::Serialize) -> Result<(), StoreError> {
    let path = path.as_ref();
    if let Some(parent) = path.parent() {
        create_dir_all(parent)?;
    }
    let bytes = serde_json::to_vec_pretty(value).map_err(json_error)?;
    fs::write(path, bytes).map_err(io_error)
}

fn read_json<T: serde::de::DeserializeOwned>(path: impl AsRef<Path>) -> Result<T, StoreError> {
    let bytes = fs::read(path).map_err(io_error)?;
    serde_json::from_slice(&bytes).map_err(json_error)
}

fn append_line(path: impl AsRef<Path>, line: &str) -> Result<(), StoreError> {
    let path = path.as_ref();
    if let Some(parent) = path.parent() {
        create_dir_all(parent)?;
    }
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(io_error)?;
    writeln!(file, "{line}").map_err(io_error)
}

fn code_extension(language: CodeLanguage) -> &'static str {
    match language {
        CodeLanguage::Python => "py",
    }
}

fn create_dir_all(path: impl AsRef<Path>) -> Result<(), StoreError> {
    fs::create_dir_all(path).map_err(io_error)
}

fn io_error(error: std::io::Error) -> StoreError {
    StoreError::new(error.to_string())
}

fn json_error(error: serde_json::Error) -> StoreError {
    StoreError::new(error.to_string())
}
