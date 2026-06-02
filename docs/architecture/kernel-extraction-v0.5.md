# Kernel Extraction Map — v0.5

## Status

- **Version:** v0.5 lift-and-shift instructions
- **Audience:** Wave 2A agents extracting Rust crates into plugin repos
- **Companion:** [v0.5-protocol-specs.md](./v0.5-protocol-specs.md) (the RPC contracts the lifted code adapts to)
- **Discipline anchor:** [kernel-and-flavors.md](./kernel-and-flavors.md)

## TL;DR

Two Rust crates lift from `ao-cli` into new plugin repos under `launchapp-dev`. The lift is **mechanical** — existing code is proven, this is a packaging change with stdio RPC adaptation, not a redesign. Each agent's job: clone the source, wrap with `animus-plugin-runtime`, expose the existing API as RPCs per the protocol spec, port tests, ship `v0.1.0-dev`.

| Source crate / module | Target plugin repo | Lift type |
|---|---|---|
| `crates/workflow-runner-v2/` (entire crate) | `launchapp-dev/animus-workflow-runner-default` | Full crate lift + RPC wrapper |
| `crates/orchestrator-daemon-runtime/src/queue/` + `src/dispatch/dispatch_support.rs` + parts of `src/control/dispatch.rs` | `launchapp-dev/animus-queue-default` | Module extraction + RPC wrapper |

---

## Extraction 1: `animus-workflow-runner-default`

### Source files (in `ao-cli`)

**Primary crate:** `crates/workflow-runner-v2/` — lift the entire crate.

```
crates/workflow-runner-v2/
├── Cargo.toml                       # adjust dependencies for plugin repo
├── src/
│   ├── lib.rs                       # main public API
│   ├── main.rs                      # KEEP — becomes the plugin binary entrypoint
│   ├── workflow_execute.rs          # workflow/execute method implementation
│   ├── phase_executor.rs            # workflow/run-phase method implementation
│   ├── phase_output.rs              # marker files, decision persistence
│   ├── phase_command.rs             # command-mode phase support
│   ├── phase_git.rs                 # git ops invocation
│   ├── phase_session.rs             # session resumption
│   ├── phase_failover.rs            # error recovery
│   ├── phase_prompt.rs              # prompt construction
│   ├── phase_targets.rs             # phase target resolution
│   ├── payload_traversal.rs         # parse PhaseDecision from agent output
│   ├── workflow_helpers.rs          # research phase, completion detection
│   ├── workflow_event_emitter.rs    # ⚠️ REMOVE Arc<dyn …>; events return in result
│   ├── workflow_merge_recovery.rs   # merge conflict recovery
│   ├── notification_log.rs          # event log durability
│   ├── metrics_hook.rs              # metrics observation
│   ├── ipc.rs                       # ⚠️ REPLACE with animus-plugin-runtime adapter
│   ├── agent_state.rs               # agent state tracking
│   ├── config_context.rs            # config loading
│   ├── direct_exec.rs               # direct execution mode
│   ├── ensure_execution_cwd.rs      # cwd setup
│   ├── runtime_contract.rs          # phase contract validation
│   ├── runtime_support.rs           # runtime helpers
│   └── skill_dispatch.rs            # skill dispatch
└── tests/
    ├── durability_idempotency_and_markers.rs        # ✅ port as-is
    ├── durability_manual_pending_and_failed_events.rs # ✅ port as-is
    ├── notification_log.rs                          # ✅ port as-is
    ├── phase_duration_histogram_records_observations.rs # ✅ port as-is
    └── session_resume.rs                            # ✅ port as-is
```

### Dependencies to update in plugin repo's `Cargo.toml`

Each new plugin repo is a **standalone Cargo project**, NOT part of any external workspace. All dependencies are pulled from crates.io (preferred) or pinned by git tag (acceptable for the pre-v0.5.0 development window). **Do not use `workspace = true` for cross-repo dependencies — that resolves to nothing in a standalone repo.** (Fixes codex P1-7.)

**Keep as-is (move with crate):**
- `serde`, `serde_json`, `tokio`, `tracing`, `chrono`, `anyhow`, `thiserror`

**Add for plugin shell** (use git-tagged dependencies until the protocol workspace publishes v0.5.0 to crates.io):

```toml
[dependencies]
animus-plugin-protocol      = { git = "https://github.com/launchapp-dev/animus-protocol", tag = "v0.5.0" }
animus-plugin-runtime       = { git = "https://github.com/launchapp-dev/animus-protocol", tag = "v0.5.0" }
animus-workflow-runner-protocol = { git = "https://github.com/launchapp-dev/animus-protocol", tag = "v0.5.0" }
animus-subject-protocol     = { git = "https://github.com/launchapp-dev/animus-protocol", tag = "v0.5.0" }
```

After publishing to crates.io (release work, not v0.5 swarm scope), the git deps become `animus-plugin-protocol = "1.1"` etc.

**Drop:**
- Direct dependency on `orchestrator-cli` (none should exist; verify)
- `orchestrator-daemon-runtime` (the plugin doesn't drive the loop; the daemon does)

**The hard truth about workflow-runner-v2's existing deps (fixes codex round-2 P1-4):**

`workflow-runner-v2` currently depends on multiple ao-cli crates that have not been factored into protocol-friendly shapes. Specifically:

- `protocol` — shared types (PhaseRoutingConfig, McpRuntimeConfig, etc.)
- `orchestrator-core` — `FileServiceHub`, `OrchestratorWorkflow`, `WorkflowStatus`, `PhaseDecision`
- `orchestrator-config` — workflow YAML loading and compilation
- `orchestrator-store` — persistence
- `agent-runner` — agent process invocation
- `orchestrator-providers`, `orchestrator-git-ops`, `orchestrator-notifications`, `orchestrator-logging`

**Vendoring all of this is not realistic for v0.5.** These crates are deeply intertwined with the kernel and carry their own subdependencies. The honest plan:

1. The plugin repo's `Cargo.toml` depends on `ao-cli`'s crates via git, pinning to a known good commit of the v0.5 branch:

```toml
[dependencies]
# Protocol surface (from animus-protocol repo at v0.5.0)
animus-plugin-protocol      = { git = "https://github.com/launchapp-dev/animus-protocol", tag = "v0.5.0" }
animus-plugin-runtime       = { git = "https://github.com/launchapp-dev/animus-protocol", tag = "v0.5.0" }
animus-workflow-runner-protocol = { git = "https://github.com/launchapp-dev/animus-protocol", tag = "v0.5.0" }
animus-subject-protocol     = { git = "https://github.com/launchapp-dev/animus-protocol", tag = "v0.5.0" }

# Existing ao-cli crates — depended on directly until they're factored into
# publishable shared crates. Pin to the v0.5 branch's commit at plugin-extraction time.
protocol                = { git = "https://github.com/launchapp-dev/animus-cli", branch = "v0.5", package = "protocol" }
orchestrator-core       = { git = "https://github.com/launchapp-dev/animus-cli", branch = "v0.5" }
orchestrator-config     = { git = "https://github.com/launchapp-dev/animus-cli", branch = "v0.5" }
orchestrator-store      = { git = "https://github.com/launchapp-dev/animus-cli", branch = "v0.5" }
agent-runner            = { git = "https://github.com/launchapp-dev/animus-cli", branch = "v0.5" }
orchestrator-providers  = { git = "https://github.com/launchapp-dev/animus-cli", branch = "v0.5" }
orchestrator-git-ops    = { git = "https://github.com/launchapp-dev/animus-cli", branch = "v0.5" }
orchestrator-notifications = { git = "https://github.com/launchapp-dev/animus-cli", branch = "v0.5" }
orchestrator-logging    = { git = "https://github.com/launchapp-dev/animus-cli", branch = "v0.5" }
```

2. The Wave 3 cleanup work does NOT delete these supporting crates from `ao-cli`. Only `crates/workflow-runner-v2/` and `crates/orchestrator-daemon-runtime/src/queue/` are deleted (their code now lives in plugin repos). The other crates stay because both the daemon AND the plugins depend on them.

3. A future release (v0.6+) factors the shared types (PhaseDecision, WorkflowStatus, etc.) into a published `animus-workflow-types` crate, at which point the plugin's git deps can be replaced with crates.io versions.

This is not architecturally clean — it leaves the plugin coupled to `ao-cli`'s release cadence. But it is the **pragmatic shape that ships in v0.5** without a multi-week shared-types refactor. Document this constraint in the plugin repo's README.

### Subject type migration

The lifted code references `protocol::SubjectDispatch` and `protocol::SubjectRef`. As part of the lift, change these references to `animus_subject_protocol::SubjectDispatch` and `animus_subject_protocol::SubjectRef`. The wire format is identical (the v0.5.0 release of `animus-subject-protocol` is the authoritative home; `ao-cli`'s `protocol` crate re-exports from it for backward compat). See `v0.5-protocol-specs.md` §"Subject type ownership" for the rationale. (Fixes codex round-3 P1-3.)

### Surgical changes required

These are the IPC-safety changes from the protocol spec (§1 "IPC mitigations table").

#### 1. Remove `PhaseEventCallback` closure

**Current** (`workflow_execute.rs` or similar):
```rust
pub struct WorkflowExecuteParams {
    // ...
    pub on_phase_event: Option<PhaseEventCallback>,  // ❌ closure — not serializable
}
```

**New**: drop the callback. Accumulate events into a `Vec<PhaseEvent>` field on the return type. Daemon polls separately if it needs real-time events (out of v0.5 scope; deferred to v0.6 `workflow/events/poll`).

#### 2. Remove `Arc<dyn ServiceHub>` from public API

**Current**: `execute_workflow(params, hub: Arc<dyn ServiceHub>)` — the hub is an in-process trait object that can't cross IPC.

**New**: the plugin owns its own service hub. The `project_root` is bound at **initialize-time** via the `init_extensions.project_binding` extension (see `v0.5-protocol-specs.md` Common Conventions) — it is NOT a per-request field on `WorkflowExecuteRequest` or `WorkflowPhaseRunRequest`. The plugin stores the project root in shared plugin state at init:

```rust
struct PluginState {
    project_root: String,
    hub: FileServiceHub,
}

async fn handle_initialize(params: InitializeParams) -> Result<InitializeResult> {
    let binding = params.init_extensions
        .get("project_binding")
        .ok_or("missing project_binding extension")?;
    let project_root = binding["project_root"].as_str().ok_or("invalid project_binding")?.to_string();
    let hub = FileServiceHub::open(&project_root).await?;
    set_plugin_state(PluginState { project_root, hub });
    // ... return InitializeResult with capabilities ...
}

async fn handle_workflow_execute(req: WorkflowExecuteRequest) -> Result<WorkflowExecuteResult> {
    let state = get_plugin_state();
    // ... existing execute_workflow logic using state.hub ...
}
```

#### 3. Replace `workflow_event_emitter::SharedWorkflowEventEmitter`

**Current**: `Arc<dyn WorkflowEventEmitter>` passed through phase execution.

**New**: replace with a simple `Vec<PhaseEvent>` accumulator inside the plugin process. All emission goes to that vector. Plugin returns the vector in the result.

#### 4. Replace stdio frame handling

**Current**: `ipc.rs` likely has its own stdio framing.

**New**: delete `ipc.rs`. Use `animus-plugin-runtime`'s stdio loop — it handles the JSON-RPC envelope, dispatch by method name, and error mapping. Plugin authors register handler functions:

```rust
use animus_plugin_runtime::{Plugin, register_method};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut plugin = Plugin::new("animus-workflow-runner-default", "0.1.0");

    register_method!(plugin, animus_workflow_runner_protocol::METHOD_WORKFLOW_EXECUTE,
        handle_workflow_execute);
    register_method!(plugin, animus_workflow_runner_protocol::METHOD_WORKFLOW_RUN_PHASE,
        handle_workflow_run_phase);

    plugin.run().await
}
```

### Tests to port

All 5 integration tests in `crates/workflow-runner-v2/tests/` and all module-level `mod tests` blocks. Tests use `tempdir` fixtures and don't depend on daemon state — they're plugin-compatible without modification.

Two adjustments needed in the test harness:
1. Tests that construct a `WorkflowExecuteParams` directly need to be updated to construct `WorkflowExecuteRequest` instead.
2. Tests that pass `on_phase_event` callbacks must instead read the returned `phase_events` vector.

### Verification gates (in the plugin repo)

```bash
cargo build
cargo test
cargo clippy -- -D warnings
cargo fmt --check
```

Plus an integration test that:
1. Starts the plugin binary
2. Sends a `workflow/execute` request via stdio JSON-RPC
3. Asserts the result matches a known fixture

---

## Extraction 2: `animus-queue-default`

### Source files (in `ao-cli`)

**Primary modules:** `crates/orchestrator-daemon-runtime/src/queue/` (full directory) plus related dispatch helpers.

```
crates/orchestrator-daemon-runtime/src/queue/
├── mod.rs                          # public surface
├── dispatch_queue_store.rs         # file-locked persistence + queue mutation helpers
└── dispatch_queue_state.rs         # in-memory state types

# Plus from src/dispatch/ — needed for the headroom/throttling reference:
crates/orchestrator-daemon-runtime/src/dispatch/
├── dispatch_support.rs             # ⚠️ STAYS in daemon (capacity is kernel concern)
├── ready_dispatch_plan.rs          # ⚠️ STAYS in daemon (uses queue + fallback picker)
└── dispatch_selection_source.rs    # ⚠️ STAYS in daemon

# Plus from src/control/:
crates/orchestrator-daemon-runtime/src/control/
└── dispatch.rs                     # ⚠️ becomes the daemon-side queue plugin client
```

**Important distinction**: The queue PLUGIN owns: enqueue/list/hold/release/drop/reorder/mark-assigned/completion + the file-locked state. The KERNEL keeps: capacity calculation, dispatch headroom budgeting, ready-dispatch planning, active-workflow filtering. The kernel polls the queue plugin for items and decides how many to lease per tick based on its own capacity logic.

### Dependencies to update

**Keep (move with modules):**
- `serde`, `serde_json`, `tokio`, `tracing`, `chrono`, `anyhow`, `thiserror`, `fs2` (file locking)

**Add** (same standalone-repo dep policy as workflow_runner — no `workspace = true`):

```toml
[dependencies]
animus-plugin-protocol      = { git = "https://github.com/launchapp-dev/animus-protocol", tag = "v0.5.0" }
animus-plugin-runtime       = { git = "https://github.com/launchapp-dev/animus-protocol", tag = "v0.5.0" }
animus-queue-protocol       = { git = "https://github.com/launchapp-dev/animus-protocol", tag = "v0.5.0" }
animus-subject-protocol     = { git = "https://github.com/launchapp-dev/animus-protocol", tag = "v0.5.0" }
```

**Drop:**
- `orchestrator-daemon-runtime` (the plugin is no longer part of the daemon runtime)
- `orchestrator-cli`

### Surgical changes required

#### 1. Extract types

The current modules use types from `orchestrator-daemon-runtime`. Move only what's needed:
- `QueueEntry` → renamed/aligned with protocol's `QueueEntry`
- `QueueState` → internal to plugin
- File-lock helpers → internal to plugin

If the queue references types that live in `orchestrator-core` (e.g., `SubjectDispatch`), keep them via the `animus-subject-protocol` crate dependency.

#### 2. Replace tracing/event hooks

If the queue currently emits events through `orchestrator-notifications`, replace with internal `tracing::info!` calls. The daemon observes via its own log_storage plugin pipeline.

#### 3. File-lock coordination

The current implementation uses `fs2::FileExt::lock_exclusive()` on a `.lock` file co-located with queue state JSON. This stays — the plugin owns the lock for the lifetime of its writes. Document the lock-safety contract in the plugin's README:

> The `animus-queue-default` plugin uses an exclusive filesystem lock during state mutations. The lock is held only across read-modify-write cycles, never across IPC. Multiple plugin instances against the same project root produce undefined behavior; the daemon SHOULD enforce single-plugin-per-project.

#### 4. RPC wrapper

Same pattern as workflow_runner:

```rust
use animus_plugin_runtime::{Plugin, register_method};
use animus_queue_protocol::*;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut plugin = Plugin::new("animus-queue-default", "0.1.0");

    register_method!(plugin, METHOD_QUEUE_ENQUEUE, handle_enqueue);
    register_method!(plugin, METHOD_QUEUE_LIST, handle_list);
    register_method!(plugin, METHOD_QUEUE_LEASE, handle_lease); // atomic dispatch path; required
    register_method!(plugin, METHOD_QUEUE_STATS, handle_stats);
    register_method!(plugin, METHOD_QUEUE_HOLD, handle_hold);
    register_method!(plugin, METHOD_QUEUE_RELEASE, handle_release);
    register_method!(plugin, METHOD_QUEUE_DROP, handle_drop);
    register_method!(plugin, METHOD_QUEUE_REORDER, handle_reorder);
    register_method!(plugin, METHOD_QUEUE_MARK_ASSIGNED, handle_mark_assigned);
    register_method!(plugin, METHOD_QUEUE_COMPLETION, handle_completion);

    plugin.run().await
}
```

### Tests to port

Port the equivalent queue behavior coverage from the current in-tree queue implementation:
- idempotent enqueue for the same task pipeline
- hold / release / reorder behavior for subject-backed entries
- non-task subject enqueue support
- reorder behavior that preserves all entries for a shared subject
- kind-qualified queue identifiers for generic subjects

These use `tempdir` fixtures and are portable directly. Test harness adjustments:
1. Replace `QueueService::new(state_path)` with `QueueBackend::new(state_path)` (or whatever the new internal name is).
2. No callback patterns to update — queue operations are already synchronous.

### Verification gates

Same as workflow_runner: `cargo build && cargo test && cargo clippy && cargo fmt --check` plus a stdio JSON-RPC smoke test.

---

## Kernel-side changes in `ao-cli` (Wave 3)

After Wave 2A merges, the daemon must be updated to call the new plugins instead of the in-tree code.

### Files to modify

| File | Change |
|---|---|
| `crates/orchestrator-daemon-runtime/src/scheduler/...` | Replace in-tree queue calls with `plugin_host.call("queue/enqueue", ...)` etc. |
| `crates/orchestrator-daemon-runtime/src/control/dispatch.rs` | Becomes thin RPC client wrapping `plugin_host.call("queue/...")` |
| `crates/orchestrator-cli/src/services/operations/ops_workflow.rs` (or similar) | Replace direct `workflow_runner_v2::execute_workflow` calls with `plugin_host.call("workflow/execute", ...)` |

### Files to delete after Wave 2A merges and Wave 3 lands

| File | Reason |
|---|---|
| `crates/workflow-runner-v2/` (entire crate) | Now lives in `animus-workflow-runner-default` repo |
| `crates/orchestrator-daemon-runtime/src/queue/` (entire directory) | Now lives in `animus-queue-default` repo |
| `Cargo.toml` workspace member entry for `workflow-runner-v2` | Cleanup |

### Default flavor manifest

Update `flavors/default.toml`:

```toml
[workflow_runner]
required = ["launchapp-dev/animus-workflow-runner-default"]

[queue]
required = ["launchapp-dev/animus-queue-default"]
```

The daemon refuses to start without these two plugins installed (per the preflight pattern already established in v0.4.12).

---

## Sequencing gates

```
Wave 1  ─────────► protocol crates published as v0.5.0
                          │
                          ▼
Wave 2A ─────────► animus-workflow-runner-default v0.1.0-dev
        ─────────► animus-queue-default v0.1.0-dev
        (parallel)
                          │
                          ▼
Wave 2B ─────────► animus-step-durable-dbos v0.1.0-dev
        ─────────► animus-memory-zep v0.1.0-dev
        (parallel with 2A; only depends on protocol crates from Wave 1)
                          │
                          ▼
Wave 3  ─────────► ao-cli v0.5 branch:
                     - Daemon calls plugins instead of in-tree
                     - flavors/default.toml updated
                     - In-tree crates deleted
                     - animus flavor CLI subcommand
                          │
                          ▼
Wave 4  ─────────► Integration tests + demo recordings
```

**Hard dependency**: Wave 2 cannot start until Wave 1's protocol crates are pushed and `v0.5.0` tag exists in the `animus-protocol` repo. The first Wave 2 agent that starts before the tag exists WILL fail to build because the `git = "..." tag = "v0.5.0"` dependency cannot resolve.

**Hard dependency**: Wave 3's daemon refactor (replacing in-tree workflow runner and queue calls with plugin-host calls) requires BOTH Wave 2A plugin repos merged and tagged at `v0.1.0-dev`. Wave 3 attempting to wire only one of the two will produce a branch that does not build. (Fixes codex P1-6.)

**Wave 3 deletion gate**: in-tree code deletion (`crates/workflow-runner-v2/`, `crates/orchestrator-daemon-runtime/src/queue/`) only happens after Wave 3's daemon refactor is merged AND green on CI AND both plugins are installable via `animus plugin install`. Until then, the in-tree code stays as fallback.

**Soft coordination**: Wave 2B (DBOS + Zep plugins) does not depend on Wave 2A. It can start as soon as Wave 1's protocol tag exists.

---

## What this map intentionally does NOT cover

These are out of v0.5 scope and explicitly NOT to be lifted in this round:

- `agent-runner` crate (becomes the `agent_process_manager` plugin in a future release)
- `orchestrator-notifications` crate (becomes `notification_router` plugin in v0.6)
- `orchestrator-git-ops` crate (becomes command-phase patterns in workflow YAML packs)
- `orchestrator-logging` crate (becomes `telemetry_sink` plugin in v0.6)
- Anything in `orchestrator-providers` (already plugin-shaped; no work needed)
- Anything in `orchestrator-session-host` (subsumed by provider plugins individually)

Wave 2A agents must resist the urge to extract these. They will produce confusing PRs that break v0.5 scope.
