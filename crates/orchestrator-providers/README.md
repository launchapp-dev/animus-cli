# orchestrator-providers

## Purpose
Defines and wires external provider adapters used by AO services.

## Responsibilities
- Abstract external provider behavior behind common trait interfaces.
- Manage provider-specific configuration and routing selection.
- Keep provider implementations isolated from domain orchestration logic.

## Key Interfaces
- Provider traits and adapters consumed by core and runtime orchestration flows.

## Local Structure
- `Cargo.toml`: provider SDK and utility dependencies.
- `src/`: trait definitions and concrete provider implementations.

## Notes
Prefer small adapter modules per provider to minimize coupling and simplify replacement.
