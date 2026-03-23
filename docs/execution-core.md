# Execution Core Spec

`pera` is a lightweight embeddable execution core for agentic systems. It sits between an external code-generating agent or LLM and an external safe interpreter, providing the trusted boundary that defines what generated programs can see and do.

In one sentence: it is a small trusted execution substrate for agent-generated programs, combining environment API, sandboxed action dispatch, stateful suspend/resume orchestration, and event streaming.

## System Responsibilities

The core system owns five responsibilities:

1. `skill/module registry`
   Load immutable skill bundles made of sandboxed executables plus metadata, schemas, API stubs, and policy requirements.
2. `execution contract`
   Expose the environment API visible to prompts and generated code, including typed actions and data normalization rules.
3. `orchestration kernel`
   Start and resume code runs, ask the interpreter for the next boundary crossing, schedule code work and action work, and manage suspend and resume.
4. `sandbox dispatch`
   Execute actions through pluggable sandbox backends, with WASM as the first backend and room for microVM, container, and process backends later.
5. `state and events`
   Persist run and action state with minimal footprint and emit structured lifecycle events back to the host agent or UI.

## Trust Model

The trust boundary is explicit:

- The agent or LLM is replaceable and untrusted for correctness.
- The compiler or interpreter is replaceable and does not own side effects.
- The core system is the trusted authority for action schemas, authorization, sandbox wiring, and effect mediation.

## Execution Model

The execution flow is:

1. The host submits code plus selected skills.
2. The core exposes the corresponding environment API to the agent or interpreter.
3. The interpreter runs until completion or until an external action must occur.
4. The core records that action, schedules it, executes it in a sandbox, stores the result, and resumes code execution.
5. The core emits semantic events such as `run_started`, `action_scheduled`, `waiting_input`, `action_completed`, and `run_completed`.

## Storage Model

The storage model is intentionally lightweight:

- Skills are stored as filesystem bundles with an in-memory index by default.
- Run and action state is stored in memory, files, or SQLite behind replaceable interfaces.
- Runnable queue and scheduler state is operational and derived, not the source of truth.

## Extension Model

The system is extensible along three axes:

- Skills extend the system with new high-level actions.
- Capability providers are lower-level privileged services exposed to sandboxes when authorized.
- Sandbox backends are pluggable and backend-neutral at the kernel boundary.

## Non-Goals

The project is intentionally narrow in scope:

- It is not an LLM framework.
- It is not a compiler.
- It is not a general workflow engine.
- It is not tied to WASM, Python, or any single transport.
