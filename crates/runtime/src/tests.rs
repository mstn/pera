use async_trait::async_trait;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::{
    ActionExecutionUpdate, ActionExecutor, ActionProcessorError, EventHub, ExecutionEngine,
    FileSystemEventLog, FileSystemRunStore, InMemoryRunStore, RecordingEventPublisher, RunExecutor,
    RunTransitionTrigger, TeeEventPublisher,
};
use pera_canonical::{CatalogSkill, SkillCatalog, SkillMetadata};
use pera_core::{
    ActionName, ActionResult, CanonicalValue, CodeArtifact, CodeArtifactId, CodeLanguage,
    CompiledProgram, EventPublisher, ExecutionEvent, ExecutionOutput, ExecutionSnapshot,
    ExecutionStatus, ExternalCall, InputValues, Interpreter, InterpreterError, InterpreterKind,
    InterpreterStep, RunStore, ScriptName, StartExecutionRequest, Suspension, Value,
};

#[derive(Debug, Default, Clone, Copy)]
struct FakeInterpreter;

impl Interpreter for FakeInterpreter {
    fn kind(&self) -> InterpreterKind {
        InterpreterKind::Monty
    }

    fn compile(&self, code: &CodeArtifact) -> Result<CompiledProgram, InterpreterError> {
        Ok(CompiledProgram {
            kind: InterpreterKind::Monty,
            input_order: code.inputs.clone(),
            bytes: code.source.as_bytes().to_vec(),
        })
    }

    fn start(
        &self,
        program: &CompiledProgram,
        _inputs: &InputValues,
    ) -> Result<InterpreterStep, InterpreterError> {
        let source = std::str::from_utf8(&program.bytes)
            .map_err(|error| InterpreterError::new(error.to_string()))?;

        if let Some(value) = source.strip_prefix("complete:") {
            return Ok(InterpreterStep::Completed(ExecutionOutput {
                value: Some(Value::Int(value.parse().unwrap())),
            }));
        }

        if let Some((action_name, argument)) = parse_call(source) {
            return Ok(InterpreterStep::Suspended(Suspension {
                snapshot: ExecutionSnapshot {
                    kind: InterpreterKind::Monty,
                    bytes: b"resume".to_vec(),
                },
                call: ExternalCall {
                    action_name: ActionName::new(action_name),
                    positional_arguments: vec![Value::Int(argument)],
                    named_arguments: BTreeMap::new(),
                },
            }));
        }

        Err(InterpreterError::new("unknown fake program"))
    }

    fn resume(
        &self,
        _snapshot: &ExecutionSnapshot,
        return_value: &Value,
    ) -> Result<InterpreterStep, InterpreterError> {
        Ok(InterpreterStep::Completed(ExecutionOutput {
            value: Some(return_value.clone()),
        }))
    }
}

#[derive(Debug, Default, Clone, Copy)]
struct EchoActionExecutor;

#[async_trait]
impl ActionExecutor for EchoActionExecutor {
    async fn execute(&self, action: pera_core::ActionRequest) -> ActionExecutionUpdate {
        let result = match action.invocation.arguments.get("value") {
            Some(CanonicalValue::S64(value)) => Ok(CanonicalValue::S64(*value)),
            Some(CanonicalValue::S32(value)) => Ok(CanonicalValue::S32(*value)),
            Some(CanonicalValue::U64(value)) => i64::try_from(*value)
                .map(CanonicalValue::S64)
                .map_err(|_| ActionProcessorError::new("canonical u64 does not fit in model int")),
            Some(CanonicalValue::U32(value)) => Ok(CanonicalValue::S64((*value).into())),
            Some(CanonicalValue::Null) | None => Ok(CanonicalValue::Null),
            Some(other) => Err(ActionProcessorError::new(format!(
                "unsupported canonical echo argument: {other:?}"
            ))),
        };

        match result {
            Ok(value) => ActionExecutionUpdate::Completed {
                result: ActionResult {
                    action_id: action.id,
                    value,
                },
                diagnostics: None,
            },
            Err(error) => ActionExecutionUpdate::Failed {
                run_id: action.run_id,
                action_id: action.id,
                skill_name: action.skill.skill_name.clone(),
                action_name: action.invocation.action_name.as_str().to_owned(),
                message: error.to_string(),
                diagnostics: None,
            },
        }
    }
}

#[derive(Debug, Default, Clone, Copy)]
struct RejectingActionExecutor;

#[async_trait]
impl ActionExecutor for RejectingActionExecutor {
    async fn execute(&self, action: pera_core::ActionRequest) -> ActionExecutionUpdate {
        ActionExecutionUpdate::Failed {
            run_id: action.run_id,
            action_id: action.id,
            skill_name: action.skill.skill_name.clone(),
            action_name: action.invocation.action_name.as_str().to_owned(),
            message: format!(
                "no action processor is configured for '{}'",
                action.invocation.action_name.as_str()
            ),
            diagnostics: None,
        }
    }
}

fn parse_call(source: &str) -> Option<(&str, i64)> {
    let mut parts = source.split(':');
    let kind = parts.next()?;
    if kind != "call" {
        return None;
    }

    let action_name = parts.next()?;
    let argument = parts.next()?.parse().ok()?;
    Some((action_name, argument))
}

fn request(source: &str) -> StartExecutionRequest {
    StartExecutionRequest {
        code: CodeArtifact {
            id: code_id("00000000-0000-0000-0000-000000000000"),
            language: CodeLanguage::Python,
            script_name: ScriptName::new("test.py"),
            source: source.to_owned(),
            inputs: Vec::new(),
        },
        inputs: InputValues::new(),
    }
}

async fn wait_for_terminal_events(
    subscription: &mut crate::EventSubscription,
    run_ids: &[pera_core::RunId],
) -> Vec<ExecutionEvent> {
    let mut events = Vec::new();
    let mut completed = std::collections::BTreeSet::new();

    while completed.len() < run_ids.len() {
        let event = subscription.recv().await.unwrap();
        if run_ids.contains(&event.run_id()) {
            if matches!(
                event,
                ExecutionEvent::RunCompleted { .. } | ExecutionEvent::RunFailed { .. }
            ) {
                completed.insert(event.run_id());
            }
            events.push(event);
        }
    }

    events
}

#[test]
fn run_executor_completes_without_external_calls() {
    let executor = RunExecutor::new(FakeInterpreter);

    let transition = executor
        .start_run(
            request("complete:7"),
            run_id("00000000-0000-0000-0000-000000000001"),
            code_id("00000000-0000-0000-0000-000000000001"),
            || action_id("00000000-0000-0000-0000-000000000001"),
        )
        .unwrap();
    let session = transition.session.clone();

    assert_eq!(transition.trigger, RunTransitionTrigger::Started);
    assert!(transition.action_records.is_empty());
    assert!(transition.action_to_enqueue.is_none());
    assert_eq!(
        session.status,
        ExecutionStatus::Completed(ExecutionOutput {
            value: Some(Value::Int(7))
        })
    );
}

#[test]
fn run_executor_suspends_and_resumes() {
    let executor = RunExecutor::with_skill_catalog(FakeInterpreter, single_action_catalog("echo"));

    let transition = executor
        .start_run(
            request("call:echo:9"),
            run_id("00000000-0000-0000-0000-000000000001"),
            code_id("00000000-0000-0000-0000-000000000001"),
            || action_id("00000000-0000-0000-0000-000000000001"),
        )
        .unwrap();
    let session = transition.session.clone();
    let action_id = match session.status {
        ExecutionStatus::WaitingForAction(action_id) => action_id,
        status => panic!("unexpected status: {status:?}"),
    };
    let action = transition.action_to_enqueue.clone().unwrap();
    assert_eq!(action.id, action_id);
    assert_eq!(
        action.invocation.arguments,
        BTreeMap::from([("value".to_owned(), CanonicalValue::S64(9))])
    );
    assert_eq!(transition.trigger, RunTransitionTrigger::Started);

    let resumed = executor
        .complete_action(
            session,
            action,
            ActionResult {
                action_id,
                value: CanonicalValue::S64(11),
            },
            None,
            || next_action_id("00000000-0000-0000-0000-000000000002"),
        )
        .unwrap();

    assert_eq!(
        resumed.session.status,
        ExecutionStatus::Completed(ExecutionOutput {
            value: Some(Value::Int(11))
        })
    );
    assert_eq!(
        resumed.trigger,
        RunTransitionTrigger::Resumed {
            completed_action_id: action_id,
        }
    );
}

#[test]
fn run_executor_annotates_actions_from_skill_catalog() {
    let executor = RunExecutor::with_skill_catalog(
        FakeInterpreter,
        single_action_catalog_for_skill_with_profile(
            "secret-service",
            "secret-service-default",
            "resolve-mission",
        ),
    );

    let transition = executor
        .start_run(
            request("call:resolve_mission:9"),
            run_id("00000000-0000-0000-0000-000000000001"),
            code_id("00000000-0000-0000-0000-000000000001"),
            || action_id("00000000-0000-0000-0000-000000000001"),
        )
        .unwrap();

    let action = transition.action_to_enqueue.unwrap();
    assert_eq!(action.invocation.action_name.as_str(), "resolve-mission");
    assert_eq!(action.skill.skill_name.as_str(), "secret-service");
    assert_eq!(
        action.skill.profile_name.as_deref(),
        Some("secret-service-default")
    );
}

#[test]
fn run_executor_rejects_unknown_actions_when_catalog_is_configured() {
    let executor = RunExecutor::with_skill_catalog(FakeInterpreter, secret_service_catalog());

    let error = executor
        .start_run(
            request("call:missing:9"),
            run_id("00000000-0000-0000-0000-000000000001"),
            code_id("00000000-0000-0000-0000-000000000001"),
            || action_id("00000000-0000-0000-0000-000000000001"),
        )
        .unwrap_err();

    assert!(
        error
            .to_string()
            .contains("unknown external action 'missing'")
    );
}

#[test]
fn run_executor_fails_run() {
    let executor = RunExecutor::with_skill_catalog(FakeInterpreter, single_action_catalog("echo"));

    let started = executor
        .start_run(
            request("call:echo:3"),
            run_id("00000000-0000-0000-0000-000000000001"),
            code_id("00000000-0000-0000-0000-000000000001"),
            || action_id("00000000-0000-0000-0000-000000000001"),
        )
        .unwrap();
    let failed = executor.fail_run(started.session.clone(), "boom");

    assert_eq!(
        failed.session.status,
        ExecutionStatus::Failed("boom".to_owned())
    );
    assert_eq!(started.trigger, RunTransitionTrigger::Started);
    assert_eq!(failed.trigger, RunTransitionTrigger::Failed);
}

#[tokio::test]
async fn execution_engine_manages_multiple_runs() {
    let event_hub = EventHub::new();
    let publisher = TeeEventPublisher::new(RecordingEventPublisher::new(), event_hub.publisher());
    let run_executor =
        RunExecutor::with_skill_catalog(FakeInterpreter, single_action_catalog("echo"));
    let action_executor = EchoActionExecutor;
    let engine = ExecutionEngine::new(
        run_executor,
        InMemoryRunStore::new(),
        publisher,
        action_executor,
        event_hub,
    );
    let mut subscription = engine.subscribe();

    let run_a = engine.submit(request("call:echo:1")).await.unwrap();
    let run_b = engine.submit(request("call:echo:2")).await.unwrap();
    let events = wait_for_terminal_events(&mut subscription, &[run_a, run_b]).await;

    assert_eq!(
        engine.run_status(run_a),
        Some(ExecutionStatus::Completed(ExecutionOutput {
            value: Some(Value::Int(1)),
        }))
    );
    assert_eq!(
        engine.run_status(run_b),
        Some(ExecutionStatus::Completed(ExecutionOutput {
            value: Some(Value::Int(2)),
        }))
    );

    let run_a_events: Vec<_> = events
        .iter()
        .filter(|event| event.run_id() == run_a)
        .cloned()
        .collect();
    let run_b_events: Vec<_> = events
        .iter()
        .filter(|event| event.run_id() == run_b)
        .cloned()
        .collect();

    assert!(run_a_events.iter().any(
        |event| matches!(event, ExecutionEvent::ActionClaimed { run_id, .. } if *run_id == run_a)
    ));
    assert!(run_b_events.iter().any(
        |event| matches!(event, ExecutionEvent::ActionClaimed { run_id, .. } if *run_id == run_b)
    ));
}

#[tokio::test]
async fn execution_engine_emits_action_failure_and_run_failure() {
    let event_hub = EventHub::new();
    let publisher = TeeEventPublisher::new(RecordingEventPublisher::new(), event_hub.publisher());
    let run_executor =
        RunExecutor::with_skill_catalog(FakeInterpreter, single_action_catalog("missing"));
    let action_executor = RejectingActionExecutor;
    let engine = ExecutionEngine::new(
        run_executor,
        InMemoryRunStore::new(),
        publisher,
        action_executor,
        event_hub,
    );
    let mut subscription = engine.subscribe();

    let run_id = engine.submit(request("call:missing:5")).await.unwrap();
    let events = wait_for_terminal_events(&mut subscription, &[run_id]).await;

    assert_eq!(
        engine.run_status(run_id),
        Some(ExecutionStatus::Failed(
            "no action processor is configured for 'missing'".to_owned(),
        ))
    );

    assert!(events.iter().any(|event| matches!(
        event,
        ExecutionEvent::ActionFailed {
            run_id: event_run_id,
            action_id: _,
            skill_name,
            action_name,
            message,
        } if *event_run_id == run_id
            && skill_name == "test-skill"
            && action_name == "missing"
            && message.contains("no action processor")
    )));
    assert!(events.contains(&ExecutionEvent::RunFailed {
        run_id,
        message: "no action processor is configured for 'missing'".to_owned(),
    }));
}

#[tokio::test]
async fn event_hub_supports_multiple_subscribers() {
    let event_hub = EventHub::new();
    let mut publisher = event_hub.publisher();
    let mut subscription_a = event_hub.subscribe();
    let mut subscription_b = event_hub.subscribe();
    let event = ExecutionEvent::RunSubmitted {
        run_id: run_id("00000000-0000-0000-0000-000000000042"),
    };

    pera_core::EventPublisher::publish(&mut publisher, event.clone()).unwrap();

    assert_eq!(subscription_a.recv().await.unwrap(), event.clone());
    assert_eq!(subscription_b.recv().await.unwrap(), event);
}

#[tokio::test]
async fn execution_engine_recovers_waiting_runs_from_event_log() {
    let root = temp_root("recovery");
    let mut store = FileSystemRunStore::new(&root).unwrap();
    let mut event_log = FileSystemEventLog::new(&root).unwrap();
    let executor = RunExecutor::with_skill_catalog(FakeInterpreter, single_action_catalog("echo"));
    let transition = executor
        .start_run(
            request("call:echo:41"),
            run_id("00000000-0000-0000-0000-000000000001"),
            code_id("00000000-0000-0000-0000-000000000001"),
            || action_id("00000000-0000-0000-0000-000000000001"),
        )
        .unwrap();

    store.save_run(transition.session.clone()).unwrap();
    for action_record in &transition.action_records {
        store.save_action(action_record.clone()).unwrap();
    }

    EventPublisher::publish(
        &mut event_log,
        ExecutionEvent::RunSubmitted {
            run_id: transition.session.id,
        },
    )
    .unwrap();
    EventPublisher::publish(
        &mut event_log,
        ExecutionEvent::RunStarted {
            run_id: transition.session.id,
        },
    )
    .unwrap();
    EventPublisher::publish(
        &mut event_log,
        ExecutionEvent::ActionEnqueued {
            run_id: transition.session.id,
            action_id: transition.action_to_enqueue.as_ref().unwrap().id,
            skill_name: transition
                .action_to_enqueue
                .as_ref()
                .unwrap()
                .skill
                .skill_name
                .clone(),
            action_name: transition
                .action_to_enqueue
                .as_ref()
                .unwrap()
                .invocation
                .action_name
                .as_str()
                .to_owned(),
        },
    )
    .unwrap();

    let recovered_events = event_log.read_events().unwrap();
    let event_hub = EventHub::new();
    let publisher = TeeEventPublisher::new(event_log, event_hub.publisher());
    let engine = ExecutionEngine::new(
        RunExecutor::with_skill_catalog(FakeInterpreter, single_action_catalog("echo")),
        FileSystemRunStore::new(&root).unwrap(),
        publisher,
        EchoActionExecutor,
        event_hub,
    );
    let mut subscription = engine.subscribe();
    engine.recover_from_events(recovered_events).await.unwrap();
    let recovered_run_id = run_id("00000000-0000-0000-0000-000000000001");
    let recovered_action_id = action_id("00000000-0000-0000-0000-000000000001");
    let events = wait_for_terminal_events(&mut subscription, &[recovered_run_id]).await;

    assert!(events.iter().any(|event| matches!(
        event,
        ExecutionEvent::ActionClaimed {
            run_id,
            action_id,
            ..
        } if *run_id == recovered_run_id && *action_id == recovered_action_id
    )));
    assert!(events.contains(&ExecutionEvent::RunCompleted {
        run_id: recovered_run_id,
        value: Some(Value::Int(41)),
    }));

    let _ = std::fs::remove_dir_all(root);
}

fn temp_root(prefix: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!("pera-{prefix}-{nanos}"))
}

fn run_id(value: &str) -> pera_core::RunId {
    pera_core::RunId::parse_str(value).unwrap()
}

fn action_id(value: &str) -> pera_core::ActionId {
    pera_core::ActionId::parse_str(value).unwrap()
}

fn next_action_id(value: &str) -> pera_core::ActionId {
    action_id(value)
}

fn code_id(value: &str) -> CodeArtifactId {
    CodeArtifactId::parse_str(value).unwrap()
}

fn secret_service_catalog() -> SkillCatalog {
    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let world_path = manifest_dir.join("../../skills/examples/secret-service/world.wit");
    let world =
        pera_canonical::load_canonical_world_from_wit(&world_path, "secret-service-default")
            .unwrap();

    SkillCatalog::from_skill(CatalogSkill {
        metadata: {
            let mut metadata = SkillMetadata::new("secret-service", "secret-service-default");
            metadata.profile_name = Some("secret-service-default".to_owned());
            metadata
        },
        world,
        capabilities: vec!["sqlite".to_owned()],
        databases: Vec::new(),
    })
    .unwrap()
}

fn single_action_catalog(action_name: &str) -> SkillCatalog {
    single_action_catalog_for_skill("test-skill", action_name)
}

fn single_action_catalog_for_skill(skill_name: &str, action_name: &str) -> SkillCatalog {
    single_action_catalog_for_skill_with_profile(skill_name, "test-profile", action_name)
}

fn single_action_catalog_for_skill_with_profile(
    skill_name: &str,
    profile_name: &str,
    action_name: &str,
) -> SkillCatalog {
    SkillCatalog::from_skill(CatalogSkill {
        metadata: {
            let mut metadata = SkillMetadata::new(skill_name, "test-world");
            metadata.profile_name = Some(profile_name.to_owned());
            metadata
        },
        world: single_action_world(skill_name, "test-world", action_name),
        capabilities: Vec::new(),
        databases: Vec::new(),
    })
    .unwrap()
}

fn single_action_world(
    skill_name: &str,
    world_name: &str,
    action_name: &str,
) -> pera_canonical::CanonicalWorld {
    pera_canonical::CanonicalWorld {
        package: Some(pera_canonical::CanonicalPackageRef {
            namespace: "tests".to_owned(),
            name: skill_name.to_owned(),
            version: None,
        }),
        name: world_name.to_owned(),
        docs: None,
        imports: Vec::new(),
        exports: vec![pera_canonical::CanonicalInterface {
            name: format!("{skill_name}-exports"),
            docs: None,
            functions: vec![pera_canonical::CanonicalFunction {
                name: action_name.to_owned(),
                docs: None,
                params: vec![pera_canonical::CanonicalParam {
                    name: "value".to_owned(),
                    ty: pera_canonical::CanonicalTypeRef::Primitive(
                        pera_canonical::CanonicalPrimitiveType::S64,
                    ),
                }],
                result: pera_canonical::CanonicalFunctionResult::Scalar(
                    pera_canonical::CanonicalTypeRef::Primitive(
                        pera_canonical::CanonicalPrimitiveType::S64,
                    ),
                ),
            }],
            types: Vec::new(),
        }],
    }
}
