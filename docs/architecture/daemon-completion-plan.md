# Daemon Completion Plan: From "Works Sometimes" to Production-Ready

## Current State

The daemon architecture is **structurally sound** — SubjectDispatch is the primary contract, workflow-runner is the execution host, projectors handle state writeback. But **operational reliability** is undermined by ~30 silent error paths, no process timeout/escalation, and non-atomic state persistence.

```
DAEMON LOOP (run_daemon.rs, interval-based)
  ↓
TICK CYCLE (run_project_tick.rs)
  ├─ Schedule evaluation (cron + active_hours)
  ├─ Snapshot capture (tasks, requirements, health)
  ├─ Manual timeout reconciliation
  ├─ Process completion → SubjectExecutionFact → projectors
  ├─ Ready task dispatch → SubjectDispatch → ProcessManager
  └─ Summary + event emission
```

---

## Phase 1: Stop Silent Failures (Reliability)

**Goal**: Every failure is either handled, retried, or surfaced — never silently swallowed.

### 1.1 Classify Error Patterns

There are 30+ silent error sites. Each falls into one of three categories:

| Category | Pattern | Count | Fix |
|----------|---------|-------|-----|
| **Must propagate** | `.unwrap_or_default()` on workflow/task lists | 6 | Return `Result`, let tick fail visibly |
| **Log and continue** | `let _ =` on PID file writes, event appends | 13 | `if let Err(e) = ... { warn!(...) }` |
| **Acceptable** | `.ok()` on PID file reads, event line parsing | 5 | Keep — graceful degradation is correct |

### 1.2 Critical Dispatch Path Fixes

**Files to change:**

```
daemon_task_dispatch.rs:38    .unwrap_or_default() → .context("workflow list")?
daemon_task_dispatch.rs:130   .unwrap_or_default() → .context("workflow list")?
daemon_reconciliation.rs:22   .unwrap_or_default() → log warning + continue with empty
daemon_reconciliation.rs:49   let _ = cancel... → log error, increment failed counter
daemon_reconciliation.rs:61   .unwrap_or_default() → log warning + continue with empty
```

**Principle**: The dispatch path must fail loudly. If we can't list workflows, we MUST NOT dispatch new ones — that's how duplicate workflows happen.

### 1.3 Process Tracker Race Fix

**File**: `agent-runner/src/cleanup.rs:51-70`

Current code does read-modify-write without locking:

```rust
// BROKEN: Two concurrent writers can lose entries
let mut tracked = fs::read_to_string(&path)...;
tracked.insert(run_id, pid);
fs::write(&path, tracked)?;
```

Fix: Use `fs2::FileExt::lock_exclusive()` around the read-modify-write cycle, or use atomic temp+rename.

### 1.4 Process Kill Escalation

**File**: `agent-runner/src/runner/process.rs:1050`

Current: single `kill_process()` call, warning on failure, continue.

Fix: SIGTERM → wait 5s → SIGKILL → verify. Process group kill (`kill(-pgid)`) to catch children.

```rust
fn terminate_process_group(pid: i32) -> bool {
    unsafe { libc::kill(-pid, libc::SIGTERM) };
    for _ in 0..50 {
        std::thread::sleep(Duration::from_millis(100));
        if !is_process_alive(pid as u32) { return true; }
    }
    unsafe { libc::kill(-pid, libc::SIGKILL) };
    !is_process_alive(pid as u32)
}
```

### 1.5 Atomic State Writes

**Files**: `daemon_runtime_state.rs`, `dispatch_queue_state.rs`

Current: `fs::write()` directly — crash mid-write corrupts file.

Fix: Write to temp file + `fs::rename()` (atomic on same filesystem).

```rust
fn write_json_atomic(path: &Path, value: &impl Serialize) -> Result<()> {
    let tmp = path.with_extension("tmp");
    let content = serde_json::to_string_pretty(value)?;
    fs::write(&tmp, &content)?;
    fs::rename(&tmp, path)?;
    Ok(())
}
```

---

## Phase 2: Process Lifecycle Hardening

**Goal**: No workflow-runner process can hang indefinitely or become a zombie.

### 2.1 ProcessManager Timeout

**File**: `process_manager.rs`

Add a `started_at` timestamp to `WorkflowProcess`. In `check_running()`, if a process exceeds `phase_timeout_secs` (from daemon options), kill it and emit a timeout fact.

### 2.2 Stderr Reader Resilience

**File**: `process_manager.rs:50-59`

Current: `tokio::spawn` captures stderr with no error handling.

Fix: Use `tokio::spawn` with a timeout, and if stderr reader panics, mark the process as failed rather than leaving it in limbo.

### 2.3 Orphan Recovery on Startup

Currently works via `recover_orphaned_running_workflows()` but:
- Only runs if `startup_cleanup` flag is true
- Errors from `cancel_orphaned_running_workflow()` are silently discarded

Fix: Make startup cleanup mandatory (not optional), and propagate cancellation errors.

---

## Phase 3: Typed Error Classification

**Goal**: Replace string-based error matching with structured error types.

### 3.1 Replace `classify_error_message()`

**File**: `protocol/src/error_classification.rs`

Current: Pattern-matches on lowercased error message strings.

Fix: Define `WorkflowError` enum in protocol crate:

```rust
pub enum WorkflowErrorKind {
    Timeout,
    ProcessCrash { exit_code: i32 },
    MergeConflict,
    ToolFailure { tool_name: String },
    RateLimited,
    AuthFailure,
    Internal { message: String },
}
```

Workflow-runner emits structured `RunnerEvent` with error kind. Daemon projects the error kind directly — no string parsing needed.

### 3.2 Retry Policy per Error Kind

```rust
impl WorkflowErrorKind {
    fn retry_policy(&self) -> RetryPolicy {
        match self {
            Timeout | RateLimited => RetryPolicy::ExponentialBackoff { max: 3 },
            ProcessCrash { .. } => RetryPolicy::Immediate { max: 1 },
            MergeConflict => RetryPolicy::Manual,
            AuthFailure => RetryPolicy::None,
            _ => RetryPolicy::None,
        }
    }
}
```

---

## Phase 4: Event System Hardening

**Goal**: Events are reliable, bounded, and queryable.

### 4.1 Atomic Event Append

**File**: `daemon_event_log.rs:67-86`

Fix: Use `fs2::FileExt::lock_exclusive()` before appending. Ensure single-line JSON write is complete (no partial lines).

### 4.2 Event Log Rotation

Add rotation at startup: if `daemon-events.jsonl` exceeds 10MB, rename to `.1` and start fresh.

### 4.3 Surface Event Errors

Replace `append_fire_and_forget()` with `append_or_warn()` that logs failures instead of silently discarding.

---

## Phase 5: Configuration Convergence

**Goal**: Single YAML config surface for all daemon behavior.

### 5.1 Current Config Files

| File | Format | Purpose |
|------|--------|---------|
| `.ao/workflows.yaml` | YAML | Workflow definitions, phases, tools |
| `.ao/pm-config.json` | JSON | Daemon behavior flags (auto_merge, auto_pr) |
| `.ao/state/agent-runtime-config.v2.json` | JSON | Agent profiles, model routing |
| `.ao/daemon-state.json` | JSON | Runtime state (PID, paused, shutdown) |
| `.ao/state/schedule-state.json` | JSON | Schedule run history |

### 5.2 Migration Plan

1. Move `pm-config.json` fields into `workflows.yaml` under a `daemon:` section
2. Move `agent-runtime-config.v2.json` into `workflows.yaml` under `agents:` section
3. Keep `daemon-state.json` and `schedule-state.json` as runtime state (not config)
4. Deprecate JSON config loading with clear error messages

Target YAML structure:
```yaml
daemon:
  auto_merge: false
  auto_pr: false
  active_hours: "09:00-18:00"
  pool_size: 3

agents:
  default:
    model: claude-sonnet-4-6
    tool: claude

workflows:
  implementation:
    phases: [...]

schedules:
  nightly-cleanup:
    cron: "0 2 * * *"
    workflow_ref: cleanup
```

---

## Phase 6: Missing Test Coverage

### 6.1 Dispatch Round-Trip Test

Verify: enqueue → tick → workflow-runner spawned → completion fact → task status projected.

This requires a mock workflow-runner that exits immediately with a RunnerEvent on stderr.

### 6.2 Crash Recovery Test

Verify: daemon killed mid-tick → restart → orphaned workflows recovered → no duplicate dispatch.

### 6.3 Concurrent Dispatch Test

Verify: multiple ready tasks → dispatched up to pool_size limit → no races.

### 6.4 Process Timeout Test

Verify: hanging workflow-runner → timeout reached → process killed → task blocked with timeout reason.

---

## Priority Order

| Priority | Phase | Effort | Impact |
|----------|-------|--------|--------|
| P0 | 1.2 Critical dispatch path fixes | 2-3 hours | Prevents duplicate workflows |
| P0 | 1.3 Process tracker race fix | 1 hour | Prevents zombie processes |
| P1 | 1.4 Process kill escalation | 2 hours | Prevents resource exhaustion |
| P1 | 1.5 Atomic state writes | 1 hour | Prevents state corruption |
| P1 | 2.1 ProcessManager timeout | 2 hours | Prevents hanging workflows |
| P2 | 3.1 Typed error classification | 4-6 hours | Correct retry behavior |
| P2 | 4.1-4.3 Event system fixes | 2-3 hours | Reliable audit trail |
| P3 | 5.1-5.2 Config convergence | 6-8 hours | Clean operator experience |
| P3 | 6.1-6.4 Test coverage | 4-6 hours | Regression safety |

**Total estimated: ~25-35 hours of implementation work.**

---

## What "Complete and Final" Looks Like

The daemon is done when:

1. **Zero silent failures**: Every error is either handled, retried, or surfaced in events/logs
2. **No zombie processes**: Every spawned process has a timeout and kill escalation path
3. **Atomic state**: All state files use temp+rename writes with file locking where needed
4. **Typed errors**: No string-based error classification — structured error kinds with retry policies
5. **Single config surface**: One YAML file controls workflows, agents, schedules, and daemon behavior
6. **Full e2e coverage**: Dispatch round-trip, crash recovery, concurrent dispatch, and timeout tests all pass
7. **Bounded event log**: Rotation, atomic append, no silent drops
