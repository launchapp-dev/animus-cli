# orchestrator-store

## Purpose
Provides persistence abstractions for AO repository-local and workspace state.

## Responsibilities
- Implement durable storage for documents and run artifacts.
- Abstract file-backed state access patterns used by runtime services.
- Handle indexing and retrieval of task/requirement state files.

## Key Interfaces
- Repository-local store adapters used by orchestrator services.
- APIs for loading, saving, and indexing AO domain records.

## Local Structure
- `Cargo.toml`: storage backend and serialization dependencies.
- `src/`: store layer and persistence utilities.

## Notes
This crate underpins `.ao` state operations for most workflows.
