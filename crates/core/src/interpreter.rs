use std::collections::BTreeMap;
use std::error::Error;
use std::fmt::{Display, Formatter};

use crate::{ActionName, CodeArtifact, InputName, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum InterpreterKind {
    Monty,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CompiledProgram {
    pub kind: InterpreterKind,
    pub input_order: Vec<InputName>,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ExecutionSnapshot {
    pub kind: InterpreterKind,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ExternalCall {
    pub action_name: ActionName,
    #[serde(default)]
    pub positional_arguments: Vec<Value>,
    #[serde(default)]
    pub named_arguments: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Suspension {
    pub snapshot: ExecutionSnapshot,
    pub call: ExternalCall,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ExecutionOutput {
    pub value: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum InterpreterStep {
    Suspended(Suspension),
    Completed(ExecutionOutput),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InterpreterError {
    message: String,
}

impl InterpreterError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl Display for InterpreterError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl Error for InterpreterError {}

pub type InputValues = BTreeMap<InputName, Value>;

pub trait Interpreter {
    fn kind(&self) -> InterpreterKind;

    fn compile(&self, code: &CodeArtifact) -> Result<CompiledProgram, InterpreterError>;

    fn start(
        &self,
        program: &CompiledProgram,
        inputs: &InputValues,
    ) -> Result<InterpreterStep, InterpreterError>;

    fn resume(
        &self,
        snapshot: &ExecutionSnapshot,
        return_value: &Value,
    ) -> Result<InterpreterStep, InterpreterError>;
}
