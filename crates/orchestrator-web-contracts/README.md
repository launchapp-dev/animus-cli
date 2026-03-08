# orchestrator-web-contracts

## Purpose
Defines shared types between the web API and web server layers.

## Responsibilities
- Export API request/response contracts.
- Provide schema-like stable data models for web endpoints.
- Reduce cross-crate drift between backend and frontend-facing interfaces.

## Key Interfaces
- Contract models consumed by `orchestrator-web-api` and `orchestrator-web-server`.
- JSON-serializable DTOs for frontend integrations.

## Local Structure
- `Cargo.toml`: serialization and type utilities.
- `src/`: shared contract type definitions.

## Notes
Keep contract changes version-conscious where web compatibility matters.
