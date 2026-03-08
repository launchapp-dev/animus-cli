# workflow-runner

## Purpose
Executes workflow graphs/phase sequences as part of AO task and agent pipelines.

## Responsibilities
- Run and coordinate workflow phases.
- Manage state transitions and checkpoint progression.
- Produce execution artifacts and status for monitoring.

## Key Interfaces
- Workflow runner APIs used by CLI and daemon entrypoints.
- Event emission hooks for runtime observability.

## Local Structure
- `Cargo.toml`: execution and async orchestration dependencies.
- `src/`: workflow state machine and runtime execution code.

## Notes
This crate should remain resilient to phase-level failures and provide clear retry/fail-fallback behavior.
