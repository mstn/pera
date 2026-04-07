use std::collections::BTreeMap;

use chrono::{Datelike, Local, Timelike, Utc};
use monty::{
    FunctionCall, MontyDate, MontyDateTime, MontyObject, MontyRun, NoLimitTracker, OsCall,
    OsFunction, PrintWriter, RunProgress,
};
use pera_core::{
    ActionName, CodeArtifact, CodeLanguage, CompiledProgram, ExecutionOutput, ExecutionSnapshot,
    ExternalCall, InputValues, Interpreter, InterpreterError, InterpreterKind, InterpreterStep,
    Suspension, Value,
};

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

        let runner = MontyRun::new(
            code.source.clone(),
            code.script_name.as_str(),
            code.inputs
                .iter()
                .map(|input| input.as_str().to_owned())
                .collect(),
        )
        .map_err(to_interpreter_error)?;

        let bytes = runner.dump().map_err(to_interpreter_error)?;

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
    ) -> Result<InterpreterStep, InterpreterError> {
        let runner = MontyRun::load(&program.bytes).map_err(to_interpreter_error)?;
        let ordered_inputs = input_values_to_monty_objects(&program.input_order, inputs)?;
        let progress = runner
            .start(ordered_inputs, NoLimitTracker, PrintWriter::Disabled)
            .map_err(to_interpreter_error)?;
        progress_to_step(progress)
    }

    fn resume(
        &self,
        snapshot: &ExecutionSnapshot,
        return_value: &Value,
    ) -> Result<InterpreterStep, InterpreterError> {
        let progress =
            RunProgress::<NoLimitTracker>::load(&snapshot.bytes).map_err(to_interpreter_error)?;
        let progress = progress
            .into_function_call()
            .ok_or_else(|| InterpreterError::new("snapshot is not a function call suspension"))?
            .resume(value_to_monty_object(return_value)?, PrintWriter::Disabled)
            .map_err(to_interpreter_error)?;
        progress_to_step(progress)
    }
}

fn progress_to_step(progress: RunProgress<NoLimitTracker>) -> Result<InterpreterStep, InterpreterError> {
    let mut progress = progress;

    loop {
        match progress {
            RunProgress::FunctionCall(call) => return function_call_to_step(call),
            RunProgress::OsCall(call) => {
                progress = resume_os_call(call)?;
            }
            RunProgress::Complete(value) => {
                return Ok(InterpreterStep::Completed(ExecutionOutput {
                    value: monty_object_to_value(value)?,
                }));
            }
            RunProgress::ResolveFutures(_) => {
                return Err(InterpreterError::new(
                    "Monty returned unsupported suspension kind: resolve_futures",
                ));
            }
            RunProgress::NameLookup(lookup) => {
                return Err(InterpreterError::new(format!(
                    "Monty returned unsupported suspension kind: name_lookup for '{}'",
                    lookup.name
                )));
            }
        }
    }
}

fn resume_os_call(call: OsCall<NoLimitTracker>) -> Result<RunProgress<NoLimitTracker>, InterpreterError> {
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
        .map_err(to_interpreter_error)
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

fn function_call_to_step(call: FunctionCall<NoLimitTracker>) -> Result<InterpreterStep, InterpreterError> {
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
    let snapshot = RunProgress::FunctionCall(call).dump().map_err(to_interpreter_error)?;

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
            .map(|(key, value)| Ok((MontyObject::String(key.clone()), value_to_monty_object(value)?)))
            .collect::<Result<Vec<_>, _>>()
            .map(MontyObject::dict),
        Value::Record { name, fields } => {
            let attrs = fields
                .iter()
                .map(|(key, value)| {
                    Ok((MontyObject::String(key.clone()), value_to_monty_object(value)?))
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
        interpreter.start(&program, &InputValues::new())
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
                Value::String(value) => assert_eq!(value.len(), 10),
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
                Value::String(value) => assert!(value.contains('T')),
                other => panic!("unexpected output value: {other:?}"),
            },
            other => panic!("unexpected interpreter step: {other:?}"),
        }
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
