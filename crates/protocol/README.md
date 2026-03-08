# protocol

## Purpose
Defines shared wire protocol types and contracts used across AO crates.

## Responsibilities
- Store canonical protocol types shared between CLI, daemon, and web layers.
- Keep serialization-compatible data models stable across crate boundaries.
- Provide a single source of protocol truth for API and IPC contracts.

## Key Interfaces
- `protocol` module exports reusable type definitions.
- Versioned enums/structs for state, commands, and events.

## Local Structure
- `Cargo.toml`: crate metadata and shared dependencies.
- `src/`: protocol type definitions.

## Notes
Downstream crates should prefer consuming protocol types from here instead of defining duplicate contracts.
