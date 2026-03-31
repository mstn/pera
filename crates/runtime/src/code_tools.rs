use serde_json::{Value, json};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentWorkspaceTool {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

pub fn agent_workspace_tools(
    available_skill_names: &[String],
    active_skill_names: &[String],
) -> Vec<AgentWorkspaceTool> {
    vec![
        AgentWorkspaceTool {
            name: "load_skill".to_owned(),
            description:
                "Load a skill by name so you can use it while working on the current request."
                    .to_owned(),
            input_schema: skill_name_schema(
                "The name of the skill to load.",
                available_skill_names,
            ),
        },
        AgentWorkspaceTool {
            name: "unload_skill".to_owned(),
            description: "Unload a previously loaded skill when you no longer need it."
                .to_owned(),
            input_schema: skill_name_schema(
                "The name of the skill to unload.",
                active_skill_names,
            ),
        },
        AgentWorkspaceTool {
            name: "execute_code".to_owned(),
            description:
                "Execute code in the workspace and inspect the result before replying."
                    .to_owned(),
            input_schema: json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "language": {
                        "type": "string",
                        "description": "The language of the code to run."
                    },
                    "source": {
                        "type": "string",
                        "description": "The code to execute."
                    },
                    "handoff_user_message": {
                        "type": "string",
                        "description": "A very short message telling the user what the code is about to do."
                    }
                },
                "required": ["language", "source", "handoff_user_message"]
            }),
        },
    ]
}

fn skill_name_schema(description: &str, skill_names: &[String]) -> Value {
    let mut skill_name = json!({
        "type": "string",
        "description": description,
    });
    if !skill_names.is_empty() {
        skill_name["enum"] = Value::Array(
            skill_names
                .iter()
                .cloned()
                .map(Value::String)
                .collect(),
        );
    }

    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "skill_name": skill_name
        },
        "required": ["skill_name"]
    })
}
