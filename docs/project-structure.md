# Project Structure Note

The recommended setup for `pera` is a single repository organized as a Rust workspace, not multiple repos.

This gives the project one source of truth for the trusted execution model while still allowing internal separation between stable abstractions, runtime orchestration, CLI tooling, and future bindings. It also keeps versioning and refactoring simple while the architecture is still settling.

## Recommendation

Start with a small workspace centered on three crates:

1. `core`
   The trusted domain model and kernel-facing abstractions.
2. `runtime`
   The orchestration layer that composes the core interfaces into an executable system.
3. `cli`
   A development and testing entrypoint for local execution, inspection, and debugging.

This is the right initial cut because those three parts reflect the clearest architectural boundaries today:

- `core` defines what the system is.
- `runtime` defines how the system runs.
- `cli` defines how developers exercise it.

Adapters should come later, after the trait boundaries are proven by real use.

## Initial Workspace Shape

The initial repository layout should look roughly like this:

```text
pera/
  Cargo.toml
  crates/
    core/
    runtime/
    cli/
  docs/
```

## Full Suggested Target Structure

The longer-term structure can expand into a workspace like this:

```text
pera/
  Cargo.toml
  crates/
    core/
    runtime/
    cli/
    skill-registry/
    sandbox/
    storage/
    queue/
  bindings/
    python/
    js/
  docs/
```

With that structure:

- `core`
  Owns the trusted execution model, domain types, traits, action schema model, event model, and suspend/resume contracts.
- `runtime`
  Owns orchestration, run lifecycle management, action scheduling, interpreter interaction, and composition of the active implementations.
- `cli`
  Owns developer-facing commands for local testing, inspection, fixture execution, and manual validation.
- `skill-registry`
  Can later own skill bundle loading, indexing, validation, and metadata resolution.
- `sandbox`
  Can later own sandbox backend traits and backend-neutral dispatch contracts.
- `storage`
  Can later own persistence traits and shared storage-facing abstractions.
- `queue`
  Can later own scheduler and queue abstractions that are operational rather than authoritative state.
- `bindings/python`
  Can later expose a stable Rust library surface to Python.
- `bindings/js`
  Can later expose a stable Rust library surface to JavaScript.

## Why Not Add Adapters Yet

Adapters should not be forced into the workspace until there is a clearer packaging and release strategy.

That is the right restraint here because:

- you may not want filesystem, cloud, in-memory, and future queue implementations bundled together
- adapter crates often bring heavy and conflicting dependencies
- bindings usually want a narrower stable surface than internal implementations do
- the correct split between traits and concrete implementations becomes clearer only after the first runtime path exists

For now, the system should prove the architecture with `core`, `runtime`, and `cli`. Once those boundaries are exercised, adapter crates can be introduced selectively, either inside the same workspace or as separate packages if distribution concerns justify that split.

## Practical Rule

Use one repo and one workspace from the beginning, but keep the crate graph small until the abstractions harden.

The first milestone should be:

1. `core` defines the trusted model and interfaces.
2. `runtime` executes a minimal end-to-end run loop against those interfaces.
3. `cli` drives that loop for testing and development.

Everything else can follow from that once the kernel boundary is real.
