mod todo_env;
mod ui_participant;

use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;

use pera_orchestrator::{
    InitialInboxMessage, Orchestrator, ParticipantId, RunLimits, RunRequest, TaskSpec,
    TerminationCondition, TrajectoryEvent,
};
use pera_ui::UiSpec;
use todo_env::TodoEnvironment;
use ui_participant::UiDemoParticipant;

fn default_spec_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("specs")
        .join("todo-app.json")
}

fn load_spec(path: &PathBuf) -> Result<UiSpec, String> {
    let raw = fs::read_to_string(path)
        .map_err(|error| format!("failed to read ui spec at {}: {error}", path.display()))?;
    serde_json::from_str(&raw)
        .map_err(|error| format!("failed to parse ui spec at {}: {error}", path.display()))
}

fn run_request(command: &str) -> RunRequest {
    RunRequest {
        task: TaskSpec {
            id: "ui-demo".to_owned(),
            instructions: "Run the UI demo".to_owned(),
        },
        limits: RunLimits {
            max_steps: 8,
            max_steps_per_agent_loop: 8,
            max_actions: 4,
            max_messages: 8,
            max_failed_actions: None,
            max_consecutive_failed_actions: None,
            max_blocked_action_wait: None,
            max_duration: None,
        },
        termination_condition: TerminationCondition::AnyOfParticipantsCompletedLoop(vec![
            ParticipantId::Custom("ui".to_owned()),
        ]),
        initial_messages: vec![InitialInboxMessage {
            to: ParticipantId::Custom("ui".to_owned()),
            from: ParticipantId::Custom("driver".to_owned()),
            content: command.to_owned(),
        }],
    }
}

fn render_run_output(
    result: &pera_orchestrator::RunResult<
        todo_env::TodoObservation,
        todo_env::TodoAction,
        todo_env::TodoOutcome,
    >,
) -> Option<String> {
    result.trajectory.events.iter().rev().find_map(|event| match event {
        TrajectoryEvent::ParticipantMessage {
            participant,
            content,
        } if *participant == ParticipantId::Custom("ui".to_owned()) => Some(content.clone()),
        _ => None,
    })
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let spec_path = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(default_spec_path);
    let spec = load_spec(&spec_path).map_err(io::Error::other)?;

    let participant = UiDemoParticipant::new(spec);
    let environment = TodoEnvironment::new();
    let mut orchestrator = Orchestrator::new(participant, environment);

    if let Some(output) = render_run_output(&orchestrator.run(run_request("render")).await?) {
        println!("{output}");
    }

    println!("Commands: render | set <node_id> <value> | click <node_id> | quit");

    let stdin = io::stdin();
    loop {
        print!("ui-demo> ");
        io::stdout().flush()?;

        let mut line = String::new();
        if stdin.read_line(&mut line)? == 0 {
            break;
        }
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if matches!(line, "quit" | "exit") {
            break;
        }

        let result = orchestrator.run(run_request(line)).await?;
        if let Some(output) = render_run_output(&result) {
            println!("{output}");
        } else {
            println!("No UI output produced. Finish reason: {:?}", result.finish_reason);
        }
    }

    Ok(())
}
