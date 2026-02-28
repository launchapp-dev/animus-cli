# TASK-103 Requirements: Daemon Process Logging to File

## Phase
- Workflow phase: `requirements`
- Workflow ID: `94d0e19c-88dc-4000-a526-30125a399b65`
- Task: `TASK-103`

## Objective

When the daemon is started in autonomous mode (`ao daemon start --autonomous`),
`spawn_autonomous_daemon_run()` currently discards all subprocess output by
piping stdout, stderr, and stdin to `Stdio::null()`. Any Rust panic, OS error,
or `eprintln!` from the child process is silently lost, making daemon crash
diagnosis impossible.

This task:
1. Redirects the child's **stderr** to a scoped log file at
   `~/.ao/<scope>/daemon/daemon.log`.
2. Applies **basic log rotation**: if that file exceeds 10 MiB on startup,
   rename it to `daemon.log.old` before opening.
3. Adds **structured startup and shutdown log lines** (with RFC 3339 timestamps)
   written by the child process to its stderr (which now flows to the log file).

## Current Baseline Audit

| Surface | Location | Current behavior | Gap |
| --- | --- | --- | --- |
| Autonomous daemon spawn | `runtime_daemon.rs:254` (`spawn_autonomous_daemon_run`) | stdout/stderr/stdin all → `Stdio::null()` | panics and errors silently discarded |
| Daemon foreground run | `daemon_run.rs` (`handle_daemon_run`) | emits daemon events to `daemon-events.jsonl` and stdout; no explicit stderr log lines | no startup/shutdown timestamps in persistent log |
| Log rotation | none | no log management exists | log can grow unbounded or never be created |

## Scope

In scope:
- Open `~/.ao/<scope>/daemon/daemon.log` in append mode from the spawner and
  pass it as the child's `Stdio::from(file)` for **stderr**.
- Keep `Stdio::null()` for **stdout** (structured events already go to
  `daemon-events.jsonl`).
- Keep `Stdio::null()` for **stdin**.
- Rotate before opening: if the log file is ≥ 10 MiB, rename it to
  `daemon.log.old` (overwriting any prior `.old` file).
- Create the `~/.ao/<scope>/daemon/` directory if it does not exist.
- In `handle_daemon_run`, emit one **startup** log line to stderr immediately
  after acquiring the run guard, and one **shutdown** log line before returning
  (success or error).
- Log line format (human-readable, one line each):
  ```
  [<RFC3339_UTC>] daemon starting pid=<PID> project=<canonical_project_root>
  [<RFC3339_UTC>] daemon stopping pid=<PID> project=<canonical_project_root>
  ```

Out of scope:
- Structured JSON logging for the daemon log file.
- Streaming the log over IPC or MCP.
- Rotating more than one generation (only one `.old` file retained).
- Any changes to `daemon-events.jsonl` format or path.
- Log file exposure via `ao daemon logs` CLI (separate task, AGENTS.md line 155).

## Constraints

- **No new dependencies**: use only `std::fs`, `chrono` (already a dependency),
  and `dirs` (already used in `shared/runner.rs`).
- **Rotation is best-effort**: if the rename fails (e.g. permissions), log a
  warning to the console and continue — do not abort daemon spawn.
- **File open failure is soft-fail**: if the log file cannot be opened, fall
  back to `Stdio::null()` for that descriptor and emit a warning to the
  operator console — do not abort daemon spawn.
- The log path must use the same `repository_scope_for_path` logic as
  `scoped_ao_root` in `shared/runner.rs` — specifically
  `dirs::home_dir()/.ao/<scope>/daemon/daemon.log`.
- Changes must compile without warnings (`cargo build -p orchestrator-cli`).
- Existing daemon behavior (event emission, registry PID tracking) must be
  unchanged.

## Data Contract

### Log file path
```
~/.ao/<repo-slug>-<12hex>/daemon/daemon.log
~/.ao/<repo-slug>-<12hex>/daemon/daemon.log.old  (rotation target)
```
where `<repo-slug>-<12hex>` is the output of
`protocol::repository_scope_for_path(project_root)`.

### Startup log line (written to child stderr)
```
[2026-02-27T12:34:56.789Z] daemon starting pid=12345 project=/path/to/project
```

### Shutdown log line (written to child stderr)
```
[2026-02-27T12:35:00.123Z] daemon stopping pid=12345 project=/path/to/project
```

## Functional Requirements

### FR-01: Log File Redirect
`spawn_autonomous_daemon_run` must open `~/.ao/<scope>/daemon/daemon.log` in
create-if-missing + append mode and pass it as `Stdio::from(file)` for the
spawned child's stderr.

### FR-02: Log Rotation
Before opening the log file, check its size. If ≥ 10 MiB (10 * 1024 * 1024
bytes), attempt to rename it to `daemon.log.old`. Rotation failure must be
non-fatal.

### FR-03: Directory Creation
Ensure `~/.ao/<scope>/daemon/` exists before opening the log file, using
`std::fs::create_dir_all`.

### FR-04: Soft-Fail on I/O Error
If the log file cannot be opened after rotation, fall back to `Stdio::null()`
for stderr (matching existing behavior). Emit a warning to operator stdout.

### FR-05: Startup Log Line
`handle_daemon_run` must write a startup log line to stderr (format per Data
Contract above) after acquiring `DaemonRunGuard`. Must use `eprintln!` or
equivalent stderr write.

### FR-06: Shutdown Log Line
`handle_daemon_run` must write a shutdown log line to stderr before returning
(regardless of success/error path).

### FR-07: Timestamp Format
All timestamps in log lines must be RFC 3339 UTC (via `chrono::Utc::now().to_rfc3339()`).

## Acceptance Criteria

- `AC-01`: After `ao daemon start --autonomous`, the file
  `~/.ao/<scope>/daemon/daemon.log` exists and contains at least a startup
  log line.
- `AC-02`: After daemon stops, a shutdown log line is present in the log file.
- `AC-03`: Rust panics in the daemon subprocess appear in `daemon.log` (not
  silently discarded).
- `AC-04`: If `daemon.log` ≥ 10 MiB at startup, a `daemon.log.old` file is
  created with the prior contents.
- `AC-05`: `daemon-events.jsonl` format and behavior are unchanged.
- `AC-06`: `cargo build -p orchestrator-cli` compiles without errors or
  warnings.
- `AC-07`: Existing daemon start/stop/status CLI behavior is unchanged.

## Implementation Notes (for Next Phase)

### Primary source targets
- `crates/orchestrator-cli/src/services/runtime/runtime_daemon.rs`
  - `spawn_autonomous_daemon_run()` at line 254: add log path computation,
    directory creation, rotation, file open, `Stdio::from(file)` for stderr
- `crates/orchestrator-cli/src/services/runtime/runtime_daemon/daemon_run.rs`
  - `handle_daemon_run()`: add `eprintln!` startup and shutdown log lines

### Helper function location
Add a private `daemon_log_path(project_root: &str) -> PathBuf` in
`runtime_daemon.rs` following the same pattern as `pm_config_path`:
```rust
fn daemon_log_path(project_root: &str) -> PathBuf {
    let canonical = PathBuf::from(canonicalize_lossy(project_root));
    let scope = protocol::repository_scope_for_path(&canonical);
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".ao")
        .join(scope)
        .join("daemon")
        .join("daemon.log")
}
```

### Rotation helper (inline in spawn_autonomous_daemon_run)
```rust
fn rotate_daemon_log_if_needed(log_path: &std::path::Path) {
    const MAX_LOG_BYTES: u64 = 10 * 1024 * 1024;
    if std::fs::metadata(log_path)
        .map(|m| m.len() >= MAX_LOG_BYTES)
        .unwrap_or(false)
    {
        let old_path = log_path.with_extension("log.old");
        let _ = std::fs::rename(log_path, &old_path);
    }
}
```

### Stderr redirect in spawn_autonomous_daemon_run
Replace `.stderr(Stdio::null())` with:
```rust
let stderr_stdio = open_daemon_log_for_append(project_root);
// ...
.stderr(stderr_stdio)
```

where `open_daemon_log_for_append` returns `Stdio`.

### No test changes required
The spawner behavior (stderr redirect) is not unit-testable without spawning
real processes. Acceptance criteria AC-01 through AC-04 are verified by manual
smoke test or integration test. AC-05 through AC-07 verified by `cargo build`
and existing test suite passing.

## Deterministic Deliverables for Implementation Phase
1. `daemon_log_path(project_root)` helper in `runtime_daemon.rs`
2. Log rotation + file open in `spawn_autonomous_daemon_run`
3. Startup `eprintln!` in `handle_daemon_run` after run guard acquired
4. Shutdown `eprintln!` in `handle_daemon_run` before return
5. `cargo build -p orchestrator-cli` passes without warnings
