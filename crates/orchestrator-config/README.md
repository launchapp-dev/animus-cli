# orchestrator-config

## Purpose
Centralizes AO configuration loading, parsing, and precedence handling.

## Responsibilities
- Define config models and default values.
- Resolve config across project and global scopes.
- Normalize env/CLI overrides into strongly typed configuration.

## Key Interfaces
- Configuration structs and helpers consumed by CLI and daemon startup.
- Validation and migration helpers for configuration evolution.

## Local Structure
- `Cargo.toml`: configuration and serialization dependencies.
- `src/`: config parsing, path resolution, defaults.

## Notes
Use this crate to avoid duplicating config precedence logic in calling layers.
