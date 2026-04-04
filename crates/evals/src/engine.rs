use std::path::PathBuf;

use crate::error::EvalError;
use crate::execution::EvalPreparation;
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
}
