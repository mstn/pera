use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct MapEntry {
    pub key: Value,
    pub value: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Value {
    Null,
    Bool(bool),
    Int(i64),
    String(String),
    List(Vec<Value>),
    Tuple(Vec<Value>),
    Map(Vec<MapEntry>),
    Record {
        name: String,
        fields: BTreeMap<String, Value>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum CanonicalValue {
    Null,
    Bool(bool),
    S32(i32),
    S64(i64),
    U32(u32),
    U64(u64),
    String(String),
    List(Vec<CanonicalValue>),
    Record(BTreeMap<String, CanonicalValue>),
    EnumCase(String),
    Tuple(Vec<CanonicalValue>),
}
