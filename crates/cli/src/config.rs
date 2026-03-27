use std::path::PathBuf;

use crate::error::CliError;

#[derive(Debug, Clone, Default)]
pub struct OpenAiConfig {
    pub api_key: String,
    pub model: String,
}

#[derive(Debug, Clone, Default)]
pub struct AgentConfig {
    pub project_root: PathBuf,
    pub openai: Option<OpenAiConfig>,
}

impl AgentConfig {
    pub fn from_openai(
        project_root: PathBuf,
        api_key: Option<String>,
        model: Option<String>,
    ) -> Result<Self, CliError> {
        match (
            api_key.map(|value| value.trim().to_owned()).filter(|value| !value.is_empty()),
            model.map(|value| value.trim().to_owned()).filter(|value| !value.is_empty()),
        ) {
            (None, None) => Ok(Self {
                project_root,
                openai: None,
            }),
            (Some(api_key), Some(model)) => Ok(Self {
                project_root,
                openai: Some(OpenAiConfig { api_key, model }),
            }),
            (Some(_), None) => Err(CliError::InvalidArguments(
                "OPENAI_MODEL is required when OPENAI_API_KEY is set",
            )),
            (None, Some(_)) => Err(CliError::InvalidArguments(
                "OPENAI_API_KEY is required when OPENAI_MODEL is set",
            )),
        }
    }
}
