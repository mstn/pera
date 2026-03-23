use crate::{CodeArtifactId, InputName};

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum CodeLanguage {
    Python,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ScriptName(String);

impl ScriptName {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CodeArtifact {
    pub id: CodeArtifactId,
    pub language: CodeLanguage,
    pub script_name: ScriptName,
    pub source: String,
    pub inputs: Vec<InputName>,
}
