use std::collections::BTreeMap;

use chrono::{Datelike, Local, Timelike, Utc};
use monty::{
    MontyDate, MontyDateTime, MontyObject, MontyRepl, NoLimitTracker, OsFunction, PrintWriter,
    ReplFunctionCall, ReplOsCall, ReplProgress,
};
use pera_core::{
    ActionName, CodeArtifact, CodeLanguage, CompiledProgram, ExecutionOutput, ExecutionSnapshot,
    ExternalCall, InputValues, Interpreter, InterpreterError, InterpreterKind, InterpreterStep,
    Suspension, Value,
};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct MontySnippetProgram {
    script_name: String,
    source: String,
}

#[derive(Debug, Default, Clone, Copy)]
pub struct MontyInterpreter;

impl MontyInterpreter {
    pub fn new() -> Self {
        Self
    }
}

impl Interpreter for MontyInterpreter {
    fn kind(&self) -> InterpreterKind {
        InterpreterKind::Monty
    }

    fn compile(&self, code: &CodeArtifact) -> Result<CompiledProgram, InterpreterError> {
        if code.language != CodeLanguage::Python {
            return Err(InterpreterError::new("Monty only supports Python code"));
        }

        let program = MontySnippetProgram {
            script_name: code.script_name.as_str().to_owned(),
            source: code.source.clone(),
        };
        let bytes = serde_json::to_vec(&program).map_err(to_interpreter_error)?;

        Ok(CompiledProgram {
            kind: InterpreterKind::Monty,
            input_order: code.inputs.clone(),
            bytes,
        })
    }

    fn start(
        &self,
        program: &CompiledProgram,
        inputs: &InputValues,
        repl_state: Option<&ExecutionSnapshot>,
    ) -> Result<InterpreterStep, InterpreterError> {
        let snippet: MontySnippetProgram =
            serde_json::from_slice(&program.bytes).map_err(to_interpreter_error)?;
        let repl = match repl_state {
            Some(snapshot) => MontyRepl::load(&snapshot.bytes).map_err(to_interpreter_error)?,
            None => MontyRepl::new(&snippet.script_name, NoLimitTracker),
        };
        let ordered_inputs = input_values_to_monty_objects(&program.input_order, inputs)?;
        let named_inputs = program
            .input_order
            .iter()
            .zip(ordered_inputs)
            .map(|(name, value)| (name.as_str().to_owned(), value))
            .collect();
        let progress = repl
            .feed_start(&snippet.source, named_inputs, PrintWriter::Disabled)
            .map_err(|error| to_interpreter_error(error.error))?;
        repl_progress_to_step(progress)
    }

    fn resume(
        &self,
        snapshot: &ExecutionSnapshot,
        return_value: &Value,
    ) -> Result<InterpreterStep, InterpreterError> {
        let progress =
            ReplProgress::<NoLimitTracker>::load(&snapshot.bytes).map_err(to_interpreter_error)?;
        let progress = progress
            .into_function_call()
            .ok_or_else(|| InterpreterError::new("snapshot is not a function call suspension"))?
            .resume(value_to_monty_object(return_value)?, PrintWriter::Disabled)
            .map_err(|error| to_interpreter_error(error.error))?;
        repl_progress_to_step(progress)
    }
}

fn repl_progress_to_step(
    progress: ReplProgress<NoLimitTracker>,
) -> Result<InterpreterStep, InterpreterError> {
    let mut progress = progress;

    loop {
        match progress {
            ReplProgress::FunctionCall(call) => return function_call_to_step(call),
            ReplProgress::OsCall(call) => {
                progress = resume_os_call(call)?;
            }
            ReplProgress::Complete { repl, value } => {
                let value = match value {
                    MontyObject::None => None,
                    other => Some(monty_object_to_value(other)?),
                };
                return Ok(InterpreterStep::Completed(ExecutionOutput {
                    value,
                    repl_state: Some(ExecutionSnapshot {
                        kind: InterpreterKind::Monty,
                        bytes: repl.dump().map_err(to_interpreter_error)?,
                    }),
                }));
            }
            ReplProgress::ResolveFutures(_) => {
                return Err(InterpreterError::new(
                    "Monty returned unsupported suspension kind: resolve_futures",
                ));
            }
            ReplProgress::NameLookup(lookup) => {
                return Err(InterpreterError::new(format!(
                    "Monty returned unsupported suspension kind: name_lookup for '{}'",
                    lookup.name
                )));
            }
        }
    }
}

fn resume_os_call(
    call: ReplOsCall<NoLimitTracker>,
) -> Result<ReplProgress<NoLimitTracker>, InterpreterError> {
    let result = match call.function {
        OsFunction::DateToday => {
            let today = Local::now().date_naive();
            MontyObject::Date(MontyDate {
                year: today.year(),
                month: today.month() as u8,
                day: today.day() as u8,
            })
        }
        OsFunction::DateTimeNow => monty_datetime_now(&call.args)?,
        other => {
            return Err(InterpreterError::new(format!(
                "Monty OS call '{}' is not yet supported",
                other
            )));
        }
    };

    call.resume(result, PrintWriter::Disabled)
        .map_err(|error| to_interpreter_error(error.error))
}

fn monty_datetime_now(args: &[MontyObject]) -> Result<MontyObject, InterpreterError> {
    let timezone = args.first().cloned().unwrap_or(MontyObject::None);

    match timezone {
        MontyObject::None => {
            let now = Local::now().naive_local();
            Ok(MontyObject::DateTime(MontyDateTime {
                year: now.year(),
                month: now.month() as u8,
                day: now.day() as u8,
                hour: now.hour() as u8,
                minute: now.minute() as u8,
                second: now.second() as u8,
                microsecond: now.and_utc().timestamp_subsec_micros(),
                offset_seconds: None,
                timezone_name: None,
            }))
        }
        MontyObject::TimeZone(tz) => {
            let offset = chrono::FixedOffset::east_opt(tz.offset_seconds).ok_or_else(|| {
                InterpreterError::new(format!(
                    "Monty datetime.now received invalid timezone offset: {}",
                    tz.offset_seconds
                ))
            })?;
            let now = Utc::now().with_timezone(&offset);
            Ok(MontyObject::DateTime(MontyDateTime {
                year: now.year(),
                month: now.month() as u8,
                day: now.day() as u8,
                hour: now.hour() as u8,
                minute: now.minute() as u8,
                second: now.second() as u8,
                microsecond: now.timestamp_subsec_micros(),
                offset_seconds: Some(tz.offset_seconds),
                timezone_name: tz.name,
            }))
        }
        other => Err(InterpreterError::new(format!(
            "Monty datetime.now expected None or TimeZone, received {:?}",
            other
        ))),
    }
}

fn function_call_to_step(
    call: ReplFunctionCall<NoLimitTracker>,
) -> Result<InterpreterStep, InterpreterError> {
    let function_name = call.function_name.clone();
    let positional_arguments = call
        .args
        .clone()
        .into_iter()
        .map(monty_object_to_value)
        .collect::<Result<Vec<_>, _>>()?;
    let named_arguments = call
        .kwargs
        .clone()
        .into_iter()
        .map(|(key, value)| {
            let key = match key {
                MontyObject::String(value) => value,
                other => {
                    return Err(InterpreterError::new(format!(
                        "external call '{}' has a non-string keyword argument key: {other:?}",
                        function_name
                    )));
                }
            };
            Ok((key, monty_object_to_value(value)?))
        })
        .collect::<Result<std::collections::BTreeMap<_, _>, _>>()?;
    let snapshot = ReplProgress::FunctionCall(call)
        .dump()
        .map_err(to_interpreter_error)?;

    Ok(InterpreterStep::Suspended(Suspension {
        snapshot: ExecutionSnapshot {
            kind: InterpreterKind::Monty,
            bytes: snapshot,
        },
        call: ExternalCall {
            action_name: ActionName::new(function_name),
            positional_arguments,
            named_arguments,
        },
    }))
}

fn input_values_to_monty_objects(
    input_order: &[pera_core::InputName],
    values: &InputValues,
) -> Result<Vec<MontyObject>, InterpreterError> {
    input_order
        .iter()
        .map(|name| {
            let value = values.get(name).ok_or_else(|| {
                InterpreterError::new(format!("missing input value for '{}'", name.as_str()))
            })?;
            value_to_monty_object(value)
        })
        .collect()
}

fn value_to_monty_object(value: &Value) -> Result<MontyObject, InterpreterError> {
    match value {
        Value::Null => Ok(MontyObject::None),
        Value::Bool(value) => Ok(MontyObject::Bool(*value)),
        Value::Int(value) => Ok(MontyObject::Int(*value)),
        Value::String(value) => Ok(MontyObject::String(value.clone())),
        Value::List(items) => items
            .iter()
            .map(value_to_monty_object)
            .collect::<Result<Vec<_>, _>>()
            .map(MontyObject::List),
        Value::Map(entries) => entries
            .iter()
            .map(|(key, value)| {
                Ok((
                    MontyObject::String(key.clone()),
                    value_to_monty_object(value)?,
                ))
            })
            .collect::<Result<Vec<_>, _>>()
            .map(MontyObject::dict),
        Value::Record { name, fields } => {
            let attrs = fields
                .iter()
                .map(|(key, value)| {
                    Ok((
                        MontyObject::String(key.clone()),
                        value_to_monty_object(value)?,
                    ))
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok(MontyObject::Dataclass {
                name: name.clone(),
                type_id: 0,
                field_names: fields.keys().cloned().collect(),
                attrs: attrs.into(),
                frozen: true,
            })
        }
    }
}

fn monty_object_to_value(value: MontyObject) -> Result<Value, InterpreterError> {
    match value {
        MontyObject::None => Ok(Value::Null),
        MontyObject::Bool(value) => Ok(Value::Bool(value)),
        MontyObject::Int(value) => Ok(Value::Int(value)),
        MontyObject::String(value) => Ok(Value::String(value)),
        MontyObject::List(items) => items
            .into_iter()
            .map(monty_object_to_value)
            .collect::<Result<Vec<_>, _>>()
            .map(Value::List),
        MontyObject::Tuple(items) => items
            .into_iter()
            .map(monty_object_to_value)
            .collect::<Result<Vec<_>, _>>()
            .map(Value::List),
        MontyObject::NamedTuple {
            type_name,
            field_names,
            values,
        } => {
            if field_names.len() != values.len() {
                return Err(InterpreterError::new(
                    "Monty namedtuple field_names and values must have the same length",
                ));
            }

            let mut map = BTreeMap::new();
            for (field_name, value) in field_names.into_iter().zip(values.into_iter()) {
                map.insert(field_name, monty_object_to_value(value)?);
            }

            Ok(Value::Record {
                name: type_name,
                fields: map,
            })
        }
        MontyObject::Dict(entries) => {
            let mut map = BTreeMap::new();

            for (key, value) in entries {
                let key = match key {
                    MontyObject::String(value) => value,
                    _ => {
                        return Err(InterpreterError::new(
                            "Monty dictionaries must use string keys",
                        ));
                    }
                };

                map.insert(key, monty_object_to_value(value)?);
            }

            Ok(Value::Map(map))
        }
        MontyObject::Dataclass { name, attrs, .. } => {
            let mut map = BTreeMap::new();

            for (key, value) in attrs {
                let key = match key {
                    MontyObject::String(value) => value,
                    _ => {
                        return Err(InterpreterError::new(
                            "Monty dataclass attributes must use string keys",
                        ));
                    }
                };

                map.insert(key, monty_object_to_value(value)?);
            }

            Ok(Value::Record { name, fields: map })
        }
        _ => Err(InterpreterError::new(
            "Monty value cannot yet be normalized into a Pera value",
        )),
    }
}

fn to_interpreter_error(error: impl std::fmt::Display) -> InterpreterError {
    InterpreterError::new(error.to_string())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use pera_core::{CodeArtifactId, ScriptName, Value};

    fn run_python(source: &str) -> Result<InterpreterStep, InterpreterError> {
        let interpreter = MontyInterpreter::new();
        let program = interpreter.compile(&CodeArtifact {
            id: CodeArtifactId::generate(),
            language: CodeLanguage::Python,
            script_name: ScriptName::new("test.py"),
            source: source.to_owned(),
            inputs: Vec::new(),
        })?;
        interpreter.start(&program, &InputValues::new(), None)
    }

    fn run_python_with_repl_state(
        source: &str,
        repl_state: &ExecutionSnapshot,
    ) -> Result<InterpreterStep, InterpreterError> {
        let interpreter = MontyInterpreter::new();
        let program = interpreter.compile(&CodeArtifact {
            id: CodeArtifactId::generate(),
            language: CodeLanguage::Python,
            script_name: ScriptName::new("test.py"),
            source: source.to_owned(),
            inputs: Vec::new(),
        })?;
        interpreter.start(&program, &InputValues::new(), Some(repl_state))
    }

    fn completed_output(step: InterpreterStep) -> ExecutionOutput {
        match step {
            InterpreterStep::Completed(output) => output,
            other => panic!("expected completed step, got {other:?}"),
        }
    }

    fn persisted_repl_state(output: &ExecutionOutput) -> ExecutionSnapshot {
        output
            .repl_state
            .clone()
            .expect("completed snippet should persist repl state")
    }

    #[test]
    fn supports_date_today_os_call() {
        let step = run_python(
            r#"
from datetime import date
date.today().isoformat()
"#,
        )
        .expect("date.today should execute successfully");

        match step {
            InterpreterStep::Completed(output) => match output.value {
                Some(Value::String(value)) => assert_eq!(value.len(), 10),
                other => panic!("unexpected output value: {other:?}"),
            },
            other => panic!("unexpected interpreter step: {other:?}"),
        }
    }

    #[test]
    fn supports_datetime_now_os_call() {
        let step = run_python(
            r#"
from datetime import datetime
datetime.now().isoformat()
"#,
        )
        .expect("datetime.now should execute successfully");

        match step {
            InterpreterStep::Completed(output) => match output.value {
                Some(Value::String(value)) => assert!(value.contains('T')),
                other => panic!("unexpected output value: {other:?}"),
            },
            other => panic!("unexpected interpreter step: {other:?}"),
        }
    }

    #[test]
    fn statement_only_snippet_has_no_display_value() {
        let step = run_python(
            r#"
x = 1
"#,
        )
        .expect("statement-only snippet should execute successfully");

        match step {
            InterpreterStep::Completed(output) => {
                assert_eq!(output.value, None);
                assert!(output.repl_state.is_some());
            }
            other => panic!("unexpected interpreter step: {other:?}"),
        }
    }

    #[test]
    fn expression_snippet_keeps_display_value() {
        let step = run_python(
            r#"
1 + 2
"#,
        )
        .expect("expression snippet should execute successfully");

        match step {
            InterpreterStep::Completed(output) => {
                assert_eq!(output.value, Some(Value::Int(3)));
                assert!(output.repl_state.is_some());
            }
            other => panic!("unexpected interpreter step: {other:?}"),
        }
    }

    #[test]
    fn unresolved_function_call_suspends_and_resumes() {
        let step = run_python(
            r#"
ext(41)
"#,
        )
        .expect("unresolved call should suspend");

        let suspension = match step {
            InterpreterStep::Suspended(suspension) => suspension,
            other => panic!("expected suspension, got {other:?}"),
        };
        assert_eq!(suspension.call.action_name.as_str(), "ext");
        assert_eq!(suspension.call.positional_arguments, vec![Value::Int(41)]);

        let interpreter = MontyInterpreter::new();
        let resumed = interpreter
            .resume(&suspension.snapshot, &Value::Int(99))
            .expect("resuming suspended call should succeed");

        match resumed {
            InterpreterStep::Completed(output) => {
                assert_eq!(output.value, Some(Value::Int(99)));
                assert!(output.repl_state.is_some());
            }
            other => panic!("unexpected interpreter step after resume: {other:?}"),
        }
    }

    #[test]
    fn persists_variables_across_turns() {
        let first = completed_output(
            run_python(
            r#"
meetings = [1, 2, 3]
"#,
        )
        .expect("first snippet should succeed"),
        );

        let second = completed_output(
            run_python_with_repl_state(
            r#"
meetings[0]
"#,
            &persisted_repl_state(&first),
        )
        .expect("second snippet should reuse prior variables"),
        );

        assert_eq!(second.value, Some(Value::Int(1)));
        assert!(second.repl_state.is_some());
    }

    #[test]
    fn persists_function_definitions_across_turns() {
        let first = completed_output(
            run_python(
            r#"
def add_one(value):
    return value + 1
"#,
        )
        .expect("function definition snippet should succeed"),
        );

        let second = completed_output(
            run_python_with_repl_state(
            r#"
add_one(41)
"#,
            &persisted_repl_state(&first),
        )
        .expect("second snippet should reuse prior function definitions"),
        );

        assert_eq!(second.value, Some(Value::Int(42)));
        assert!(second.repl_state.is_some());
    }

    #[test]
    fn later_snippets_see_mutations_from_earlier_snippets() {
        let first = completed_output(
            run_python(
                r#"
counter = 1
"#,
            )
            .expect("first snippet should succeed"),
        );

        let second = completed_output(
            run_python_with_repl_state(
                r#"
counter += 4
"#,
                &persisted_repl_state(&first),
            )
            .expect("second snippet should mutate existing state"),
        );
        assert_eq!(second.value, None);

        let third = completed_output(
            run_python_with_repl_state(
                r#"
counter
"#,
                &persisted_repl_state(&second),
            )
            .expect("third snippet should see the mutated value"),
        );
        assert_eq!(third.value, Some(Value::Int(5)));
    }

    #[test]
    fn independent_repl_states_do_not_leak_between_sequences() {
        let state_a = completed_output(
            run_python(
                r#"
trip = "berlin"
"#,
            )
            .expect("first sequence should succeed"),
        );
        let state_b = completed_output(
            run_python(
                r#"
trip = "rome"
"#,
            )
            .expect("second sequence should succeed"),
        );

        let read_a = completed_output(
            run_python_with_repl_state(
                r#"
trip
"#,
                &persisted_repl_state(&state_a),
            )
            .expect("first repl state should retain its own value"),
        );
        let read_b = completed_output(
            run_python_with_repl_state(
                r#"
trip
"#,
                &persisted_repl_state(&state_b),
            )
            .expect("second repl state should retain its own value"),
        );

        assert_eq!(read_a.value, Some(Value::String("berlin".to_owned())));
        assert_eq!(read_b.value, Some(Value::String("rome".to_owned())));
    }

    #[test]
    fn normalizes_tuple_as_list() {
        let value = monty_object_to_value(MontyObject::Tuple(vec![
            MontyObject::Int(1),
            MontyObject::String("two".to_owned()),
        ]))
        .expect("tuple should normalize");

        assert_eq!(
            value,
            Value::List(vec![Value::Int(1), Value::String("two".to_owned())])
        );
    }

    #[test]
    fn normalizes_namedtuple_as_record() {
        let value = monty_object_to_value(MontyObject::NamedTuple {
            type_name: "WeatherPair".to_owned(),
            field_names: vec!["location".to_owned(), "day".to_owned()],
            values: vec![
                MontyObject::String("Berlin".to_owned()),
                MontyObject::String("tomorrow".to_owned()),
            ],
        })
        .expect("namedtuple should normalize");

        let mut expected_fields = BTreeMap::new();
        expected_fields.insert("location".to_owned(), Value::String("Berlin".to_owned()));
        expected_fields.insert("day".to_owned(), Value::String("tomorrow".to_owned()));
        assert_eq!(
            value,
            Value::Record {
                name: "WeatherPair".to_owned(),
                fields: expected_fields,
            }
        );
    }
}
