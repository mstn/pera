mod engine;
mod execution;
mod evaluator;
mod error;
mod overrides;
mod runner;
mod spec;
mod user;

pub use engine::{EvalEngine, EvalMode, EvalRequest, EvalSession};
pub use execution::{
    EvalJudgeResult, EvalPreparation, EvalProjectLayout, EvalRunResult, EvalRunWorkspace, EvalTraceEvent,
    EvalTrajectoryActionRunStatus, EvalTrajectoryEvent, EvalTrajectoryPayload,
    PreparedCatalogSkill, SerializedAction, SerializedOutcome,
};
pub use evaluator::{
    EvalActionAdapter, EvalJudge, EvalJudgeRequest, EvalJudgeResultPayload, SpecEvaluator,
    build_llm_judge_requests, build_optimization_suggestion_request,
    parse_judge_verdict, parse_optimization_suggestions, serialize_trajectory_events,
    trajectory_trace_events, EvalOptimizationSuggestionRequest,
    EvalOptimizationSuggestionsResponse, EvalOptimizationTargetSuggestion,
    EvalOptimizationTargetSuggestionValue,
};
pub use error::EvalError;
pub use overrides::OverrideSet;
pub use runner::EvalRunner;
pub use spec::{
    EvalAgentSpec, EvalCatalogSkillSpec, EvalCriterionSpec, EvalEvaluationSpec,
    EvalExpectedActionSpec, EvalHistoryMessage, EvalOptimizationSpec,
    EvalOptimizationTargetSpec, EvalRuntimeSpec, EvalScenarioSpec, EvalSkillSourceSpec,
    EvalSpec, EvalUserSpec, LoadedEvalSpec, load_eval_spec,
};
pub use user::ScriptedUserParticipant;
