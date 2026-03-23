use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Value {
    Null,
    Bool(bool),
    Int(i64),
    String(String),
    List(Vec<Value>),
    Map(BTreeMap<String, Value>),
}
