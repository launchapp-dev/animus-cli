# agent-runner

## Purpose
Runs agent worker processes as a standalone daemon and exposes an IPC interface for orchestrating tool-llm execution.

## Responsibilities
- Boot and supervise long-running runner processes.
- Launch and monitor LLM CLI workers.
- Stream execution events back to orchestrator components.
- Emit process lifecycle and error signals for recovery.

## Key Interfaces
- Runtime control entry points for runner lifecycle.
- IPC transport contracts consumed by orchestrator and CLI runtime glue.

## Local Structure
- `Cargo.toml`: crate metadata and dependencies.
- `src/`: runtime implementation and protocol handlers.

## Notes
This crate is a runtime dependency for local and daemon-based agent execution.
