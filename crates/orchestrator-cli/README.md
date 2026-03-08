# orchestrator-cli

## Purpose
Main `ao` command-line application and CLI command dispatcher.

## Responsibilities
- Parse command-line inputs with clap.
- Resolve runtime context (project root, output format, config overlays).
- Route commands to orchestrator services and workflow/runtime handlers.
- Provide human-readable and JSON output envelopes.

## Key Interfaces
- `main.rs` entrypoint.
- Modular `cli_types` definitions for each command domain.
- Output/error contracts for stable CLI responses.

## Local Structure
- `Cargo.toml`: binary metadata and CLI dependencies.
- `src/main.rs`: bootstrap and command routing.
- `src/cli_types/`: domain command parameter types.
- `src/shared/`: shared CLI helpers and output adapters.

## Notes
This crate is the primary user-facing surface for AO.
