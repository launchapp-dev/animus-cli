# orchestrator-git-ops

## Purpose
Provides safe, reusable Git operations used by AO runtime workflows.

## Responsibilities
- Execute Git actions needed for task/worktree lifecycle.
- Resolve branches, remotes, and repository root metadata.
- Provide helpers for status checks and conflict-safe updates.

## Key Interfaces
- Git utility APIs used by orchestration and worktree management flows.

## Local Structure
- `Cargo.toml`: git bindings and process helpers.
- `src/`: command execution and repository helpers.

## Notes
This crate is used where deterministic repository manipulations are required.
