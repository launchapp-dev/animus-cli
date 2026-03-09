# Data Layout

The `.ao/` directory contains all AO-managed state for a project. These files are written atomically (temp file + rename) and should never be hand-edited. Use `ao` commands to modify state.

---

## Directory Structure

```
.ao/
├── config.json                          # Project configuration
├── core-state.json                      # Core orchestrator state
├── resume-config.json                   # Daemon resume configuration
├── daemon.log                           # Daemon process log (rotated at 10MB to .log.1)
├── daemon.pid                           # Daemon process ID file
│
├── docs/
│   ├── vision.json                      # Project vision document
│   ├── requirements.json                # Legacy requirements document
│   ├── tasks.json                       # Legacy tasks document
│   └── architecture.json               # Architecture graph document
│
├── requirements/
│   ├── index.json                       # Requirements index (id, title, status, priority)
│   └── generated/
│       └── *.json                       # Generated requirement detail files
│
├── tasks/
│   ├── index.json                       # Task index (id, title, status, priority)
│   └── TASK-*.json                      # Individual task files (one per task)
│
├── state/
│   ├── workflow-config.v2.json          # Compiled workflow configuration
│   ├── agent-runtime-config.v2.json     # Agent profiles and model/tool settings
│   ├── state-machines.v1.json           # State machine definitions
│   ├── reviews.json                     # Review decisions
│   ├── handoffs.json                    # Role handoff records
│   ├── history.json                     # Execution history
│   ├── errors.json                      # Error tracking records
│   ├── qa-results.json                  # QA evaluation results
│   └── qa-review-approvals.json         # QA gate approval records
│
├── workflows/
│   └── *.yaml                           # YAML workflow source files
│
├── runs/
│   └── <run_id>/
│       └── events.jsonl                 # Agent run event log (JSONL format)
│
└── artifacts/
    └── <execution_id>/
        └── ...                          # Files generated during agent execution
```

---

## File Descriptions

### Core Files

| File | Description |
|---|---|
| `config.json` | Project-level configuration created during `ao setup` |
| `core-state.json` | Central orchestrator state including workflow instances and scheduling data |
| `resume-config.json` | State needed to resume the daemon after restart |
| `daemon.log` | Daemon process stderr log. Rotated to `daemon.log.1` at 10MB. Structured JSON lines for startup/shutdown events. |
| `daemon.pid` | PID of the running daemon process |

### Documents (docs/)

| File | Description |
|---|---|
| `docs/vision.json` | Project vision drafted via `ao vision draft` |
| `docs/requirements.json` | Legacy requirements document (from `ao requirements draft`) |
| `docs/tasks.json` | Legacy tasks document |
| `docs/architecture.json` | Architecture graph (entities and edges) |

### Requirements (requirements/)

| File | Description |
|---|---|
| `requirements/index.json` | Index of all requirements with id, title, status, priority |
| `requirements/generated/*.json` | Individual generated requirement detail files |

### Tasks (tasks/)

| File | Description |
|---|---|
| `tasks/index.json` | Index of all tasks with id, title, status, priority |
| `tasks/TASK-*.json` | Individual task files. Each contains full task data including description, checklist, dependencies, metadata, and workflow history. |

### State (state/)

| File | Description |
|---|---|
| `state/workflow-config.v2.json` | Compiled from `.ao/workflows/*.yaml`. Contains phases, workflows, MCP servers, agent profiles. Schema: `ao.workflow-config.v2`. |
| `state/agent-runtime-config.v2.json` | Agent profiles with model, tool, system prompt settings. Overrides compiled defaults. |
| `state/state-machines.v1.json` | State machine definitions for workflow phase transitions |
| `state/reviews.json` | Review decision records (from `ao review record`) |
| `state/handoffs.json` | Role handoff records (from `ao review handoff`) |
| `state/history.json` | Execution history records |
| `state/errors.json` | Error tracking records (from `ao errors`) |
| `state/qa-results.json` | QA evaluation results (from `ao qa evaluate`) |
| `state/qa-review-approvals.json` | QA gate approval records (from `ao qa approval`) |

### Runs (runs/)

Each agent run produces a directory named by its run ID containing:

| File | Description |
|---|---|
| `runs/<run_id>/events.jsonl` | Line-delimited JSON event log. Each line is an `AgentRunEvent` with timestamp, event type, and payload. |

### Artifacts (artifacts/)

Files generated during agent execution are stored under the execution ID:

| Path | Description |
|---|---|
| `artifacts/<execution_id>/` | Directory containing any files produced by the agent during that execution |

---

## Worktree State

Worktree metadata is stored outside the project directory to avoid conflicts with git:

```
~/.ao/<repo-scope>/worktrees/
```

The `<repo-scope>` is derived from the repository path to create a unique, filesystem-safe identifier. Worktree state tracks branch names, task associations, and sync status.

---

## Atomic Writes

All state files are written atomically using the temp-file-and-rename pattern (`write_json_atomic`). This prevents corruption from interrupted writes or concurrent access.

---

## Daemon Event Log

The daemon also writes structured events to a separate event log:

```
<project_root>/.ao/daemon-events.jsonl
```

This log is used by `ao daemon events` and the `ao://project/daemon-events` MCP resource.

See also: [Configuration](configuration.md), [Status Values](status-values.md).
