use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use crate::error::{AgentError, EnvironmentError, EvaluatorError};
use crate::orchestrator::Orchestrator;
use crate::traits::{Agent, Environment, Evaluator};
use crate::types::{
    AgentDecision, AgentTurnInput, EvalResult, FinishReason, RunLimits, RunRequest, TaskSpec,
    Trajectory, TrajectoryEvent,
};

#[derive(Debug, Clone, PartialEq, Eq)]
struct TestObservation(&'static str);

#[derive(Debug, Clone, PartialEq, Eq)]
struct TestAction(&'static str);

#[derive(Debug, Clone, PartialEq, Eq)]
struct TestOutcome(&'static str);

#[derive(Debug, Clone, PartialEq, Eq)]
struct TestSnapshot(&'static str);

struct FakeAgent {
    decisions: VecDeque<Result<AgentDecision<TestAction>, AgentError>>,
}

#[async_trait]
impl Agent for FakeAgent {
    type Observation = TestObservation;
    type Action = TestAction;
    type Outcome = TestOutcome;

    async fn next_decision(
        &mut self,
        _input: AgentTurnInput<Self::Observation, Self::Action, Self::Outcome>,
    ) -> Result<AgentDecision<Self::Action>, AgentError> {
        self.decisions
            .pop_front()
            .unwrap_or_else(|| Ok(AgentDecision::Finish {
                reason: FinishReason::AgentFinished,
            }))
    }
}

#[derive(Clone)]
struct FakeEnvironment {
    observation: TestObservation,
    terminal: Option<String>,
    outcomes: VecDeque<Result<TestOutcome, EnvironmentError>>,
}

#[async_trait]
impl Environment for FakeEnvironment {
    type Observation = TestObservation;
    type Action = TestAction;
    type Outcome = TestOutcome;
    type Snapshot = TestSnapshot;

    async fn reset(&mut self, _task: &TaskSpec) -> Result<Self::Observation, EnvironmentError> {
        Ok(self.observation.clone())
    }

    async fn observe(&self) -> Result<Self::Observation, EnvironmentError> {
        Ok(self.observation.clone())
    }

    async fn step(
        &mut self,
        _action: Self::Action,
    ) -> Result<Self::Outcome, EnvironmentError> {
        self.outcomes
            .pop_front()
            .unwrap_or_else(|| Ok(TestOutcome("ok")))
    }

    async fn snapshot(&self) -> Result<Self::Snapshot, EnvironmentError> {
        Ok(TestSnapshot("snapshot"))
    }

    async fn restore(&mut self, _snapshot: &Self::Snapshot) -> Result<(), EnvironmentError> {
        Ok(())
    }

    async fn terminal_status(&self) -> Result<Option<String>, EnvironmentError> {
        Ok(self.terminal.clone())
    }
}

struct RecordingEvaluator {
    calls: Arc<Mutex<usize>>,
}

#[async_trait]
impl Evaluator<TestObservation, TestAction, TestOutcome> for RecordingEvaluator {
    async fn evaluate(
        &self,
        _task: &TaskSpec,
        _trajectory: &Trajectory<TestObservation, TestAction, TestOutcome>,
    ) -> Result<EvalResult, EvaluatorError> {
        *self.calls.lock().unwrap() += 1;
        Ok(EvalResult {
            passed: true,
            score: Some(1.0),
            summary: Some("ok".to_owned()),
        })
    }
}

fn test_request() -> RunRequest {
    RunRequest {
        task: TaskSpec {
            id: "task-1".to_owned(),
            instructions: "solve it".to_owned(),
        },
        limits: RunLimits {
            max_steps: 8,
            max_actions: 8,
            max_messages: 8,
            max_duration: None,
        },
    }
}

#[tokio::test]
async fn orchestrator_exits_on_immediate_finish() {
    let agent = FakeAgent {
        decisions: VecDeque::from([Ok(AgentDecision::Finish {
            reason: FinishReason::AgentFinished,
        })]),
    };
    let environment = FakeEnvironment {
        observation: TestObservation("initial"),
        terminal: None,
        outcomes: VecDeque::new(),
    };
    let mut orchestrator = Orchestrator::new(agent, environment);

    let result = orchestrator.run(test_request()).await.unwrap();

    assert_eq!(result.finish_reason, FinishReason::AgentFinished);
    assert!(matches!(
        result.trajectory.events[1],
        TrajectoryEvent::ObservationRecorded { .. }
    ));
}

#[tokio::test]
async fn orchestrator_records_messages_actions_and_results() {
    let agent = FakeAgent {
        decisions: VecDeque::from([
            Ok(AgentDecision::Message {
                content: "thinking".to_owned(),
            }),
            Ok(AgentDecision::EnvironmentAction {
                action: TestAction("run"),
            }),
            Ok(AgentDecision::Finish {
                reason: FinishReason::AgentFinished,
            }),
        ]),
    };
    let environment = FakeEnvironment {
        observation: TestObservation("initial"),
        terminal: None,
        outcomes: VecDeque::from([Ok(TestOutcome("done"))]),
    };
    let mut orchestrator = Orchestrator::new(agent, environment);

    let result = orchestrator.run(test_request()).await.unwrap();

    assert!(result.trajectory.events.iter().any(|event| matches!(
        event,
        TrajectoryEvent::AgentMessage { content } if content == "thinking"
    )));
    assert!(result.trajectory.events.iter().any(|event| matches!(
        event,
        TrajectoryEvent::EnvironmentActionCompleted { outcome, .. } if *outcome == TestOutcome("done")
    )));
}

#[tokio::test]
async fn orchestrator_enforces_action_budget() {
    let agent = FakeAgent {
        decisions: VecDeque::from([Ok(AgentDecision::EnvironmentAction {
            action: TestAction("run"),
        })]),
    };
    let environment = FakeEnvironment {
        observation: TestObservation("initial"),
        terminal: None,
        outcomes: VecDeque::from([Ok(TestOutcome("done"))]),
    };
    let mut orchestrator = Orchestrator::new(agent, environment);
    let mut request = test_request();
    request.limits.max_actions = 0;

    let result = orchestrator.run(request).await.unwrap();

    assert_eq!(result.finish_reason, FinishReason::ActionLimitExceeded);
}

#[tokio::test]
async fn orchestrator_runs_evaluator_once_when_present() {
    let calls = Arc::new(Mutex::new(0usize));
    let agent = FakeAgent {
        decisions: VecDeque::from([Ok(AgentDecision::Finish {
            reason: FinishReason::AgentFinished,
        })]),
    };
    let environment = FakeEnvironment {
        observation: TestObservation("initial"),
        terminal: None,
        outcomes: VecDeque::new(),
    };
    let evaluator = RecordingEvaluator {
        calls: Arc::clone(&calls),
    };
    let mut orchestrator = Orchestrator::with_evaluator(agent, environment, evaluator);

    let result = orchestrator.run(test_request()).await.unwrap();

    assert_eq!(*calls.lock().unwrap(), 1);
    assert_eq!(result.evaluation.unwrap().score, Some(1.0));
}
