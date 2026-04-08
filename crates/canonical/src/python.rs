use std::collections::BTreeMap;

use pera_core::Value;

use crate::ir::{
    CanonicalField, CanonicalFunctionResult, CanonicalInterface, CanonicalPrimitiveType,
    CanonicalTypeDef, CanonicalTypeDefKind, CanonicalTypeRef, CanonicalVariantCase,
    CanonicalWorld,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanonicalPythonBindings {
    pub module_name: String,
    pub functions: Vec<CanonicalPythonFunction>,
    pub type_names: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanonicalPythonFunction {
    pub canonical_name: String,
    pub python_name: String,
    pub docs: Option<String>,
    pub params: Vec<CanonicalPythonParam>,
    pub return_annotation: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanonicalPythonParam {
    pub canonical_name: String,
    pub python_name: String,
    pub annotation: String,
}

pub fn render_python_stubs(world: &CanonicalWorld) -> String {
    let bindings = canonical_python_bindings(world);
    let exports = world
        .exports
        .first()
        .map(render_python_exports)
        .unwrap_or_default();

    let mut sections = vec![
        "from dataclasses import dataclass".to_owned(),
        "from enum import Enum".to_owned(),
        "from typing import Optional, TypeAlias".to_owned(),
    ];

    if !exports.is_empty() {
        sections.push(String::new());
        sections.push(exports);
    }

    if !bindings.functions.is_empty() {
        sections.push(String::new());
        sections.push(render_python_functions(&bindings.functions));
    }

    sections.join("\n")
}

pub fn canonical_python_bindings(world: &CanonicalWorld) -> CanonicalPythonBindings {
    let exports = world.exports.first();
    let type_names = exports
        .map(|interface| {
            interface
                .types
                .iter()
                .map(|ty| (ty.name.clone(), python_type_name(&ty.name)))
                .collect()
        })
        .unwrap_or_default();

    let functions = exports
        .map(|interface| {
            interface
                .functions
                .iter()
                .map(|function| CanonicalPythonFunction {
                    canonical_name: function.name.clone(),
                    python_name: python_function_name(&function.name),
                    docs: function.docs.clone(),
                    params: function
                        .params
                        .iter()
                        .map(|param| CanonicalPythonParam {
                            canonical_name: param.name.clone(),
                            python_name: python_function_name(&param.name),
                            annotation: python_annotation(&param.ty),
                        })
                        .collect(),
                    return_annotation: python_function_result_annotation(&function.result),
                })
                .collect()
        })
        .unwrap_or_default();

    CanonicalPythonBindings {
        module_name: python_module_name(&world.name),
        functions,
        type_names,
    }
}

pub fn python_module_name(name: &str) -> String {
    python_function_name(name)
}

pub fn python_function_name(name: &str) -> String {
    let mut output = String::with_capacity(name.len());
    for ch in name.chars() {
        match ch {
            '-' | ' ' | '.' => output.push('_'),
            _ => output.push(ch),
        }
    }
    output
}

pub fn python_type_name(name: &str) -> String {
    let mut output = String::new();

    for part in name
        .split(['-', '_', ' ', '.'])
        .filter(|part| !part.is_empty())
    {
        let mut chars = part.chars();
        if let Some(first) = chars.next() {
            output.push(first.to_ascii_uppercase());
            for ch in chars {
                output.push(ch);
            }
        }
    }

    output
}

pub fn render_python_value(value: &Value) -> String {
    match value {
        Value::Null => "None".to_owned(),
        Value::Bool(value) => {
            if *value {
                "True".to_owned()
            } else {
                "False".to_owned()
            }
        }
        Value::Int(value) => value.to_string(),
        Value::String(value) => format!("{value:?}"),
        Value::List(items) => format!(
            "[{}]",
            items
                .iter()
                .map(render_python_value)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Value::Tuple(items) => {
            let rendered = items
                .iter()
                .map(render_python_value)
                .collect::<Vec<_>>();
            match rendered.as_slice() {
                [single] => format!("({},)", single),
                _ => format!("({})", rendered.join(", ")),
            }
        }
        Value::Map(entries) => format!(
            "{{{}}}",
            entries
                .iter()
                .map(|(key, value)| format!("{key:?}: {}", render_python_value(value)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Value::Record { name, fields } => format!(
            "{}({})",
            python_type_name(name),
            fields
                .iter()
                .map(|(key, value)| format!("{}={}", python_function_name(key), render_python_value(value)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

fn render_python_exports(interface: &CanonicalInterface) -> String {
    let mut sections = Vec::new();

    for ty in &interface.types {
        sections.push(match &ty.kind {
            CanonicalTypeDefKind::Alias(alias) => render_alias(ty, alias),
            CanonicalTypeDefKind::Record(record) => render_record(ty, &record.fields),
            CanonicalTypeDefKind::Enum(cases) => render_enum(ty, cases),
            CanonicalTypeDefKind::Variant(cases) => render_variant(ty, cases),
            CanonicalTypeDefKind::Flags(flags) => render_flags(ty, flags),
        });
    }

    sections.join("\n\n")
}

fn render_alias(ty: &CanonicalTypeDef, alias: &CanonicalTypeRef) -> String {
    let mut lines = Vec::new();
    if let Some(docs) = &ty.docs {
        lines.extend(render_comment_block(docs, 0));
    }
    lines.push(format!(
        "{}: TypeAlias = {}",
        python_type_name(&ty.name),
        python_annotation(alias)
    ));
    lines.join("\n")
}

fn render_record(ty: &CanonicalTypeDef, fields: &[CanonicalField]) -> String {
    let mut lines = vec!["@dataclass".to_owned(), format!("class {}:", python_type_name(&ty.name))];
    if let Some(docs) = &ty.docs {
        lines.extend(render_docstring(docs, 4));
    }
    if fields.is_empty() {
        lines.push("    pass".to_owned());
        return lines.join("\n");
    }

    for field in fields {
        if let Some(docs) = &field.docs {
            lines.extend(render_comment_block(docs, 4));
        }
        lines.push(format!(
            "    {}: {}",
            python_function_name(&field.name),
            python_annotation(&field.ty)
        ));
    }

    lines.join("\n")
}

fn render_enum(ty: &CanonicalTypeDef, cases: &[crate::ir::CanonicalEnumCase]) -> String {
    let mut lines = vec![format!("class {}(Enum):", python_type_name(&ty.name))];
    if let Some(docs) = &ty.docs {
        lines.extend(render_docstring(docs, 4));
    }
    if cases.is_empty() {
        lines.push("    pass".to_owned());
        return lines.join("\n");
    }

    for case in cases {
        if let Some(docs) = &case.docs {
            lines.extend(render_comment_block(docs, 4));
        }
        lines.push(format!(
            "    {} = {:?}",
            python_type_name(&case.name).to_ascii_uppercase(),
            case.name
        ));
    }

    lines.join("\n")
}

fn render_variant(ty: &CanonicalTypeDef, cases: &[CanonicalVariantCase]) -> String {
    let mut lines = vec!["@dataclass".to_owned(), format!("class {}:", python_type_name(&ty.name))];
    if let Some(docs) = &ty.docs {
        lines.extend(render_docstring(docs, 4));
    }
    if cases.is_empty() {
        lines.push("    pass".to_owned());
        return lines.join("\n");
    }

    for case in cases {
        if let Some(docs) = &case.docs {
            lines.extend(render_comment_block(docs, 4));
        }
        match &case.ty {
            Some(ty) => lines.push(format!(
                "    {}: Optional[{}] = None",
                python_function_name(&case.name),
                python_annotation(ty)
            )),
            None => lines.push(format!(
                "    {}: bool = False",
                python_function_name(&case.name)
            )),
        }
    }

    lines.join("\n")
}

fn render_flags(ty: &CanonicalTypeDef, flags: &[String]) -> String {
    let mut lines = vec!["@dataclass".to_owned(), format!("class {}:", python_type_name(&ty.name))];
    if let Some(docs) = &ty.docs {
        lines.extend(render_docstring(docs, 4));
    }
    if flags.is_empty() {
        lines.push("    pass".to_owned());
        return lines.join("\n");
    }

    for flag in flags {
        lines.push(format!("    {}: bool = False", python_function_name(flag)));
    }

    lines.join("\n")
}

fn render_python_functions(functions: &[CanonicalPythonFunction]) -> String {
    functions
        .iter()
        .map(render_python_function)
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn render_python_function(function: &CanonicalPythonFunction) -> String {
    let params = function
        .params
        .iter()
        .map(|param| format!("{}: {}", param.python_name, param.annotation))
        .collect::<Vec<_>>()
        .join(", ");

    let mut lines = vec![format!(
        "def {}({}) -> {}:",
        function.python_name, params, function.return_annotation
    )];
    if let Some(docs) = &function.docs {
        lines.extend(render_docstring(docs, 4));
    }
    lines.push("    ...".to_owned());
    lines.join("\n")
}

fn python_function_result_annotation(result: &CanonicalFunctionResult) -> String {
    match result {
        CanonicalFunctionResult::None => "None".to_owned(),
        CanonicalFunctionResult::Scalar(ty) => python_annotation(ty),
        CanonicalFunctionResult::Named(params) => {
            if params.is_empty() {
                "None".to_owned()
            } else if params.len() == 1 {
                python_annotation(&params[0].ty)
            } else {
                let inner = params
                    .iter()
                    .map(|param| python_annotation(&param.ty))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("tuple[{inner}]")
            }
        }
    }
}

fn python_annotation(ty: &CanonicalTypeRef) -> String {
    match ty {
        CanonicalTypeRef::Primitive(primitive) => python_primitive(primitive).to_owned(),
        CanonicalTypeRef::Named(name) => python_type_name(name),
        CanonicalTypeRef::List(inner) => format!("list[{}]", python_annotation(inner)),
        CanonicalTypeRef::Option(inner) => format!("Optional[{}]", python_annotation(inner)),
        CanonicalTypeRef::Result { ok, err } => {
            let ok = ok
                .as_deref()
                .map(python_annotation)
                .unwrap_or_else(|| "None".to_owned());
            let err = err
                .as_deref()
                .map(python_annotation)
                .unwrap_or_else(|| "None".to_owned());
            format!("tuple[{}, {}]", ok, err)
        }
        CanonicalTypeRef::Tuple(items) => {
            let inner = items
                .iter()
                .map(python_annotation)
                .collect::<Vec<_>>()
                .join(", ");
            format!("tuple[{inner}]")
        }
    }
}

fn python_primitive(primitive: &CanonicalPrimitiveType) -> &'static str {
    match primitive {
        CanonicalPrimitiveType::Bool => "bool",
        CanonicalPrimitiveType::S8
        | CanonicalPrimitiveType::S16
        | CanonicalPrimitiveType::S32
        | CanonicalPrimitiveType::S64
        | CanonicalPrimitiveType::U8
        | CanonicalPrimitiveType::U16
        | CanonicalPrimitiveType::U32
        | CanonicalPrimitiveType::U64 => "int",
        CanonicalPrimitiveType::Float32 | CanonicalPrimitiveType::Float64 => "float",
        CanonicalPrimitiveType::Char | CanonicalPrimitiveType::String => "str",
    }
}

fn render_docstring(docs: &str, indent: usize) -> Vec<String> {
    let prefix = " ".repeat(indent);
    let lines = split_doc_lines(docs);
    if lines.is_empty() {
        return Vec::new();
    }
    if lines.len() == 1 {
        return vec![format!(r#"{prefix}"""{doc}""""#, doc = lines[0])];
    }

    let mut rendered = vec![format!(r#"{prefix}""""#)];
    for line in lines {
        rendered.push(format!("{prefix}{line}"));
    }
    rendered.push(format!(r#"{prefix}""""#));
    rendered
}

fn render_comment_block(docs: &str, indent: usize) -> Vec<String> {
    let prefix = " ".repeat(indent);
    split_doc_lines(docs)
        .into_iter()
        .map(|line| format!("{prefix}# {line}"))
        .collect()
}

fn split_doc_lines(docs: &str) -> Vec<String> {
    docs.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use pera_core::Value;

    use crate::load_canonical_world_from_wit;

    #[test]
    fn renders_secret_service_python_stubs() {
        let world = load_canonical_world_from_wit(
            "../../skills/examples/secret-service/world.wit",
            "secret-service-default",
        )
        .unwrap();

        let rendered = super::render_python_stubs(&world);

        assert!(rendered.contains("class MissionStatus(Enum):"));
        assert!(rendered.contains("class Mission:"));
        assert!(rendered.contains(r#"    """Mission plan and execution state."""#));
        assert!(rendered.contains("assigned_agent_id: Optional[str]"));
        assert!(rendered.contains(
            "def resolve_mission(mission_id: str, outcome: MissionOutcome, notes: Optional[str]) -> ResolveMissionOutput:"
        ));
        assert!(rendered.contains(
            r#"    """Resolve a mission, record the outcome, and release any assigned gadgets."""#
        ));
    }

    #[test]
    fn render_python_value_preserves_tuple_syntax() {
        let rendered = super::render_python_value(&Value::Tuple(vec![
            Value::List(vec![Value::String("meeting".to_owned())]),
            Value::List(vec![]),
            Value::Record {
                name: "trip-policy".to_owned(),
                fields: BTreeMap::from([(
                    "shared_room_allowed".to_owned(),
                    Value::Bool(true),
                )]),
            },
        ]));

        assert_eq!(
            rendered,
            "([\"meeting\"], [], TripPolicy(shared_room_allowed=True))"
        );
    }
}
