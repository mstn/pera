use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use pera_core::ActionId;

use crate::error::{EnvironmentError, EvaluatorError, ParticipantError};
use crate::orchestrator::Orchestrator;
use crate::streaming::ParticipantOutput;
use crate::traits::{Environment, Evaluator, Participant};
use crate::types::{
    ActionError, ActionExecution, EnvironmentEvent, EvalResult, FinishReason,
    InitialInboxMessage, ParticipantDecision, ParticipantId, ParticipantInboxEvent,
    ParticipantInput, RunLimits, RunRequest, ScheduledAction, TaskSpec, TerminationCondition,
    Trajectory, TrajectoryEvent, WorkItem,
};

#[derive(Debug, Clone, PartialEq, Eq)]
struct TestObservation(&'static str);

#[derive(Debug, Clone, PartialEq, Eq)]
struct TestAction(&'static str);

#[derive(Debug, Clone, PartialEq, Eq)]
struct TestOutcome(&'static str);

#[derive(Debug, Clone, PartialEq, Eq)]
struct TestSnapshot(&'static str);

struct FakeParticipant {
    id: ParticipantId,
    decisions: VecDeque<Result<ParticipantDecision<TestAction>, ParticipantError>>,
    seen_inboxes: Arc<Mutex<Vec<Vec<ParticipantInboxEvent<TestAction, TestOutcome>>>>>,
    seen_work_items: Arc<Mutex<Vec<Option<WorkItem>>>>,
}

#[async_trait]
impl Participant for FakeParticipant {
    type Observation = TestObservation;
    type Action = TestAction;
    type Outcome = TestOutcome;

    fn id(&self) -> ParticipantId {
        self.id.clone()
    }

    async fn respond(
        &mut self,
        input: ParticipantInput<Self::Observation, Self::Action, Self::Outcome>,
        _output: &mut dyn ParticipantOutput<Self::Action, Self::Outcome>,
    ) -> Result<ParticipantDecision<Self::Action>, ParticipantError> {
        self.seen_inboxes.lock().unwrap().push(input.inbox);
        self.seen_work_items
            .lock()
            .unwrap()
            .push(input.work_item);
        self.decisions
            .pop_front()
            .unwrap_or(Ok(ParticipantDecision::Finish))
    }
}

struct FakeEnvironment {
    observation: TestObservation,
    terminal: Option<String>,
    immediate_outcomes: VecDeque<Result<TestOutcome, EnvironmentError>>,
    submitted_events: VecDeque<Vec<EnvironmentEvent<TestAction, TestOutcome>>>,
    submitted_ids: VecDeque<ActionId>,
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

    async fn perform_now(
        &mut self,
        _actor: ParticipantId,
        _action: Self::Action,
    ) -> Result<Self::Outcome, EnvironmentError> {
        self.immediate_outcomes
            .pop_front()
            .unwrap_or(Ok(TestOutcome("ok")))
    }

    async fn schedule(
        &mut self,
        _actor: ParticipantId,
        _action: Self::Action,
    ) -> Result<ScheduledAction, ActionError> {
        Ok(ScheduledAction {
            action_id: self.submitted_ids.pop_front().unwrap(),
        })
    }

    async fn poll_events(
        &mut self,
    ) -> Result<Vec<EnvironmentEvent<Self::Action, Self::Outcome>>, EnvironmentError> {
        Ok(self.submitted_events.pop_front().unwrap_or_default())
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
            max_steps: 12,
            max_steps_per_agent_loop: 12,
            max_actions: 12,
            max_messages: 12,
            max_failed_actions: None,
            max_consecutive_failed_actions: None,
            max_blocked_action_wait: None,
            max_duration: None,
        },
        termination_condition: TerminationCondition::AllParticipantsFinished,
        initial_messages: Vec::new(),
    }
}

#[tokio::test]
async fn orchestrator_handles_single_participant_immediate_action() {
    let agent = FakeParticipant {
        id: ParticipantId::Agent,
        decisions: VecDeque::from([
            Ok(ParticipantDecision::Action {
                message: None,
                action: TestAction("run"),
                execution: ActionExecution::Immediate,
            }),
            Ok(ParticipantDecision::Finish),
        ]),
        seen_inboxes: Arc::new(Mutex::new(Vec::new())),
        seen_work_items: Arc::new(Mutex::new(Vec::new())),
    };
    let user = FakeParticipant {
        id: ParticipantId::User,
        decisions: VecDeque::from([Ok(ParticipantDecision::Finish)]),
        seen_inboxes: Arc::new(Mutex::new(Vec::new())),
        seen_work_items: Arc::new(Mutex::new(Vec::new())),
    };
    let environment = FakeEnvironment {
        observation: TestObservation("initial"),
        terminal: None,
        immediate_outcomes: VecDeque::from([Ok(TestOutcome("done"))]),
        submitted_events: VecDeque::new(),
        submitted_ids: VecDeque::new(),
    };
    let participants = vec![
        Box::new(user)
            as Box<
                dyn Participant<
                        Observation = TestObservation,
                        Action = TestAction,
                        Outcome = TestOutcome,
                    >,
            >,
        Box::new(agent)
            as Box<
                dyn Participant<
                        Observation = TestObservation,
                        Action = TestAction,
                        Outcome = TestOutcome,
                    >,
            >,
    ];
    let mut orchestrator = Orchestrator::from_participants(participants, environment);
    let mut request = test_request();
    request.initial_messages.push(InitialInboxMessage {
        to: ParticipantId::Agent,
        from: ParticipantId::User,
        content: "go".to_owned(),
    });
    request.initial_messages.push(InitialInboxMessage {
        to: ParticipantId::User,
        from: ParticipantId::Custom("system".to_owned()),
        content: "done".to_owned(),
    });

    let result = orchestrator.run(request).await.unwrap();

    assert_eq!(result.finish_reason, FinishReason::ParticipantsFinished);
    assert!(result.trajectory.events.iter().any(|event| matches!(
        event,
        TrajectoryEvent::ActionCompleted { outcome, .. } if *outcome == TestOutcome("done")
    )));
}

#[tokio::test]
async fn orchestrator_delivers_immediate_completion_via_inbox() {
    let seen_inboxes = Arc::new(Mutex::new(Vec::new()));
    let agent = FakeParticipant {
        id: ParticipantId::Agent,
        decisions: VecDeque::from([
            Ok(ParticipantDecision::Action {
                message: None,
                action: TestAction("run"),
                execution: ActionExecution::Immediate,
            }),
            Ok(ParticipantDecision::Finish),
        ]),
        seen_inboxes: Arc::clone(&seen_inboxes),
        seen_work_items: Arc::new(Mutex::new(Vec::new())),
    };
    let user = FakeParticipant {
        id: ParticipantId::User,
        decisions: VecDeque::from([Ok(ParticipantDecision::Finish)]),
        seen_inboxes: Arc::new(Mutex::new(Vec::new())),
        seen_work_items: Arc::new(Mutex::new(Vec::new())),
    };
    let environment = FakeEnvironment {
        observation: TestObservation("initial"),
        terminal: None,
        immediate_outcomes: VecDeque::from([Ok(TestOutcome("done"))]),
        submitted_events: VecDeque::new(),
        submitted_ids: VecDeque::new(),
    };
    let participants = vec![
        Box::new(user)
            as Box<
                dyn Participant<
                        Observation = TestObservation,
                        Action = TestAction,
                        Outcome = TestOutcome,
                    >,
            >,
        Box::new(agent)
            as Box<
                dyn Participant<
                        Observation = TestObservation,
                        Action = TestAction,
                        Outcome = TestOutcome,
                    >,
            >,
    ];
    let mut orchestrator = Orchestrator::from_participants(participants, environment);
    let mut request = test_request();
    request.initial_messages.push(InitialInboxMessage {
        to: ParticipantId::Agent,
        from: ParticipantId::User,
        content: "go".to_owned(),
    });
    request.initial_messages.push(InitialInboxMessage {
        to: ParticipantId::User,
        from: ParticipantId::Custom("system".to_owned()),
        content: "done".to_owned(),
    });

    let result = orchestrator.run(request).await.unwrap();

    assert_eq!(result.finish_reason, FinishReason::ParticipantsFinished);
    let inboxes = seen_inboxes.lock().unwrap();
    assert!(
        inboxes.iter().any(|inbox| {
            inbox.iter().any(|event| matches!(
        event,
        ParticipantInboxEvent::ActionCompleted { outcome, .. } if *outcome == TestOutcome("done")
    ))
        })
    );
}

#[tokio::test]
async fn orchestrator_delivers_deferred_completion_via_inbox() {
    let seen_inboxes = Arc::new(Mutex::new(Vec::new()));
    let submitted_action_id = ActionId::parse_str("00000000-0000-0000-0000-000000000123").unwrap();
    let agent = FakeParticipant {
        id: ParticipantId::Agent,
        decisions: VecDeque::from([
            Ok(ParticipantDecision::Action {
                message: None,
                action: TestAction("background"),
                execution: ActionExecution::DeferredNonBlocking,
            }),
            Ok(ParticipantDecision::Yield),
            Ok(ParticipantDecision::Finish),
        ]),
        seen_inboxes: Arc::clone(&seen_inboxes),
        seen_work_items: Arc::new(Mutex::new(Vec::new())),
    };
    let user = FakeParticipant {
        id: ParticipantId::User,
        decisions: VecDeque::from([Ok(ParticipantDecision::Finish)]),
        seen_inboxes: Arc::new(Mutex::new(Vec::new())),
        seen_work_items: Arc::new(Mutex::new(Vec::new())),
    };
    let environment = FakeEnvironment {
        observation: TestObservation("initial"),
        terminal: None,
        immediate_outcomes: VecDeque::new(),
        submitted_events: VecDeque::from([
            Vec::new(),
            Vec::new(),
            vec![EnvironmentEvent::ActionCompleted {
                participant: ParticipantId::Agent,
                action_id: submitted_action_id,
                outcome: TestOutcome("completed"),
            }],
        ]),
        submitted_ids: VecDeque::from([submitted_action_id]),
    };
    let participants = vec![
        Box::new(user)
            as Box<
                dyn Participant<
                        Observation = TestObservation,
                        Action = TestAction,
                        Outcome = TestOutcome,
                    >,
            >,
        Box::new(agent)
            as Box<
                dyn Participant<
                        Observation = TestObservation,
                        Action = TestAction,
                        Outcome = TestOutcome,
                    >,
            >,
    ];
    let mut orchestrator = Orchestrator::from_participants(participants, environment);
    let mut request = test_request();
    request.initial_messages.push(InitialInboxMessage {
        to: ParticipantId::Agent,
        from: ParticipantId::User,
        content: "go".to_owned(),
    });
    request.initial_messages.push(InitialInboxMessage {
        to: ParticipantId::User,
        from: ParticipantId::Custom("system".to_owned()),
        content: "done".to_owned(),
    });

    let result = orchestrator.run(request).await.unwrap();
    let inboxes = seen_inboxes.lock().unwrap();

    assert!(
        inboxes
            .iter()
            .any(|inbox| inbox.iter().any(|event| matches!(
                event,
                ParticipantInboxEvent::ActionCompleted { action_id, outcome }
                    if *action_id == submitted_action_id && *outcome == TestOutcome("completed")
            )))
    );
    assert!(result.trajectory.events.iter().any(|event| matches!(
        event,
        TrajectoryEvent::ActionScheduled { action_id, .. } if *action_id == submitted_action_id
    )));
}

#[tokio::test]
async fn orchestrator_starts_a_new_agent_loop_for_a_second_user_message() {
    let seen_work_items = Arc::new(Mutex::new(Vec::new()));
    let agent = FakeParticipant {
        id: ParticipantId::Agent,
        decisions: VecDeque::from([
            Ok(ParticipantDecision::CompleteLoop {
                content: "reply 1".to_owned(),
            }),
            Ok(ParticipantDecision::CompleteLoop {
                content: "reply 2".to_owned(),
            }),
            Ok(ParticipantDecision::Finish),
        ]),
        seen_inboxes: Arc::new(Mutex::new(Vec::new())),
        seen_work_items: Arc::clone(&seen_work_items),
    };
    let user = FakeParticipant {
        id: ParticipantId::User,
        decisions: VecDeque::from([
            Ok(ParticipantDecision::CompleteLoop {
                content: "request 1".to_owned(),
            }),
            Ok(ParticipantDecision::CompleteLoop {
                content: "request 2".to_owned(),
            }),
            Ok(ParticipantDecision::Finish),
        ]),
        seen_inboxes: Arc::new(Mutex::new(Vec::new())),
        seen_work_items: Arc::new(Mutex::new(Vec::new())),
    };
    let environment = FakeEnvironment {
        observation: TestObservation("initial"),
        terminal: None,
        immediate_outcomes: VecDeque::new(),
        submitted_events: VecDeque::new(),
        submitted_ids: VecDeque::new(),
    };
    let participants = vec![
        Box::new(user)
            as Box<
                dyn Participant<
                        Observation = TestObservation,
                        Action = TestAction,
                        Outcome = TestOutcome,
                    >,
            >,
        Box::new(agent)
            as Box<
                dyn Participant<
                        Observation = TestObservation,
                        Action = TestAction,
                        Outcome = TestOutcome,
                    >,
            >,
    ];
    let mut orchestrator = Orchestrator::from_participants(participants, environment);
    let mut request = test_request();
    request.termination_condition =
        TerminationCondition::AnyOfParticipantsFinished(vec![ParticipantId::User]);
    request.initial_messages.push(InitialInboxMessage {
        to: ParticipantId::User,
        from: ParticipantId::Custom("system".to_owned()),
        content: "start".to_owned(),
    });

    let result = orchestrator.run(request).await.unwrap();

    assert_eq!(
        result.finish_reason,
        FinishReason::ParticipantFinished {
            participant: ParticipantId::User,
        }
    );
    let seen_work_items = seen_work_items.lock().unwrap();
    assert_eq!(
        seen_work_items.as_slice(),
        &[
            Some(WorkItem {
                id: seen_work_items[0].as_ref().unwrap().id,
                from: ParticipantId::User,
                content: "request 1".to_owned(),
            }),
            Some(WorkItem {
                id: seen_work_items[1].as_ref().unwrap().id,
                from: ParticipantId::User,
                content: "request 2".to_owned(),
            }),
        ]
    );
}

#[tokio::test]
async fn orchestrator_blocks_participant_on_deferred_blocking_action() {
    let seen_inboxes = Arc::new(Mutex::new(Vec::new()));
    let submitted_action_id = ActionId::parse_str("00000000-0000-0000-0000-000000000124").unwrap();
    let agent = FakeParticipant {
        id: ParticipantId::Agent,
        decisions: VecDeque::from([
            Ok(ParticipantDecision::Action {
                message: None,
                action: TestAction("blocking"),
                execution: ActionExecution::DeferredBlocking,
            }),
            Ok(ParticipantDecision::Finish),
        ]),
        seen_inboxes: Arc::clone(&seen_inboxes),
        seen_work_items: Arc::new(Mutex::new(Vec::new())),
    };
    let user = FakeParticipant {
        id: ParticipantId::User,
        decisions: VecDeque::from([Ok(ParticipantDecision::Finish)]),
        seen_inboxes: Arc::new(Mutex::new(Vec::new())),
        seen_work_items: Arc::new(Mutex::new(Vec::new())),
    };
    let environment = FakeEnvironment {
        observation: TestObservation("initial"),
        terminal: None,
        immediate_outcomes: VecDeque::new(),
        submitted_events: VecDeque::from([
            Vec::new(),
            Vec::new(),
            vec![EnvironmentEvent::ActionCompleted {
                participant: ParticipantId::Agent,
                action_id: submitted_action_id,
                outcome: TestOutcome("completed"),
            }],
        ]),
        submitted_ids: VecDeque::from([submitted_action_id]),
    };
    let participants = vec![
        Box::new(user)
            as Box<
                dyn Participant<
                        Observation = TestObservation,
                        Action = TestAction,
                        Outcome = TestOutcome,
                    >,
            >,
        Box::new(agent)
            as Box<
                dyn Participant<
                        Observation = TestObservation,
                        Action = TestAction,
                        Outcome = TestOutcome,
                    >,
            >,
    ];
    let mut orchestrator = Orchestrator::from_participants(participants, environment);
    let mut request = test_request();
    request.initial_messages.push(InitialInboxMessage {
        to: ParticipantId::Agent,
        from: ParticipantId::User,
        content: "go".to_owned(),
    });
    request.initial_messages.push(InitialInboxMessage {
        to: ParticipantId::User,
        from: ParticipantId::Custom("system".to_owned()),
        content: "done".to_owned(),
    });

    let result = orchestrator.run(request).await.unwrap();
    let inboxes = seen_inboxes.lock().unwrap();

    assert_eq!(result.finish_reason, FinishReason::ParticipantsFinished);
    assert_eq!(inboxes.len(), 2);
    assert!(inboxes[1].iter().any(|event| matches!(
        event,
        ParticipantInboxEvent::ActionCompleted { action_id, outcome }
            if *action_id == submitted_action_id && *outcome == TestOutcome("completed")
    )));
}

#[tokio::test]
async fn orchestrator_resumes_blocked_participant_after_deferred_action_failure() {
    let seen_inboxes = Arc::new(Mutex::new(Vec::new()));
    let submitted_action_id = ActionId::parse_str("00000000-0000-0000-0000-000000000128").unwrap();
    let agent = FakeParticipant {
        id: ParticipantId::Agent,
        decisions: VecDeque::from([
            Ok(ParticipantDecision::Action {
                message: None,
                action: TestAction("blocking"),
                execution: ActionExecution::DeferredBlocking,
            }),
            Ok(ParticipantDecision::Finish),
        ]),
        seen_inboxes: Arc::clone(&seen_inboxes),
        seen_work_items: Arc::new(Mutex::new(Vec::new())),
    };
    let user = FakeParticipant {
        id: ParticipantId::User,
        decisions: VecDeque::from([Ok(ParticipantDecision::Finish)]),
        seen_inboxes: Arc::new(Mutex::new(Vec::new())),
        seen_work_items: Arc::new(Mutex::new(Vec::new())),
    };
    let environment = FakeEnvironment {
        observation: TestObservation("initial"),
        terminal: None,
        immediate_outcomes: VecDeque::new(),
        submitted_events: VecDeque::from([
            Vec::new(),
            Vec::new(),
            vec![EnvironmentEvent::ActionFailed {
                participant: ParticipantId::Agent,
                action_id: submitted_action_id,
                error: ActionError {
                    user_message: "The generated code could not be executed.".to_owned(),
                    detail: "interpreter error".to_owned(),
                    origin: crate::types::ActionErrorOrigin::Interpreter,
                },
            }],
        ]),
        submitted_ids: VecDeque::from([submitted_action_id]),
    };
    let participants = vec![
        Box::new(user)
            as Box<
                dyn Participant<
                        Observation = TestObservation,
                        Action = TestAction,
                        Outcome = TestOutcome,
                    >,
            >,
        Box::new(agent)
            as Box<
                dyn Participant<
                        Observation = TestObservation,
                        Action = TestAction,
                        Outcome = TestOutcome,
                    >,
            >,
    ];
    let mut orchestrator = Orchestrator::from_participants(participants, environment);
    let mut request = test_request();
    request.initial_messages.push(InitialInboxMessage {
        to: ParticipantId::Agent,
        from: ParticipantId::User,
        content: "go".to_owned(),
    });
    request.initial_messages.push(InitialInboxMessage {
        to: ParticipantId::User,
        from: ParticipantId::Custom("system".to_owned()),
        content: "done".to_owned(),
    });

    let result = orchestrator.run(request).await.unwrap();
    let inboxes = seen_inboxes.lock().unwrap();

    assert_eq!(result.finish_reason, FinishReason::ParticipantsFinished);
    assert_eq!(inboxes.len(), 2);
    assert!(inboxes[1].iter().any(|event| matches!(
        event,
        ParticipantInboxEvent::ActionFailed { action_id, error }
            if *action_id == submitted_action_id
                && error.user_message == "The generated code could not be executed."
    )));
}

#[tokio::test]
async fn orchestrator_starts_new_loop_from_idle_action_failure() {
    let seen_inboxes = Arc::new(Mutex::new(Vec::new()));
    let seen_work_items = Arc::new(Mutex::new(Vec::new()));
    let submitted_action_id = ActionId::parse_str("00000000-0000-0000-0000-000000000129").unwrap();
    let agent = FakeParticipant {
        id: ParticipantId::Agent,
        decisions: VecDeque::from([
            Ok(ParticipantDecision::Action {
                message: None,
                action: TestAction("background"),
                execution: ActionExecution::DeferredNonBlocking,
            }),
            Ok(ParticipantDecision::CompleteLoop {
                content: "retrying after failure".to_owned(),
            }),
            Ok(ParticipantDecision::Finish),
        ]),
        seen_inboxes: Arc::clone(&seen_inboxes),
        seen_work_items: Arc::clone(&seen_work_items),
    };
    let environment = FakeEnvironment {
        observation: TestObservation("initial"),
        terminal: None,
        immediate_outcomes: VecDeque::new(),
        submitted_events: VecDeque::from([
            Vec::new(),
            Vec::new(),
            Vec::new(),
            vec![EnvironmentEvent::ActionFailed {
                participant: ParticipantId::Agent,
                action_id: submitted_action_id,
                error: ActionError {
                    user_message: "The generated code could not be executed.".to_owned(),
                    detail: "interpreter error".to_owned(),
                    origin: crate::types::ActionErrorOrigin::Interpreter,
                },
            }],
        ]),
        submitted_ids: VecDeque::from([submitted_action_id]),
    };
    let participants = vec![Box::new(agent)
        as Box<
            dyn Participant<
                    Observation = TestObservation,
                    Action = TestAction,
                    Outcome = TestOutcome,
                >,
        >];
    let mut orchestrator = Orchestrator::from_participants(participants, environment);
    let mut request = test_request();
    request.initial_messages.push(InitialInboxMessage {
        to: ParticipantId::Agent,
        from: ParticipantId::User,
        content: "go".to_owned(),
    });

    let result = orchestrator.run(request).await.unwrap();
    let inboxes = seen_inboxes.lock().unwrap();
    let work_items = seen_work_items.lock().unwrap();

    assert_eq!(result.finish_reason, FinishReason::ParticipantsFinished);
    assert_eq!(inboxes.len(), 3);
    assert!(matches!(work_items.as_slice(), [Some(_), Some(_), None]));
    assert!(inboxes[2].iter().any(|event| matches!(
        event,
        ParticipantInboxEvent::ActionFailed { action_id, error }
            if *action_id == submitted_action_id
                && error.user_message == "The generated code could not be executed."
    )));
    assert!(result.trajectory.events.iter().any(|event| matches!(
        event,
        TrajectoryEvent::ParticipantMessage { participant, content }
            if *participant == ParticipantId::Agent && content == "retrying after failure"
    )));
}

#[tokio::test]
async fn orchestrator_alternates_two_participants() {
    let seen_agent = Arc::new(Mutex::new(Vec::new()));
    let seen_user = Arc::new(Mutex::new(Vec::new()));
    let agent = FakeParticipant {
        id: ParticipantId::Agent,
        decisions: VecDeque::from([
            Ok(ParticipantDecision::Message {
                content: "hello".to_owned(),
            }),
            Ok(ParticipantDecision::Finish),
        ]),
        seen_inboxes: Arc::clone(&seen_agent),
        seen_work_items: Arc::new(Mutex::new(Vec::new())),
    };
    let user = FakeParticipant {
        id: ParticipantId::User,
        decisions: VecDeque::from([
            Ok(ParticipantDecision::Message {
                content: "hi".to_owned(),
            }),
            Ok(ParticipantDecision::Finish),
        ]),
        seen_inboxes: Arc::clone(&seen_user),
        seen_work_items: Arc::new(Mutex::new(Vec::new())),
    };
    let environment = FakeEnvironment {
        observation: TestObservation("initial"),
        terminal: None,
        immediate_outcomes: VecDeque::new(),
        submitted_events: VecDeque::new(),
        submitted_ids: VecDeque::new(),
    };
    let participants = vec![
        Box::new(agent)
            as Box<
                dyn Participant<
                        Observation = TestObservation,
                        Action = TestAction,
                        Outcome = TestOutcome,
                    >,
            >,
        Box::new(user)
            as Box<
                dyn Participant<
                        Observation = TestObservation,
                        Action = TestAction,
                        Outcome = TestOutcome,
                    >,
            >,
    ];
    let mut orchestrator = Orchestrator::from_participants(participants, environment);
    let mut request = test_request();
    request.initial_messages.push(InitialInboxMessage {
        to: ParticipantId::User,
        from: ParticipantId::Custom("system".to_owned()),
        content: "start".to_owned(),
    });

    let result = orchestrator.run(request).await.unwrap();

    assert_eq!(result.finish_reason, FinishReason::ParticipantsFinished);
    assert!(result.trajectory.events.iter().any(|event| matches!(
        event,
        TrajectoryEvent::ParticipantMessage { participant, content }
            if *participant == ParticipantId::Agent && content == "hello"
    )));
    assert!(result.trajectory.events.iter().any(|event| matches!(
        event,
        TrajectoryEvent::ParticipantMessage { participant, content }
            if *participant == ParticipantId::User && content == "hi"
    )));
}

#[tokio::test]
async fn orchestrator_runs_evaluator_once_when_present() {
    let calls = Arc::new(Mutex::new(0usize));
    let participant = FakeParticipant {
        id: ParticipantId::Agent,
        decisions: VecDeque::from([Ok(ParticipantDecision::Finish)]),
        seen_inboxes: Arc::new(Mutex::new(Vec::new())),
        seen_work_items: Arc::new(Mutex::new(Vec::new())),
    };
    let environment = FakeEnvironment {
        observation: TestObservation("initial"),
        terminal: None,
        immediate_outcomes: VecDeque::new(),
        submitted_events: VecDeque::new(),
        submitted_ids: VecDeque::new(),
    };
    let evaluator = RecordingEvaluator {
        calls: Arc::clone(&calls),
    };
    let mut orchestrator = Orchestrator::with_evaluator(participant, environment, evaluator);

    let mut request = test_request();
    request.initial_messages.push(InitialInboxMessage {
        to: ParticipantId::Agent,
        from: ParticipantId::Custom("system".to_owned()),
        content: "start".to_owned(),
    });

    let result = orchestrator.run(request).await.unwrap();

    assert_eq!(*calls.lock().unwrap(), 1);
    assert_eq!(result.evaluation.unwrap().score, Some(1.0));
}

#[tokio::test]
async fn orchestrator_can_terminate_when_a_specific_participant_finishes() {
    let agent = FakeParticipant {
        id: ParticipantId::Agent,
        decisions: VecDeque::from([Ok(ParticipantDecision::Yield)]),
        seen_inboxes: Arc::new(Mutex::new(Vec::new())),
        seen_work_items: Arc::new(Mutex::new(Vec::new())),
    };
    let user = FakeParticipant {
        id: ParticipantId::User,
        decisions: VecDeque::from([Ok(ParticipantDecision::Finish)]),
        seen_inboxes: Arc::new(Mutex::new(Vec::new())),
        seen_work_items: Arc::new(Mutex::new(Vec::new())),
    };
    let environment = FakeEnvironment {
        observation: TestObservation("initial"),
        terminal: None,
        immediate_outcomes: VecDeque::new(),
        submitted_events: VecDeque::new(),
        submitted_ids: VecDeque::new(),
    };
    let participants = vec![
        Box::new(agent)
            as Box<
                dyn Participant<
                        Observation = TestObservation,
                        Action = TestAction,
                        Outcome = TestOutcome,
                    >,
            >,
        Box::new(user)
            as Box<
                dyn Participant<
                        Observation = TestObservation,
                        Action = TestAction,
                        Outcome = TestOutcome,
                    >,
            >,
    ];
    let mut orchestrator = Orchestrator::from_participants(participants, environment);
    let mut request = test_request();
    request.termination_condition =
        TerminationCondition::AnyOfParticipantsFinished(vec![ParticipantId::User]);
    request.initial_messages.push(InitialInboxMessage {
        to: ParticipantId::User,
        from: ParticipantId::Custom("system".to_owned()),
        content: "start".to_owned(),
    });

    let result = orchestrator.run(request).await.unwrap();

    assert_eq!(
        result.finish_reason,
        FinishReason::ParticipantFinished {
            participant: ParticipantId::User,
        }
    );
}

#[tokio::test]
async fn orchestrator_can_terminate_when_a_specific_participant_completes_a_loop() {
    let agent = FakeParticipant {
        id: ParticipantId::Agent,
        decisions: VecDeque::from([Ok(ParticipantDecision::CompleteLoop {
            content: "done".to_owned(),
        })]),
        seen_inboxes: Arc::new(Mutex::new(Vec::new())),
        seen_work_items: Arc::new(Mutex::new(Vec::new())),
    };
    let user = FakeParticipant {
        id: ParticipantId::User,
        decisions: VecDeque::from([Ok(ParticipantDecision::Yield)]),
        seen_inboxes: Arc::new(Mutex::new(Vec::new())),
        seen_work_items: Arc::new(Mutex::new(Vec::new())),
    };
    let environment = FakeEnvironment {
        observation: TestObservation("initial"),
        terminal: None,
        immediate_outcomes: VecDeque::new(),
        submitted_events: VecDeque::new(),
        submitted_ids: VecDeque::new(),
    };
    let participants = vec![
        Box::new(user)
            as Box<
                dyn Participant<
                        Observation = TestObservation,
                        Action = TestAction,
                        Outcome = TestOutcome,
                    >,
            >,
        Box::new(agent)
            as Box<
                dyn Participant<
                        Observation = TestObservation,
                        Action = TestAction,
                        Outcome = TestOutcome,
                    >,
            >,
    ];
    let mut orchestrator = Orchestrator::from_participants(participants, environment);
    let mut request = test_request();
    request.termination_condition =
        TerminationCondition::AnyOfParticipantsCompletedLoop(vec![ParticipantId::Agent]);
    request.initial_messages.push(InitialInboxMessage {
        to: ParticipantId::Agent,
        from: ParticipantId::User,
        content: "go".to_owned(),
    });

    let result = orchestrator.run(request).await.unwrap();

    assert_eq!(
        result.finish_reason,
        FinishReason::ParticipantCompletedLoop {
            participant: ParticipantId::Agent,
        }
    );
    assert!(result.trajectory.events.iter().any(|event| matches!(
        event,
        TrajectoryEvent::ParticipantLoopCompleted { participant }
            if *participant == ParticipantId::Agent
    )));
}

#[tokio::test]
async fn orchestrator_routes_participant_message_to_other_mailboxes() {
    let seen_agent_inboxes = Arc::new(Mutex::new(Vec::new()));
    let seen_agent_work_items = Arc::new(Mutex::new(Vec::new()));
    let seen_user = Arc::new(Mutex::new(Vec::new()));
    let user = FakeParticipant {
        id: ParticipantId::User,
        decisions: VecDeque::from([
            Ok(ParticipantDecision::Message {
                content: "hi".to_owned(),
            }),
            Ok(ParticipantDecision::Finish),
        ]),
        seen_inboxes: Arc::clone(&seen_user),
        seen_work_items: Arc::new(Mutex::new(Vec::new())),
    };
    let agent = FakeParticipant {
        id: ParticipantId::Agent,
        decisions: VecDeque::from([Ok(ParticipantDecision::Finish)]),
        seen_inboxes: Arc::clone(&seen_agent_inboxes),
        seen_work_items: Arc::clone(&seen_agent_work_items),
    };
    let environment = FakeEnvironment {
        observation: TestObservation("initial"),
        terminal: None,
        immediate_outcomes: VecDeque::new(),
        submitted_events: VecDeque::new(),
        submitted_ids: VecDeque::new(),
    };
    let participants = vec![
        Box::new(user)
            as Box<
                dyn Participant<
                        Observation = TestObservation,
                        Action = TestAction,
                        Outcome = TestOutcome,
                    >,
            >,
        Box::new(agent)
            as Box<
                dyn Participant<
                        Observation = TestObservation,
                        Action = TestAction,
                        Outcome = TestOutcome,
                    >,
            >,
    ];
    let mut orchestrator = Orchestrator::from_participants(participants, environment);
    let mut request = test_request();
    request.initial_messages.push(InitialInboxMessage {
        to: ParticipantId::User,
        from: ParticipantId::Custom("system".to_owned()),
        content: "start".to_owned(),
    });

    let result = orchestrator.run(request).await.unwrap();
    let agent_work_items = seen_agent_work_items.lock().unwrap();

    assert!(result.trajectory.events.iter().any(|event| matches!(
        event,
        TrajectoryEvent::ParticipantMessage { participant, content }
            if *participant == ParticipantId::User && content == "hi"
    )));
    assert!(matches!(
        agent_work_items.as_slice(),
        [Some(WorkItem {
            from: ParticipantId::User,
            content,
            ..
        })] if content == "hi"
    ));
}

#[tokio::test]
async fn orchestrator_persists_action_handoff_message_before_requesting_action() {
    let agent = FakeParticipant {
        id: ParticipantId::Agent,
        decisions: VecDeque::from([
            Ok(ParticipantDecision::Action {
                message: Some("Running a quick check.".to_owned()),
                action: TestAction("run"),
                execution: ActionExecution::Immediate,
            }),
            Ok(ParticipantDecision::Finish),
        ]),
        seen_inboxes: Arc::new(Mutex::new(Vec::new())),
        seen_work_items: Arc::new(Mutex::new(Vec::new())),
    };
    let user = FakeParticipant {
        id: ParticipantId::User,
        decisions: VecDeque::from([Ok(ParticipantDecision::Finish)]),
        seen_inboxes: Arc::new(Mutex::new(Vec::new())),
        seen_work_items: Arc::new(Mutex::new(Vec::new())),
    };
    let environment = FakeEnvironment {
        observation: TestObservation("initial"),
        terminal: None,
        immediate_outcomes: VecDeque::from([Ok(TestOutcome("done"))]),
        submitted_events: VecDeque::new(),
        submitted_ids: VecDeque::new(),
    };
    let participants = vec![
        Box::new(user)
            as Box<
                dyn Participant<
                        Observation = TestObservation,
                        Action = TestAction,
                        Outcome = TestOutcome,
                    >,
            >,
        Box::new(agent)
            as Box<
                dyn Participant<
                        Observation = TestObservation,
                        Action = TestAction,
                        Outcome = TestOutcome,
                    >,
            >,
    ];
    let mut orchestrator = Orchestrator::from_participants(participants, environment);
    let mut request = test_request();
    request.initial_messages.push(InitialInboxMessage {
        to: ParticipantId::Agent,
        from: ParticipantId::User,
        content: "go".to_owned(),
    });
    request.initial_messages.push(InitialInboxMessage {
        to: ParticipantId::User,
        from: ParticipantId::Custom("system".to_owned()),
        content: "done".to_owned(),
    });

    let result = orchestrator.run(request).await.unwrap();

    let handoff_index = result
        .trajectory
        .events
        .iter()
        .position(|event| matches!(
            event,
            TrajectoryEvent::ParticipantMessage { participant, content }
                if *participant == ParticipantId::Agent && content == "Running a quick check."
        ))
        .expect("handoff message should be persisted");
    let action_index = result
        .trajectory
        .events
        .iter()
        .position(|event| matches!(
            event,
            TrajectoryEvent::ActionRequested { participant, action, .. }
                if *participant == ParticipantId::Agent && *action == TestAction("run")
        ))
        .expect("action request should be recorded");

    assert!(handoff_index < action_index);
}

#[tokio::test]
async fn orchestrator_times_out_when_blocked_action_never_completes() {
    let submitted_action_id = ActionId::parse_str("00000000-0000-0000-0000-000000000125").unwrap();
    let agent = FakeParticipant {
        id: ParticipantId::Agent,
        decisions: VecDeque::from([Ok(ParticipantDecision::Action {
            message: None,
            action: TestAction("blocking"),
            execution: ActionExecution::DeferredBlocking,
        })]),
        seen_inboxes: Arc::new(Mutex::new(Vec::new())),
        seen_work_items: Arc::new(Mutex::new(Vec::new())),
    };
    let user = FakeParticipant {
        id: ParticipantId::User,
        decisions: VecDeque::from([Ok(ParticipantDecision::Finish)]),
        seen_inboxes: Arc::new(Mutex::new(Vec::new())),
        seen_work_items: Arc::new(Mutex::new(Vec::new())),
    };
    let environment = FakeEnvironment {
        observation: TestObservation("initial"),
        terminal: None,
        immediate_outcomes: VecDeque::new(),
        submitted_events: VecDeque::new(),
        submitted_ids: VecDeque::from([submitted_action_id]),
    };
    let participants = vec![
        Box::new(user)
            as Box<
                dyn Participant<
                        Observation = TestObservation,
                        Action = TestAction,
                        Outcome = TestOutcome,
                    >,
            >,
        Box::new(agent)
            as Box<
                dyn Participant<
                        Observation = TestObservation,
                        Action = TestAction,
                        Outcome = TestOutcome,
                    >,
            >,
    ];
    let mut orchestrator = Orchestrator::from_participants(participants, environment);
    let mut request = test_request();
    request.limits.max_blocked_action_wait = Some(std::time::Duration::from_millis(20));
    request.initial_messages.push(InitialInboxMessage {
        to: ParticipantId::Agent,
        from: ParticipantId::User,
        content: "go".to_owned(),
    });
    request.initial_messages.push(InitialInboxMessage {
        to: ParticipantId::User,
        from: ParticipantId::Custom("system".to_owned()),
        content: "done".to_owned(),
    });

    let result = orchestrator.run(request).await.unwrap();

    assert_eq!(result.finish_reason, FinishReason::BlockedActionWaitExceeded);
}

#[tokio::test]
async fn completed_user_loop_does_not_restart_while_agent_is_blocked() {
    let submitted_action_id = ActionId::parse_str("00000000-0000-0000-0000-000000000126").unwrap();
    let seen_user_work_items = Arc::new(Mutex::new(Vec::new()));
    let agent = FakeParticipant {
        id: ParticipantId::Agent,
        decisions: VecDeque::from([Ok(ParticipantDecision::Action {
            message: None,
            action: TestAction("blocking"),
            execution: ActionExecution::DeferredBlocking,
        })]),
        seen_inboxes: Arc::new(Mutex::new(Vec::new())),
        seen_work_items: Arc::new(Mutex::new(Vec::new())),
    };
    let user = FakeParticipant {
        id: ParticipantId::User,
        decisions: VecDeque::from([
            Ok(ParticipantDecision::CompleteLoop {
                content: "start planning".to_owned(),
            }),
            Ok(ParticipantDecision::Yield),
        ]),
        seen_inboxes: Arc::new(Mutex::new(Vec::new())),
        seen_work_items: Arc::clone(&seen_user_work_items),
    };
    let environment = FakeEnvironment {
        observation: TestObservation("initial"),
        terminal: None,
        immediate_outcomes: VecDeque::new(),
        submitted_events: VecDeque::new(),
        submitted_ids: VecDeque::from([submitted_action_id]),
    };
    let participants = vec![
        Box::new(user)
            as Box<
                dyn Participant<
                        Observation = TestObservation,
                        Action = TestAction,
                        Outcome = TestOutcome,
                    >,
            >,
        Box::new(agent)
            as Box<
                dyn Participant<
                        Observation = TestObservation,
                        Action = TestAction,
                        Outcome = TestOutcome,
                    >,
            >,
    ];
    let mut orchestrator = Orchestrator::from_participants(participants, environment);
    let mut request = test_request();
    request.limits.max_steps_per_agent_loop = 1;
    request.limits.max_blocked_action_wait = Some(std::time::Duration::from_millis(20));
    request.initial_messages.push(InitialInboxMessage {
        to: ParticipantId::User,
        from: ParticipantId::Custom("system".to_owned()),
        content: "start".to_owned(),
    });

    let result = orchestrator.run(request).await.unwrap();

    assert_eq!(result.finish_reason, FinishReason::BlockedActionWaitExceeded);
    let work_items = seen_user_work_items.lock().unwrap();
    assert_eq!(work_items.len(), 1);
    assert!(matches!(
        work_items.as_slice(),
        [Some(WorkItem {
            from: ParticipantId::Custom(from),
            content,
            ..
        })] if from == "system" && content == "start"
    ));
}

#[tokio::test]
async fn yielded_user_waits_for_new_input_instead_of_spinning() {
    let submitted_action_id = ActionId::parse_str("00000000-0000-0000-0000-000000000127").unwrap();
    let seen_user_work_items = Arc::new(Mutex::new(Vec::new()));
    let user = FakeParticipant {
        id: ParticipantId::User,
        decisions: VecDeque::from([
            Ok(ParticipantDecision::CompleteLoop {
                content: "start planning".to_owned(),
            }),
            Ok(ParticipantDecision::Yield),
        ]),
        seen_inboxes: Arc::new(Mutex::new(Vec::new())),
        seen_work_items: Arc::clone(&seen_user_work_items),
    };
    let agent = FakeParticipant {
        id: ParticipantId::Agent,
        decisions: VecDeque::from([
            Ok(ParticipantDecision::Message {
                content: "Working on it.".to_owned(),
            }),
            Ok(ParticipantDecision::Action {
                message: None,
                action: TestAction("blocking"),
                execution: ActionExecution::DeferredBlocking,
            }),
        ]),
        seen_inboxes: Arc::new(Mutex::new(Vec::new())),
        seen_work_items: Arc::new(Mutex::new(Vec::new())),
    };
    let environment = FakeEnvironment {
        observation: TestObservation("initial"),
        terminal: None,
        immediate_outcomes: VecDeque::new(),
        submitted_events: VecDeque::new(),
        submitted_ids: VecDeque::from([submitted_action_id]),
    };
    let participants = vec![
        Box::new(user)
            as Box<
                dyn Participant<
                        Observation = TestObservation,
                        Action = TestAction,
                        Outcome = TestOutcome,
                    >,
            >,
        Box::new(agent)
            as Box<
                dyn Participant<
                        Observation = TestObservation,
                        Action = TestAction,
                        Outcome = TestOutcome,
                    >,
            >,
    ];
    let mut orchestrator = Orchestrator::from_participants(participants, environment);
    let mut request = test_request();
    request.limits.max_blocked_action_wait = Some(std::time::Duration::from_millis(20));
    request.initial_messages.push(InitialInboxMessage {
        to: ParticipantId::User,
        from: ParticipantId::Custom("system".to_owned()),
        content: "start".to_owned(),
    });

    let result = orchestrator.run(request).await.unwrap();

    assert_eq!(result.finish_reason, FinishReason::BlockedActionWaitExceeded);
    let work_items = seen_user_work_items.lock().unwrap();
    assert_eq!(work_items.len(), 2);
    assert!(matches!(
        work_items.as_slice(),
        [
            Some(WorkItem {
                from: ParticipantId::Custom(first_from),
                content: first_content,
                ..
            }),
            Some(WorkItem {
                from: ParticipantId::Agent,
                content: second_content,
                ..
            })
        ] if first_from == "system"
            && first_content == "start"
            && second_content == "Working on it."
    ));
}
