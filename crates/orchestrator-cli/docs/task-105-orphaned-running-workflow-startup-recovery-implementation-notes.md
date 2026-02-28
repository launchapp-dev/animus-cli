# TASK-105 Implementation Notes: Startup Recovery for Orphaned Running Workflows

## Phase Context
- Workflow phase: `requirements`
- Workflow ID: `f3f07ca8-ef3d-4947-a35a-2a7fc4540623`
- Task: `TASK-105`

## Purpose
Translate TASK-105 requirements into a concrete, bounded implementation plan
for daemon startup recovery of workflows that are still marked `running` but no
longer have a live agent process.

## Non-Negotiable Constraints
- Keep changes scoped to daemon runtime paths in `orchestrator-cli`.
- Use existing service APIs for workflow/task mutations; no direct `.ao` state
  file edits.
- Keep startup recovery deterministic and startup-only (once per daemon run per
  project root).
- Avoid false-positive recovery for non-agent phase modes.
- Preserve existing CLI JSON envelope and daemon event schema compatibility
  (additive fields only).

## Proposed Change Surface

### 1) Startup Recovery Orchestration Hook
- Target: `crates/orchestrator-cli/src/services/runtime/runtime_daemon/daemon_run.rs`
  and/or `daemon_scheduler_project_tick.rs`.
- Add per-project-root startup guard (in-memory during one daemon run process)
  so orphan recovery executes once before normal phase execution loops.
- Gate on existing `reconcile_stale` toggle to align with current stale
  reconciliation semantics.

### 2) Orphan Detection Helper
- Target:
  `crates/orchestrator-cli/src/services/runtime/runtime_daemon/daemon_scheduler_project_tick.rs`.
- Add helper that:
  - lists running workflows,
  - filters to agent-executed current phases,
  - resolves run-id candidates for each workflow using deterministic
    `wf-<workflow_id>-...` matching,
  - queries runner per candidate run id via `AgentStatusRequest`.
- Classify workflow as orphaned only when no candidate run id is active.

### 3) Recovery Mutations
- Target:
  `crates/orchestrator-cli/src/services/runtime/runtime_daemon/daemon_scheduler_project_tick.rs`.
- Implement two-path recovery:
  - primary: reset phase for re-execution (service-API based),
  - fallback: cancel workflow and set task `ready`.
- Ensure task/workflow coherence after each path and emit consistent recovery
  reason text.

### 4) Summary + Event Payload Extensions
- Target:
  - `crates/orchestrator-cli/src/services/runtime/runtime_daemon/daemon_scheduler.rs`
    (`ProjectTickSummary`),
  - `crates/orchestrator-cli/src/services/runtime/runtime_daemon/daemon_run.rs`
    (event emission).
- Add additive summary fields for orphan recovery counters and workflow IDs.
- Include fields in emitted `workflow` (and/or `queue`) daemon events for
  operator auditability.

### 5) Run-ID Resolution Utility
- Target (as needed):
  - `daemon_scheduler_project_tick.rs`,
  - possibly shared helper in daemon runtime module.
- Keep resolution deterministic with explicit tie-breakers for multiple
  candidate run ids (for example lexical or newest-known ordering).
- Handle missing/malformed candidate data as non-fatal and deterministic.

## Suggested Implementation Sequence
1. Add startup-once guard plumbing for project roots.
2. Implement orphan detector with runner status query helper seams.
3. Implement recovery mutation paths and result struct.
4. Wire recovery results into `ProjectTickSummary` and daemon event payloads.
5. Add focused tests for detection/recovery/no-op/fallback/startup-once.
6. Run targeted daemon scheduler/daemon run tests.

## Validation Targets
- `cargo test -p orchestrator-cli runtime_daemon::daemon_scheduler`
- `cargo test -p orchestrator-cli runtime_daemon::daemon_run`
- Optional broader pass:
  - `cargo test -p orchestrator-cli`

## Risks and Mitigations
- Risk: false positives for active workflows due run-id resolution ambiguity.
  - Mitigation: strict phase-mode filtering + deterministic candidate rules +
    explicit active-status checks.
- Risk: repeated recovery across ticks.
  - Mitigation: startup-once project-root guard for each daemon run lifecycle.
- Risk: task/workflow drift after fallback cancellation.
  - Mitigation: explicit post-mutation task status assertions in tests.
- Risk: runner unavailable during startup.
  - Mitigation: deterministic unavailable handling (treat as no active run,
    recover via policy) and non-fatal daemon loop behavior.

## Deliverables for Next Phase
- Startup-only orphaned-running-workflow recovery path in daemon scheduler.
- Additive daemon summary/event telemetry for orphan recovery outcomes.
- Deterministic tests covering liveness detection and recovery policy.
