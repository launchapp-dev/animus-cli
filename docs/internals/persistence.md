# Persistence

AO persists all state as JSON files using atomic writes. The `orchestrator-store` crate provides the low-level primitives, while `orchestrator-core` builds domain-specific persistence on top.

## Atomic Writes

The core persistence function is `write_json_atomic()` in `crates/orchestrator-store/src/lib.rs`:

1. Create the parent directory if it does not exist
2. Serialize the value to pretty-printed JSON bytes
3. Write to a temporary file in the same directory (via `tempfile::NamedTempFile`)
4. Flush and `fsync` the temporary file to ensure data reaches disk
5. Atomically rename the temporary file to the target path

This approach ensures that readers never see a partially written file. If the process crashes mid-write, the temporary file is left behind and the original remains intact.

On platforms where rename fails because the target already exists, the implementation falls back to removing the target first and retrying the rename.

```rust
pub fn write_json_atomic<T: Serialize>(path: &Path, value: &T) -> Result<()>
```

Additional helpers:
- `write_json_pretty()` -- Alias for `write_json_atomic` (both produce pretty-printed output)
- `write_json_if_missing()` -- Only writes if the file does not already exist (used for bootstrapping defaults)
- `read_json_or_default()` -- Reads and deserializes, returning `T::default()` if the file is absent

## File Locking

Concurrent access from multiple CLI invocations or daemon ticks is protected by file locking via the `fs2` crate (`FileExt` trait). The `FileServiceHub` acquires locks when loading state for mutation (`load_core_state_for_mutation`) to prevent lost updates.

## Scoped Directories

Each project's state is stored under a scoped root to avoid conflicts between projects:

```
~/.ao/<repo-scope>/
  state/
    core-state.json            # Projects, tasks, daemon status, logs
    workflow-config.compiled.json  # Compiled workflow definitions
    agent-runtime-config.v2.json   # Agent profiles, tool configs
    state-machines.v1.json         # State machine definitions
    schedule-state.json            # Schedule last-run timestamps
    model-quality-ledger.json      # Model phase outcome tracking
    errors.json                    # Error records
    history.json                   # Execution history
    reviews.json                   # Review records
    qa-results.json                # QA gate results
    ...
  workflows/
    <workflow-id>.json            # Individual workflow state files
  dispatch-queue/
    dispatch-queue.json           # Daemon dispatch queue state
  output/
    <run-id>/                     # Per-run output artifacts
```

The `<repo-scope>` is derived from the project root path using `protocol::scoped_state_root()`, which hashes the path to create a unique directory name under `~/.ao/`. This allows multiple projects to coexist without collisions.

For projects that use `.ao/` directly within the project root (legacy mode), state is stored at `<project-root>/.ao/state/`.

## JSON Schemas

All persisted state files use JSON with serde serialization. Key schemas:

| File | Primary Type | Description |
|------|-------------|-------------|
| `core-state.json` | `CoreState` | Projects, tasks, daemon status, logs |
| `workflow-config.compiled.json` | `WorkflowConfig` | Compiled workflow definitions with phases |
| `agent-runtime-config.v2.json` | `AgentRuntimeConfig` | Agent profiles, tool configs, phase execution definitions |
| `state-machines.v1.json` | `StateMachinesDocument` | Workflow and requirement lifecycle state machines |
| `<workflow-id>.json` | `OrchestratorWorkflow` | Individual workflow state including phase executions |
| `dispatch-queue.json` | `DispatchQueueState` | Ordered dispatch queue entries with status |

Schema versioning is embedded in file names (e.g., `v2`, `v1`) to support future migrations.

## Workflow State Persistence

Individual workflows are stored as separate files under the `workflows/` directory rather than inline in `core-state.json`. This design:

- Reduces contention on the core state file during concurrent workflow execution
- Allows workflow state to grow independently without bloating the main state file
- Enables the `WorkflowStateManager` to load/save individual workflows efficiently

The `FileServiceHub` constructor loads all workflow files at startup and merges them into the in-memory `CoreState` for fast access.
