#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CanonicalPackageRef {
    pub namespace: String,
    pub name: String,
    pub version: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CanonicalWorld {
    pub package: Option<CanonicalPackageRef>,
    pub name: String,
    pub docs: Option<String>,
    pub imports: Vec<CanonicalInterface>,
    pub exports: Vec<CanonicalInterface>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CanonicalInterface {
    pub name: String,
    pub docs: Option<String>,
    pub functions: Vec<CanonicalFunction>,
    pub types: Vec<CanonicalTypeDef>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CanonicalFunction {
    pub name: String,
    pub docs: Option<String>,
    pub params: Vec<CanonicalParam>,
    pub result: CanonicalFunctionResult,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CanonicalParam {
    pub name: String,
    pub ty: CanonicalTypeRef,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum CanonicalFunctionResult {
    None,
    Scalar(CanonicalTypeRef),
    Named(Vec<CanonicalParam>),
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CanonicalTypeDef {
    pub name: String,
    pub docs: Option<String>,
    pub kind: CanonicalTypeDefKind,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum CanonicalTypeDefKind {
    Alias(CanonicalTypeRef),
    Record(CanonicalRecord),
    Enum(Vec<CanonicalEnumCase>),
    Variant(Vec<CanonicalVariantCase>),
    Flags(Vec<String>),
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CanonicalRecord {
    pub fields: Vec<CanonicalField>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CanonicalField {
    pub name: String,
    pub docs: Option<String>,
    pub ty: CanonicalTypeRef,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CanonicalEnumCase {
    pub name: String,
    pub docs: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CanonicalVariantCase {
    pub name: String,
    pub docs: Option<String>,
    pub ty: Option<CanonicalTypeRef>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum CanonicalTypeRef {
    Primitive(CanonicalPrimitiveType),
    Named(String),
    List(Box<CanonicalTypeRef>),
    Option(Box<CanonicalTypeRef>),
    Result {
        ok: Option<Box<CanonicalTypeRef>>,
        err: Option<Box<CanonicalTypeRef>>,
    },
    Tuple(Vec<CanonicalTypeRef>),
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum CanonicalPrimitiveType {
    Bool,
    S8,
    S16,
    S32,
    S64,
    U8,
    U16,
    U32,
    U64,
    Float32,
    Float64,
    Char,
    String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum CanonicalType {
    Primitive(CanonicalPrimitiveType),
    List(Box<CanonicalType>),
    Option(Box<CanonicalType>),
    Result {
        ok: Option<Box<CanonicalType>>,
        err: Option<Box<CanonicalType>>,
    },
    Tuple(Vec<CanonicalType>),
    Named(CanonicalTypeDef),
}
