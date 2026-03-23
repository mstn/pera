use std::error::Error;
use std::fmt::{Display, Formatter};
use std::path::Path;

use wit_parser::{
    Enum, Flags, Function, InterfaceId, PackageId, Record, Resolve, Result_, Tuple, Type,
    TypeDefKind, TypeId, Variant, WorldId, WorldItem, WorldKey,
};

use crate::ir::{
    CanonicalEnumCase, CanonicalField, CanonicalFunction, CanonicalFunctionResult,
    CanonicalInterface, CanonicalPackageRef, CanonicalParam, CanonicalPrimitiveType,
    CanonicalRecord, CanonicalTypeDef, CanonicalTypeDefKind, CanonicalTypeRef,
    CanonicalVariantCase, CanonicalWorld,
};

#[derive(Debug)]
pub struct CanonicalWitError {
    message: String,
}

impl CanonicalWitError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl Display for CanonicalWitError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl Error for CanonicalWitError {}

pub fn load_canonical_world_from_wit(
    wit_path: impl AsRef<Path>,
    world_name: &str,
) -> Result<CanonicalWorld, CanonicalWitError> {
    let mut resolve = Resolve::default();
    let (package_id, _) = resolve
        .push_path(wit_path.as_ref())
        .map_err(|error| CanonicalWitError::new(error.to_string()))?;

    let world_id = find_world(&resolve, package_id, world_name)?;
    canonical_world(&resolve, world_id)
}

fn find_world(
    resolve: &Resolve,
    package_id: PackageId,
    world_name: &str,
) -> Result<WorldId, CanonicalWitError> {
    resolve
        .worlds
        .iter()
        .find_map(|(world_id, world)| {
            if world.package == Some(package_id) && world.name == world_name {
                Some(world_id)
            } else {
                None
            }
        })
        .ok_or_else(|| CanonicalWitError::new(format!("world '{world_name}' was not found")))
}

fn canonical_world(
    resolve: &Resolve,
    world_id: WorldId,
) -> Result<CanonicalWorld, CanonicalWitError> {
    let world = &resolve.worlds[world_id];
    let package = world.package.map(|package_id| {
        let package = &resolve.packages[package_id];
        CanonicalPackageRef {
            namespace: package.name.namespace.to_string(),
            name: package.name.name.to_string(),
            version: package.name.version.as_ref().map(ToString::to_string),
        }
    });

    let imports = world
        .imports
        .iter()
        .map(|(key, item)| canonical_world_item(resolve, key, item))
        .collect::<Result<Vec<_>, _>>()?;
    let exports = world
        .exports
        .iter()
        .map(|(key, item)| canonical_world_item(resolve, key, item))
        .collect::<Result<Vec<_>, _>>()?;

    Ok(CanonicalWorld {
        package,
        name: world.name.clone(),
        docs: world.docs.contents.clone(),
        imports,
        exports,
    })
}

fn canonical_world_item(
    resolve: &Resolve,
    key: &WorldKey,
    item: &WorldItem,
) -> Result<CanonicalInterface, CanonicalWitError> {
    let name = world_key_name(key);
    match item {
        WorldItem::Interface { id, .. } => canonical_interface(resolve, name, *id),
        WorldItem::Function(function) => Ok(CanonicalInterface {
            name,
            docs: function.docs.contents.clone(),
            functions: vec![canonical_function(resolve, function)?],
            types: Vec::new(),
        }),
        WorldItem::Type(_) => Err(CanonicalWitError::new(
            "top-level world types are not yet supported in canonical import",
        )),
    }
}

fn canonical_interface(
    resolve: &Resolve,
    name: String,
    interface_id: InterfaceId,
) -> Result<CanonicalInterface, CanonicalWitError> {
    let interface = &resolve.interfaces[interface_id];

    let mut types = interface
        .types
        .iter()
        .map(|(_, type_id)| canonical_type_def(resolve, *type_id))
        .collect::<Result<Vec<_>, _>>()?;
    types.sort_by(|left, right| left.name.cmp(&right.name));

    let mut functions = interface
        .functions
        .iter()
        .map(|(_, function)| canonical_function(resolve, function))
        .collect::<Result<Vec<_>, _>>()?;
    functions.sort_by(|left, right| left.name.cmp(&right.name));

    Ok(CanonicalInterface {
        name,
        docs: interface.docs.contents.clone(),
        functions,
        types,
    })
}

fn canonical_function(
    resolve: &Resolve,
    function: &Function,
) -> Result<CanonicalFunction, CanonicalWitError> {
    let params = function
        .params
        .iter()
        .map(|(name, ty)| {
            Ok(CanonicalParam {
                name: name.clone(),
                ty: canonical_type_ref(resolve, ty)?,
            })
        })
        .collect::<Result<Vec<_>, CanonicalWitError>>()?;

    let result = match &function.result {
        Some(ty) => CanonicalFunctionResult::Scalar(canonical_type_ref(resolve, ty)?),
        None => CanonicalFunctionResult::None,
    };

    Ok(CanonicalFunction {
        name: function.name.clone(),
        docs: function.docs.contents.clone(),
        params,
        result,
    })
}

fn canonical_type_def(
    resolve: &Resolve,
    type_id: TypeId,
) -> Result<CanonicalTypeDef, CanonicalWitError> {
    let ty = &resolve.types[type_id];
    let name = ty.name.clone().ok_or_else(|| {
        CanonicalWitError::new("anonymous interface type cannot be canonicalized")
    })?;

    let kind = canonical_type_def_kind(resolve, &ty.kind, &name)?;
    let docs = ty
        .docs
        .contents
        .clone()
        .or_else(|| fallback_type_docs(resolve, &ty.kind));

    Ok(CanonicalTypeDef { name, docs, kind })
}

fn fallback_type_docs(resolve: &Resolve, kind: &TypeDefKind) -> Option<String> {
    match kind {
        TypeDefKind::Type(Type::Id(target)) => resolve.types[*target].docs.contents.clone(),
        _ => None,
    }
}

fn canonical_type_def_kind(
    resolve: &Resolve,
    kind: &TypeDefKind,
    name: &str,
) -> Result<CanonicalTypeDefKind, CanonicalWitError> {
    match kind {
        TypeDefKind::Record(record) => Ok(CanonicalTypeDefKind::Record(canonical_record(
            resolve, record,
        )?)),
        TypeDefKind::Enum(enum_) => Ok(CanonicalTypeDefKind::Enum(canonical_enum(enum_))),
        TypeDefKind::Variant(variant) => Ok(CanonicalTypeDefKind::Variant(canonical_variant(
            resolve, variant,
        )?)),
        TypeDefKind::Flags(flags) => Ok(CanonicalTypeDefKind::Flags(canonical_flags(flags))),
        TypeDefKind::Tuple(tuple) => Ok(CanonicalTypeDefKind::Alias(CanonicalTypeRef::Tuple(
            canonical_tuple(resolve, tuple)?,
        ))),
        TypeDefKind::Option(inner) => Ok(CanonicalTypeDefKind::Alias(CanonicalTypeRef::Option(
            Box::new(canonical_type_ref(resolve, inner)?),
        ))),
        TypeDefKind::Result(result) => Ok(CanonicalTypeDefKind::Alias(canonical_result(
            resolve, result,
        )?)),
        TypeDefKind::List(inner) => Ok(CanonicalTypeDefKind::Alias(CanonicalTypeRef::List(
            Box::new(canonical_type_ref(resolve, inner)?),
        ))),
        TypeDefKind::Type(Type::Id(target)) => {
            let target = &resolve.types[*target];
            canonical_type_def_kind(resolve, &target.kind, name)
        }
        TypeDefKind::Type(inner) => Ok(CanonicalTypeDefKind::Alias(canonical_type_ref(
            resolve, inner,
        )?)),
        TypeDefKind::Resource
        | TypeDefKind::Handle(_)
        | TypeDefKind::Future(_)
        | TypeDefKind::Stream(_)
        | TypeDefKind::Unknown => Err(CanonicalWitError::new(format!(
            "unsupported WIT type '{}'",
            name
        ))),
    }
}

fn canonical_record(
    resolve: &Resolve,
    record: &Record,
) -> Result<CanonicalRecord, CanonicalWitError> {
    let fields = record
        .fields
        .iter()
        .map(|field| {
            Ok(CanonicalField {
                name: field.name.clone(),
                docs: field.docs.contents.clone(),
                ty: canonical_type_ref(resolve, &field.ty)?,
            })
        })
        .collect::<Result<Vec<_>, CanonicalWitError>>()?;

    Ok(CanonicalRecord { fields })
}

fn canonical_enum(enum_: &Enum) -> Vec<CanonicalEnumCase> {
    enum_
        .cases
        .iter()
        .map(|case| CanonicalEnumCase {
            name: case.name.clone(),
            docs: case.docs.contents.clone(),
        })
        .collect()
}

fn canonical_variant(
    resolve: &Resolve,
    variant: &Variant,
) -> Result<Vec<CanonicalVariantCase>, CanonicalWitError> {
    variant
        .cases
        .iter()
        .map(|case| {
            Ok(CanonicalVariantCase {
                name: case.name.clone(),
                docs: case.docs.contents.clone(),
                ty: case
                    .ty
                    .as_ref()
                    .map(|ty| canonical_type_ref(resolve, ty))
                    .transpose()?,
            })
        })
        .collect()
}

fn canonical_flags(flags: &Flags) -> Vec<String> {
    flags.flags.iter().map(|flag| flag.name.clone()).collect()
}

fn canonical_result(
    resolve: &Resolve,
    result: &Result_,
) -> Result<CanonicalTypeRef, CanonicalWitError> {
    Ok(CanonicalTypeRef::Result {
        ok: result
            .ok
            .as_ref()
            .map(|ty| canonical_type_ref(resolve, ty).map(Box::new))
            .transpose()?,
        err: result
            .err
            .as_ref()
            .map(|ty| canonical_type_ref(resolve, ty).map(Box::new))
            .transpose()?,
    })
}

fn canonical_tuple(
    resolve: &Resolve,
    tuple: &Tuple,
) -> Result<Vec<CanonicalTypeRef>, CanonicalWitError> {
    tuple
        .types
        .iter()
        .map(|ty| canonical_type_ref(resolve, ty))
        .collect()
}

fn canonical_type_ref(resolve: &Resolve, ty: &Type) -> Result<CanonicalTypeRef, CanonicalWitError> {
    match ty {
        Type::Bool => Ok(CanonicalTypeRef::Primitive(CanonicalPrimitiveType::Bool)),
        Type::S8 => Ok(CanonicalTypeRef::Primitive(CanonicalPrimitiveType::S8)),
        Type::S16 => Ok(CanonicalTypeRef::Primitive(CanonicalPrimitiveType::S16)),
        Type::S32 => Ok(CanonicalTypeRef::Primitive(CanonicalPrimitiveType::S32)),
        Type::S64 => Ok(CanonicalTypeRef::Primitive(CanonicalPrimitiveType::S64)),
        Type::U8 => Ok(CanonicalTypeRef::Primitive(CanonicalPrimitiveType::U8)),
        Type::U16 => Ok(CanonicalTypeRef::Primitive(CanonicalPrimitiveType::U16)),
        Type::U32 => Ok(CanonicalTypeRef::Primitive(CanonicalPrimitiveType::U32)),
        Type::U64 => Ok(CanonicalTypeRef::Primitive(CanonicalPrimitiveType::U64)),
        Type::F32 => Ok(CanonicalTypeRef::Primitive(CanonicalPrimitiveType::Float32)),
        Type::F64 => Ok(CanonicalTypeRef::Primitive(CanonicalPrimitiveType::Float64)),
        Type::Char => Ok(CanonicalTypeRef::Primitive(CanonicalPrimitiveType::Char)),
        Type::String => Ok(CanonicalTypeRef::Primitive(CanonicalPrimitiveType::String)),
        Type::Id(type_id) => canonical_named_or_inline_type_ref(resolve, *type_id),
        Type::ErrorContext => Err(CanonicalWitError::new(
            "resource and handle types are not yet supported in canonical import",
        )),
    }
}

fn canonical_named_or_inline_type_ref(
    resolve: &Resolve,
    type_id: TypeId,
) -> Result<CanonicalTypeRef, CanonicalWitError> {
    let ty = &resolve.types[type_id];
    if let Some(name) = &ty.name {
        return Ok(CanonicalTypeRef::Named(name.clone()));
    }

    match &ty.kind {
        TypeDefKind::List(inner) => Ok(CanonicalTypeRef::List(Box::new(canonical_type_ref(
            resolve, inner,
        )?))),
        TypeDefKind::Option(inner) => Ok(CanonicalTypeRef::Option(Box::new(canonical_type_ref(
            resolve, inner,
        )?))),
        TypeDefKind::Result(result) => canonical_result(resolve, result),
        TypeDefKind::Tuple(tuple) => Ok(CanonicalTypeRef::Tuple(canonical_tuple(resolve, tuple)?)),
        TypeDefKind::Type(inner) => canonical_type_ref(resolve, inner),
        _ => Err(CanonicalWitError::new(
            "anonymous non-inline type is not supported in canonical import",
        )),
    }
}

fn world_key_name(key: &WorldKey) -> String {
    match key {
        WorldKey::Name(name) => name.clone(),
        WorldKey::Interface(id) => format!("interface-{}", id.index()),
    }
}

#[cfg(test)]
mod tests {
    use super::load_canonical_world_from_wit;
    use crate::{CanonicalFunctionResult, CanonicalTypeDefKind};

    #[test]
    fn imports_secret_service_world_into_canonical_ir() {
        let world = load_canonical_world_from_wit(
            "../../skills/examples/secret-service/world.wit",
            "secret-service-default",
        )
        .unwrap();

        assert_eq!(world.name, "secret-service-default");
        assert_eq!(world.imports.len(), 2);
        assert_eq!(world.exports.len(), 1);

        let exports = &world.exports[0];
        assert!(
            exports
                .functions
                .iter()
                .any(|function| function.name == "resolve-mission")
        );
        assert!(exports.types.iter().any(|ty| ty.name == "mission"));

        let mission = exports
            .types
            .iter()
            .find(|ty| ty.name == "mission")
            .unwrap();
        match &mission.kind {
            CanonicalTypeDefKind::Record(record) => {
                assert!(
                    record
                        .fields
                        .iter()
                        .any(|field| field.name == "assigned-agent-id")
                );
            }
            _ => panic!("mission should import as a canonical record"),
        }

        let resolve_mission = exports
            .functions
            .iter()
            .find(|function| function.name == "resolve-mission")
            .unwrap();
        match &resolve_mission.result {
            CanonicalFunctionResult::Scalar(_) => {}
            _ => panic!("resolve-mission should have a scalar result"),
        }
    }
}
