# orchestrator-daemon-runtime

## Purpose
Provides runtime primitives for daemon-backed execution workflows.

## Responsibilities
- Define the in-process runtime environment for daemon tasks.
- Handle startup/shutdown semantics for background workflow execution.
- Coordinate event streaming and result propagation for daemon workers.

## Key Interfaces
- Runtime scaffolding used by daemon orchestration and agent execution paths.

## Local Structure
- `Cargo.toml`: async runtime and telemetry dependencies.
- `src/`: runtime state, worker orchestration, event flow utilities.

## Notes
Used when AO needs long-lived execution beyond one-off CLI invocations.
