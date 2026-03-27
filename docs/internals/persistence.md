# Persistence

AO persists state with a mix of atomic JSON files and a repo-scoped SQLite database.

## Atomic JSON Writes

The low-level JSON helpers live in `crates/orchestrator-store/src/lib.rs`:

- `write_json_atomic()`
- `write_json_pretty()`
- `write_json_if_missing()`
- `read_json_or_default()`

`write_json_atomic()` writes to a temporary file in the target directory, flushes and syncs it, then renames it into place so readers never observe a partially written JSON file.

## Scoped Runtime Root

Runtime state is scoped per repository under:

```text
~/.ao/<repo-scope>/
```

The scope name is derived from the canonical project path and includes a sanitized repo name plus a 12-hex SHA-256 prefix.

## Key Stores

```text
~/.ao/<repo-scope>/
├── core-state.json
├── resume-config.json
├── workflow.db
├── config/state-machines.v1.json
├── daemon/pm-config.json
├── state/
└── worktrees/
```

### `core-state.json`

The shared runtime snapshot AO loads into memory at startup.

### `workflow.db`

SQLite database that stores:

- workflows
- checkpoints
- tasks
- requirements

The database uses WAL mode and a short busy timeout to support concurrent access patterns during CLI and daemon activity.

### `state/`

JSON stores for operational records such as:

- `pack-selection.v1.json`
- `schedule-state.json`
- `reviews.json`
- `handoffs.json`
- `history.json`
- `errors.json`
- `qa-results.json`
- `qa-review-approvals.json`

## File Locking

`FileServiceHub` uses file locking around `core-state.json` mutations to avoid lost updates when multiple AO processes operate on the same repository scope.

## Migration Behavior

AO still contains migration helpers for older layouts:

- repo-local `.ao/` state can be migrated to `~/.ao/<repo-scope>/`
- legacy workflow JSON files can be migrated into `workflow.db`
- older `state/state-machines.v1.json` can be moved to `config/state-machines.v1.json`

Those fallbacks exist for compatibility. New features should target the scoped runtime layout.
