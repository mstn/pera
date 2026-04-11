//! Renderer-agnostic UI specification types for Pera.

use std::collections::BTreeMap;

use pera_core::{ActionName, CanonicalValue};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize)]
pub struct UiSpecId(String);

impl UiSpecId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize)]
pub struct UiNodeId(String);

impl UiNodeId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize)]
pub struct UiComponentName(String);

impl UiComponentName {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct UiSpec {
    pub id: UiSpecId,
    pub version: String,
    pub title: Option<String>,
    pub root: UiNodeId,
    pub nodes: Vec<UiNode>,
    pub data_schema: Option<serde_json::Value>,
    pub initial_data: Option<serde_json::Value>,
}

impl UiSpec {
    pub fn node(&self, id: &UiNodeId) -> Option<&UiNode> {
        self.nodes.iter().find(|node| &node.id == id)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct UiNode {
    pub id: UiNodeId,
    pub component: UiComponentName,
    pub props: BTreeMap<String, UiPropValue>,
    pub children: Vec<UiNodeId>,
    pub events: Vec<UiEventHandler>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum UiPropValue {
    Null,
    Bool {
        value: bool,
    },
    Int {
        value: i64,
    },
    String {
        value: String,
    },
    Binding {
        binding: UiBinding,
    },
    List {
        items: Vec<UiPropValue>,
    },
    Object {
        fields: BTreeMap<String, UiPropValue>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct UiBinding {
    pub path: String,
}

impl UiBinding {
    pub fn new(path: impl Into<String>) -> Self {
        Self { path: path.into() }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct UiEventHandler {
    pub event: UiEvent,
    pub action: UiActionInvocation,
    pub result: Option<UiActionResultBinding>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UiEvent {
    Click,
    Change,
    Submit,
    Focus,
    Blur,
    Open,
    Close,
    Select,
    Input,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct UiActionInvocation {
    pub name: ActionName,
    pub args: BTreeMap<String, UiActionArgValue>,
}

impl UiActionInvocation {
    pub fn new(name: ActionName) -> Self {
        Self {
            name,
            args: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum UiActionArgValue {
    Literal {
        value: CanonicalValue,
    },
    Binding {
        binding: UiBinding,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct UiActionResultBinding {
    pub path: String,
}

impl UiActionResultBinding {
    pub fn new(path: impl Into<String>) -> Self {
        Self { path: path.into() }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use pera_core::{ActionName, CanonicalValue};

    use super::{
        UiActionArgValue, UiActionInvocation, UiActionResultBinding, UiBinding,
        UiComponentName, UiEvent, UiEventHandler, UiNode, UiNodeId, UiPropValue, UiSpec,
        UiSpecId,
    };

    #[test]
    fn ui_spec_round_trips_as_json() {
        let spec = UiSpec {
            id: UiSpecId::new("contact-form"),
            version: "1".to_owned(),
            title: Some("Contact us".to_owned()),
            root: UiNodeId::new("root"),
            nodes: vec![
                UiNode {
                    id: UiNodeId::new("root"),
                    component: UiComponentName::new("column"),
                    props: BTreeMap::new(),
                    children: vec![
                        UiNodeId::new("title"),
                        UiNodeId::new("name_field"),
                        UiNodeId::new("submit_button"),
                    ],
                    events: Vec::new(),
                },
                UiNode {
                    id: UiNodeId::new("title"),
                    component: UiComponentName::new("text"),
                    props: BTreeMap::from([(
                        "text".to_owned(),
                        UiPropValue::String {
                            value: "Contact us".to_owned(),
                        },
                    )]),
                    children: Vec::new(),
                    events: Vec::new(),
                },
                UiNode {
                    id: UiNodeId::new("name_field"),
                    component: UiComponentName::new("text_field"),
                    props: BTreeMap::from([
                        (
                            "label".to_owned(),
                            UiPropValue::String {
                                value: "Name".to_owned(),
                            },
                        ),
                        (
                            "value".to_owned(),
                            UiPropValue::Binding {
                                binding: UiBinding::new("/contact/name"),
                            },
                        ),
                    ]),
                    children: Vec::new(),
                    events: Vec::new(),
                },
                UiNode {
                    id: UiNodeId::new("submit_button"),
                    component: UiComponentName::new("button"),
                    props: BTreeMap::from([(
                        "label".to_owned(),
                        UiPropValue::String {
                            value: "Submit".to_owned(),
                        },
                    )]),
                    children: Vec::new(),
                    events: vec![UiEventHandler {
                        event: UiEvent::Click,
                        action: UiActionInvocation {
                            name: ActionName::new("submit_contact"),
                            args: BTreeMap::from([
                                (
                                    "name".to_owned(),
                                    UiActionArgValue::Binding {
                                        binding: UiBinding::new("/contact/name"),
                                    },
                                ),
                                (
                                    "notify".to_owned(),
                                    UiActionArgValue::Literal {
                                        value: CanonicalValue::Bool(true),
                                    },
                                ),
                            ]),
                        },
                        result: Some(UiActionResultBinding::new("/submission")),
                    }],
                },
            ],
            data_schema: Some(serde_json::json!({
                "type": "object",
                "properties": {
                    "contact": {
                        "type": "object",
                        "properties": {
                            "name": { "type": "string" }
                        }
                    }
                }
            })),
            initial_data: Some(serde_json::json!({
                "contact": {
                    "name": ""
                }
            })),
        };

        let json = serde_json::to_value(&spec).expect("ui spec should serialize");
        let restored: UiSpec = serde_json::from_value(json).expect("ui spec should deserialize");

        assert_eq!(restored, spec);
        assert_eq!(
            restored.node(&UiNodeId::new("submit_button")).map(|node| node.events.len()),
            Some(1)
        );
        assert_eq!(
            restored
                .node(&UiNodeId::new("submit_button"))
                .and_then(|node| node.events.first())
                .map(|event| event.action.args.len()),
            Some(2)
        );
    }
}
