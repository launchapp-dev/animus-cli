# State Management

## The `.ao/` Directory

All AO runtime state lives under `.ao/` at the project root. This directory is CLI-managed -- you should never hand-edit the JSON files inside it. Use `ao` commands or [MCP tools](./mcp-tools.md) to make changes.

```
.ao/
├── config.json              # Project configuration (MCP servers, tokens)
├── state/
│   ├── core-state.json      # Tasks, requirements, project metadata
│   ├── vision.json          # Vision document
│   ├── agent-runtime-config.v2.json  # Agent profiles, model overrides
│   └── schedule-state.json  # Cron schedule state
├── workflows/               # YAML workflow definitions
│   ├── builtin/             # Built-in workflows (vision, requirements)
│   └── custom/              # User-defined workflows
├── docs/                    # Generated documentation artifacts
├── requirements/            # Requirement details and acceptance criteria
├── execution/               # Workflow execution records and logs
└── daemon.log               # Daemon process log (when running autonomous)
```

---

## State File Categories

### Core State

`core-state.json` is the primary state file. It contains tasks, requirements, project metadata, and workflow references. All reads and writes to this file go through the `ServiceHub` trait, which provides locking and atomic write guarantees.

### Agent Runtime Config

`agent-runtime-config.v2.json` defines agent profiles and model routing overrides. Agent profiles set `model` and `tool` fields that override the compiled defaults in `protocol/src/model_routing.rs`. Setting a field to `null` lets the compiled default take over.

### Workflow Definitions

YAML files under `.ao/workflows/` define workflow pipelines, agents, phases, and post-success actions. See [Workflows](./workflows.md).

### Execution Records

Each workflow run produces execution records under `.ao/execution/`. These contain phase-by-phase results, timing, verdicts, and output artifacts.

---

## Atomic Writes

AO uses atomic writes for all state persistence to prevent corruption from crashes or concurrent access:

1. Write the new state to a temporary file in the same directory.
2. `fsync` the temporary file to ensure data is flushed to disk.
3. Rename the temporary file to the target path (atomic on POSIX systems).

This pattern is implemented in `write_json_atomic` (and related helpers in `orchestrator-store`). A partial write or crash during step 1 or 2 leaves the original file intact.

File-level locking (`fs2::FileExt`) is used for concurrent access protection when multiple processes (e.g. daemon and CLI) may write to the same state file.

---

## Repository Scoping

AO supports multiple projects on the same machine. Each project's runtime state is scoped to avoid collisions.

### Project-local state

The `.ao/` directory at the project root holds project-specific configuration and workflow definitions.

### Global scoped state

Worktrees, execution artifacts, and other runtime data live under:

```
~/.ao/<repo-scope>/
```

Where `<repo-scope>` is computed as:

```
<sanitized-repo-name>-<sha256-hash-prefix>
```

For example, a project at `/Users/alice/my-saas` might have scope `my-saas-a1b2c3d4e5f6`. The hash is computed from the canonical (resolved symlinks) absolute path, using the first 6 bytes (12 hex characters) of the SHA-256 digest.

This means two checkouts of the same repo at different paths get separate scoped state, and there are no collisions between projects with the same directory name.

### Global configuration

Machine-wide settings (agent runner tokens, global MCP servers) live at:

- macOS: `~/Library/Application Support/com.launchpad.agent-orchestrator/`
- Linux: `~/.config/agent-orchestrator/`

The `AO_CONFIG_DIR` environment variable can override this path.

---

## Mutation Policy

State changes must go through validated surfaces:

| Surface | Example |
|---------|---------|
| CLI commands | `ao task status --id TASK-001 --status done` |
| MCP tools | Agent calls `ao.task.update` |
| Projectors | Daemon emits execution fact, task projector updates status |

Never edit `.ao/*.json` files by hand. The CLI applies validation, status transition rules, and side effects (e.g. clearing `paused`/`blocked` flags when a task transitions to `ready`) that hand-edits would skip.

The `ServiceHub` trait enforces this boundary. `FileServiceHub` is the production implementation that reads and writes to disk. `InMemoryServiceHub` is the test implementation that operates on in-memory state.
