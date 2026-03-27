use serde_json::{Value, json};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeEnvironmentTool {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

pub fn default_code_environment_tools() -> Vec<CodeEnvironmentTool> {
    vec![
        CodeEnvironmentTool {
            name: "load_skill".to_owned(),
            description:
                "Load a skill by name so you can use it while working on the current request."
                    .to_owned(),
            input_schema: json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "skill_name": {
                        "type": "string",
                        "description": "The name of the skill to load."
                    }
                },
                "required": ["skill_name"]
            }),
        },
        CodeEnvironmentTool {
            name: "unload_skill".to_owned(),
            description: "Unload a previously loaded skill when you no longer need it."
                .to_owned(),
            input_schema: json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "skill_name": {
                        "type": "string",
                        "description": "The name of the skill to unload."
                    }
                },
                "required": ["skill_name"]
            }),
        },
        CodeEnvironmentTool {
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
