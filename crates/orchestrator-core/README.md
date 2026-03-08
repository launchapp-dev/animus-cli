# orchestrator-core

## Purpose
Houses core domain logic, business rules, and central service wiring.

## Responsibilities
- Maintain core AO state management abstractions.
- Coordinate service creation/injection (for example `FileServiceHub`).
- Enforce non-UI domain invariants used by CLI and workflow engines.

## Key Interfaces
- Core service traits and implementations in domain modules.
- Shared utilities for state transitions and validation.

## Local Structure
- `Cargo.toml`: workspace dependency entry.
- `src/`: domain models, services, and orchestrator behavior.

## Notes
Most higher-level crates compose their logic through this crate.
