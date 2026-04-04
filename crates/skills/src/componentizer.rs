use std::path::Path;
use std::process::Command as ProcessCommand;

use crate::error::SkillProvisionError;

const COMPONENTIZE_PY_VERSION: &str = "0.21.0";

pub trait Componentizer: Clone + Send + Sync + 'static {
    fn generate_bindings(
        &self,
        wit_path: &Path,
        world: &str,
        out_dir: &Path,
    ) -> Result<(), SkillProvisionError>;

    fn componentize(
        &self,
        cwd: &Path,
        wit_path: &Path,
        world: &str,
        module: &str,
        output: &Path,
    ) -> Result<(), SkillProvisionError>;
}

#[derive(Debug, Clone)]
pub struct UvxComponentizer {
    uvx: String,
}

impl Default for UvxComponentizer {
    fn default() -> Self {
        Self {
            uvx: "uvx".to_owned(),
        }
    }
}

impl UvxComponentizer {
    pub fn new(uvx: impl Into<String>) -> Self {
        Self { uvx: uvx.into() }
    }

    fn run(
        &self,
        cwd: Option<&Path>,
        args: impl IntoIterator<Item = String>,
    ) -> Result<(), SkillProvisionError> {
        let mut command = ProcessCommand::new(&self.uvx);
        command
            .arg("--from")
            .arg(format!("componentize-py=={COMPONENTIZE_PY_VERSION}"))
            .arg("componentize-py");
        if let Some(cwd) = cwd {
            command.current_dir(cwd);
        }
        for arg in args {
            command.arg(arg);
        }

        let output = command.output().map_err(|source| {
            if source.kind() == std::io::ErrorKind::NotFound {
                SkillProvisionError::ToolNotInstalled {
                    tool: "uvx",
                    hint: "Install uv or pass --uvx <path-to-uvx>.".to_owned(),
                }
            } else {
                SkillProvisionError::ToolIo { tool: "uvx", source }
            }
        })?;

        if !output.status.success() {
            return Err(SkillProvisionError::ToolFailed {
                tool: "uvx componentize-py",
                status: output.status,
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            });
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        if !stdout.trim().is_empty() {
            print!("{stdout}");
        }

        Ok(())
    }
}

impl Componentizer for UvxComponentizer {
    fn generate_bindings(
        &self,
        wit_path: &Path,
        world: &str,
        out_dir: &Path,
    ) -> Result<(), SkillProvisionError> {
        self.run(
            None,
            [
                "--wit-path".to_owned(),
                wit_path.display().to_string(),
                "--world".to_owned(),
                world.to_owned(),
                "bindings".to_owned(),
                out_dir.display().to_string(),
            ],
        )
    }

    fn componentize(
        &self,
        cwd: &Path,
        wit_path: &Path,
        world: &str,
        module: &str,
        output: &Path,
    ) -> Result<(), SkillProvisionError> {
        self.run(
            Some(cwd),
            [
                "--wit-path".to_owned(),
                wit_path.display().to_string(),
                "--world".to_owned(),
                world.to_owned(),
                "componentize".to_owned(),
                module.to_owned(),
                "-o".to_owned(),
                output.display().to_string(),
            ],
        )
    }
}
