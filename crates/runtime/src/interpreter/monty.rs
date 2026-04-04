use std::collections::BTreeMap;

use monty::{FunctionCall, MontyObject, MontyRun, NoLimitTracker, PrintWriter, RunProgress};
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
    match progress {
        RunProgress::FunctionCall(call) => function_call_to_step(call),
        RunProgress::Complete(value) => Ok(InterpreterStep::Completed(ExecutionOutput {
            value: monty_object_to_value(value)?,
        })),
        _ => Err(InterpreterError::new(
            "Monty returned a suspension kind that is not yet supported",
        )),
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

    use monty::MontyObject;
    use pera_core::Value;

    use super::monty_object_to_value;

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
