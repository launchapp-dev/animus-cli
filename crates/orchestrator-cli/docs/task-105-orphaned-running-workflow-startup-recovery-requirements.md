# TASK-105 Requirements: Startup Recovery for Orphaned Running Workflows

## Phase
- Workflow phase: `requirements`
- Workflow ID: `f3f07ca8-ef3d-4947-a35a-2a7fc4540623`
- Task: `TASK-105`
- Requirement: unlinked in current task metadata

## Objective
On daemon startup, detect workflows stuck in `running` state with no live
phase agent and recover them deterministically so tasks do not remain
permanently in `in-progress` with no active execution.

Target outcome from task brief:
- identify orphaned running workflows at startup,
- verify runner liveness against workflow phase run IDs,
- recover each orphaned workflow by either:
  - resetting the current phase for re-execution, or
  - cancelling the workflow and returning the task to `ready`.

## Current Baseline Audit

| Surface | Current location | Current behavior | Gap |
| --- | --- | --- | --- |
| Daemon startup tick entry | `crates/orchestrator-cli/src/services/runtime/runtime_daemon/daemon_scheduler_project_tick.rs` (`project_tick`) | runs `resume_interrupted` and `reconcile_stale` paths before normal scheduling | no startup workflow-agent liveness scan |
| Interrupted workflow resume path | `daemon_scheduler_project_tick.rs` (`resume_interrupted_workflows_for_project`) | resumes based on workflow state-machine resumability only | does not verify active runner agent for current phase run |
| Stale in-progress reconciliation | `daemon_scheduler_project_tick.rs` (`reconcile_stale_in_progress_tasks_for_project`) | reconciles task status drift from workflow/task status mismatch | task-level only; does not recover workflows stuck in `running` with no agent |
| Phase run-id generation | `crates/orchestrator-cli/src/services/runtime/runtime_daemon/daemon_scheduler_phase_exec.rs` | agent phase run ids follow `wf-<workflow_id>-<phase_id>-<target>-<uuid>` | startup recovery path does not currently use this run-id shape for liveness checks |
| Runner liveness API | `crates/protocol/src/agent_runner.rs` (`AgentStatusRequest`) + runner implementation in `crates/agent-runner/src/runner/mod.rs` | supports per-run status query and explicit `not_found` for unknown runs | daemon startup path does not query per-run status for running workflows |
| Transient runner failure handling | `crates/orchestrator-cli/src/services/runtime/runtime_daemon/daemon_scheduler_failover.rs` | connection and disconnect errors are treated as transient for phase execution | can leave workflows effectively stalled without explicit startup orphan recovery |

## Problem Statement
After daemon interruption, workflow state can remain `running` while no phase
agent process is alive. Current startup logic does not reconcile this mismatch
at workflow/run-id level, causing indefinite `running` workflows and blocked
task throughput.

## Scope
In scope for implementation after this requirements phase:
- Add startup-only orphaned-running-workflow detection for each project root.
- Verify runner liveness for running workflows by checking workflow-associated
  phase run IDs against `AgentStatusRequest`.
- Recover orphaned workflows with deterministic policy:
  - preferred: reset current phase to allow re-execution,
  - fallback: cancel workflow and set task status to `ready`.
- Add startup recovery counters/metadata to daemon tick summary and daemon
  events.
- Add focused regression tests for detection, recovery, and no-false-positive
  behavior.

Out of scope:
- Redesigning workflow state-machine semantics.
- Replacing existing `resume_interrupted` or task stale-reconcile flows.
- Protocol schema changes for runner status payloads.
- Manual edits to `/.ao/*.json`.

## Constraints
- Recovery must be deterministic and idempotent at startup.
- Recovery applies only to workflows in `WorkflowStatus::Running`.
- Recovery must avoid false positives for phases that do not require an agent
  process (for example manual/command phase modes).
- Liveness checks must use existing runner IPC contracts (no shelling out to
  external process lists as primary truth).
- Changes must remain repository-safe and use existing AO service APIs for
  workflow/task mutations.
- Recovery should run once per daemon startup per project root (not repeatedly
  every scheduler tick).

## Functional Requirements

### FR-01: Startup Orphan Scan Trigger
- When daemon run starts processing a project root and `reconcile_stale` is
  enabled, execute orphaned-running-workflow scan before normal phase execution
  for that root.
- Scan must run once per daemon process lifecycle for each project root.

### FR-02: Workflow Run-ID Liveness Check
- For each candidate `running` workflow, resolve workflow-associated phase
  run-id candidate(s) using deterministic matching on known daemon phase run-id
  prefix shape (`wf-<workflow_id>-...`).
- For each resolved candidate run id, query runner via `AgentStatusRequest`.
- A workflow is considered live when at least one candidate run id returns an
  active runner status for that run.
- If no candidate run id is active (or all resolve to terminal/not-found), the
  workflow is considered orphaned.

### FR-03: Candidate Filtering
- Candidate set for orphan detection must include only workflows where:
  - workflow status is `running`,
  - current phase is agent-executed (not manual/command-only execution mode).
- Completed/failed/cancelled/paused workflows are excluded.

### FR-04: Recovery Policy
- For each orphaned workflow:
  - primary recovery path: reset current phase for re-execution through
    workflow service APIs, preserving auditable failure reason for recovery.
  - if primary path cannot produce runnable workflow state, fallback path:
    cancel workflow and set task status to `ready`.
- Recovery path taken must be recorded in structured summary/evidence fields.

### FR-05: Task State Coherence
- After successful reset-for-retry recovery, task remains `in-progress`.
- After cancel-and-ready fallback recovery, task must be `ready`.
- Recovery must not leave task/workflow in contradictory terminal/non-terminal
  combinations.

### FR-06: Startup Summary and Event Visibility
- Project tick summary must expose orphan recovery outcomes:
  - detected count,
  - recovered count,
  - canceled-fallback count,
  - affected workflow IDs.
- Daemon event output must include these values in startup/tick telemetry so
  operators can audit recovery outcomes.

### FR-07: Deterministic Non-Destructive Behavior
- If runner confirms active agent for workflow run id, workflow must remain
  unchanged.
- Re-running startup scan against unchanged state must not introduce additional
  transitions.

### FR-08: Regression Coverage
- Add tests for:
  - orphan detection and primary recovery,
  - active-run liveness no-op behavior,
  - phase-mode filtering (manual/command phases are not treated as orphaned
    agent runs),
  - fallback cancel-and-ready behavior,
  - startup-once execution semantics.

## Acceptance Criteria
- `AC-01`: On daemon startup, running workflows without active runner agent are
  detected as orphaned.
- `AC-02`: Running workflows with active runner agent for resolved run id are
  not recovered/cancelled.
- `AC-03`: Orphaned workflows are recovered via phase reset when primary
  recovery succeeds.
- `AC-04`: When phase reset recovery is not possible, workflow is cancelled and
  task is set to `ready`.
- `AC-05`: Recovery is restricted to agent-executed running phases; manual or
  command-only phases are not falsely recovered as orphaned-agent runs.
- `AC-06`: Startup recovery executes once per daemon startup per project root.
- `AC-07`: Daemon summary/events expose orphan detection and recovery counts
  with affected workflow IDs.
- `AC-08`: Targeted tests validate detection, recovery policy, startup-once
  behavior, and non-regression for live workflows.

## Testable Acceptance Checklist
- `T-01`: daemon scheduler test with running workflow + no active run id status
  returns orphan detection and performs primary reset recovery.
- `T-02`: daemon scheduler test with active runner status for workflow run id
  confirms no recovery mutation.
- `T-03`: test for manual/command phase workflow shows no orphan-agent
  recovery action.
- `T-04`: test forcing primary recovery failure verifies cancel-and-ready
  fallback.
- `T-05`: daemon run/project tick test verifies startup recovery executes once
  per project root for a single daemon run lifecycle.
- `T-06`: daemon event/summary test asserts orphan recovery counters and
  workflow id list are present.

## Verification Matrix

| Requirement area | Verification method |
| --- | --- |
| FR-01, FR-07 | project tick startup orchestration tests |
| FR-02, FR-03 | run-id liveness resolver tests + runner-status query stubs |
| FR-04, FR-05 | workflow/task mutation tests for reset and cancel fallback paths |
| FR-06 | daemon run queue/workflow event payload assertions |
| FR-08 | targeted `cargo test -p orchestrator-cli` for touched daemon modules |

## Implementation Notes Input (Next Phase)
Primary source targets:
- `crates/orchestrator-cli/src/services/runtime/runtime_daemon/daemon_scheduler_project_tick.rs`
- `crates/orchestrator-cli/src/services/runtime/runtime_daemon/daemon_scheduler.rs` (tests)
- `crates/orchestrator-cli/src/services/runtime/runtime_daemon/daemon_run.rs`
- `crates/orchestrator-cli/src/services/runtime/runtime_daemon/daemon_scheduler_phase_exec.rs` (run-id mapping utilities if needed)

Supporting references:
- `crates/orchestrator-cli/src/services/runtime/runtime_agent/status.rs`
- `crates/protocol/src/agent_runner.rs`
- `crates/agent-runner/src/runner/mod.rs`

## Deterministic Deliverables for Implementation Phase
- Startup-only orphaned-running-workflow detection tied to runner liveness.
- Deterministic reset-or-cancel recovery policy with task-state coherence.
- Structured daemon telemetry for orphan recovery outcomes.
- Focused regression tests that prevent stuck `running` workflow regressions.
