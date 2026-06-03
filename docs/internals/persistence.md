# Persistence

Animus persists state with a mix of atomic JSON files and a repo-scoped SQLite database.

## Atomic JSON Writes

The low-level JSON helpers live in `crates/orchestrator-core/src/store/mod.rs` (folded in from the former `orchestrator-store` crate in v0.5.3):

- `write_json_atomic()`
- `write_json_pretty()`
- `write_json_if_missing()`
- `read_json_or_default()`

`write_json_atomic()` writes to a temporary file in the target directory, flushes and syncs it, then renames it into place so readers never observe a partially written JSON file.

## Scoped Runtime Root

Runtime state is scoped per repository under:

```text
~/.animus/<repo-scope>/
```

The scope name is derived from the canonical project path and includes a sanitized repo name plus a 12-hex SHA-256 prefix.

## Key Stores

```text
~/.animus/<repo-scope>/
├── core-state.json
├── resume-config.json
├── workflow.db
├── config/
│   ├── state-machines.v1.json
│   ├── workflow-config.v2.json
│   └── agent-runtime-config.v2.json
├── daemon/pm-config.json
├── docs/
├── logs/
├── runner/
├── state/
└── worktrees/
```

### `core-state.json`

The shared runtime snapshot Animus loads into memory at startup.

### `workflow.db`

SQLite database that stores:

- workflows
- checkpoints
- tasks
- requirements

The database uses WAL mode and a short busy timeout to support concurrent access patterns during CLI and daemon activity.

### `config/`

Compiled runtime configuration lives under `config/`:

- `state-machines.v1.json`
- `workflow-config.v2.json`
- `agent-runtime-config.v2.json`

These files are generated runtime state, not hand-authored project config.

### `logs/` and `runner/`

- `daemon/pm-config.json` stores persisted daemon automation settings
- `logs/events.jsonl` stores redacted structured runtime events for the repo scope
- `runner/config.json` stores scoped runner config, including the runner auth token
- `runner/agent-runner.sock` is the default scoped Unix socket path for runner clients

### `state/`

JSON stores for operational records such as:

- `pack-selection.v1.json`
- `schedule-state.json`
- `handoffs.json`
- `history.json`
- `errors.json`
- `agent-handoffs/<workflow-id>/<root-run-id>.jsonl`

The `state/` directory is intentionally open-ended: new runtime stores may be
added over time, but the current domain-state JSON helpers back `handoffs`,
`history`, and `errors`.

## File Locking

`FileServiceHub` uses file locking around `core-state.json` mutations to avoid lost updates when multiple Animus processes operate on the same repository scope.

## Migration Behavior

Animus still contains migration helpers for older layouts:

- repo-local `.animus/` state can be migrated to `~/.animus/<repo-scope>/`
- legacy workflow JSON files can be migrated into `workflow.db`
- older `state/state-machines.v1.json` can be moved to `config/state-machines.v1.json`

Those fallbacks exist for compatibility. New features should target the scoped runtime layout.
