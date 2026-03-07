# TASK-026 Implementation Notes: Event Notification Connector Framework

## Purpose
Define the concrete implementation delta needed to complete TASK-026 based on
what already exists in
`crates/orchestrator-daemon-runtime/src/notification_runtime.rs` and related
daemon runtime plumbing.

## Current State Summary
Already implemented in code:
- connector runtime with `webhook` and `slack_webhook` adapters,
- subscription filtering (`event_types` wildcard + optional project/workflow/task
  filters),
- durable outbox + dead-letter with bounded retry/backoff,
- notification lifecycle daemon events,
- daemon config CLI flags for notification config JSON/file updates,
- operator guide with setup/troubleshooting flow.

Remaining implementation focus:
- explicit `task-state-change` event emission,
- notification lifecycle ingestion in `ao errors` surfaces,
- contract/documentation alignment and acceptance-proof tests.

## Non-Negotiable Constraints
- Preserve `ao.daemon-notification-config.v1` schema compatibility.
- Preserve existing connector behavior and retry classification semantics.
- Never leak raw credential values in output/log/state.
- Keep daemon scheduling non-blocking under notification failures.

## Proposed Change Surface

### Task Transition Event Emission
- `crates/orchestrator-cli/src/services/runtime/runtime_daemon/daemon_scheduler_project_tick.rs`
  - capture task status transitions during tick execution.
  - produce structured transition records (task id, from/to status,
    workflow/task linkage where available).
- `crates/orchestrator-cli/src/services/runtime/runtime_daemon/daemon_scheduler.rs`
  - extend `ProjectTickSummary` with task transition events.
- `crates/orchestrator-cli/src/services/runtime/runtime_daemon/daemon_run.rs`
  - emit `task-state-change` daemon events from summary transitions using
    existing event emission path.

### Error Surface Integration
- `crates/orchestrator-cli/src/services/operations/ops_errors.rs`
  - ingest notification lifecycle failures:
    - `notification-delivery-failed` (error category: `notification`),
    - `notification-delivery-dead-lettered` (severity escalated as needed).
  - de-duplicate by source event id as with existing daemon error ingestion.

### Documentation Alignment
- `crates/orchestrator-cli/docs/task-026-notification-operator-guide.md`
  - add explicit subscription examples for `task-state-change`.
  - clarify how notification lifecycle failures appear in `ao errors`.
- `crates/orchestrator-cli/docs/task-026-event-notification-connector-framework-requirements.md`
  - keep acceptance and verification matrix aligned to implemented behavior.

## Data Contract Additions
Add normalized daemon event payload for task transitions:
- `event_type`: `task-state-change`
- `data`:
  - `task_id`
  - `from_status`
  - `to_status`
  - `changed_at`
  - optional `workflow_id`
  - optional `phase_id`

## Implementation Sequence
1. Add task transition detection in project tick execution path.
2. Thread transition records through summary and emit `task-state-change` events.
3. Extend `ops_errors` sync logic for notification failure/dead-letter events.
4. Add/adjust tests for transition emission and errors ingestion.
5. Refresh TASK-026 docs and examples to match final behavior.

## Test Plan
- Unit tests:
  - task transition extraction logic,
  - task-state-change payload normalization,
  - notification lifecycle -> error record mapping.
- Integration tests:
  - daemon run emits task-state-change when task status advances,
  - notification delivery failures become visible in `errors list`.
- Regression tests:
  - existing notification outbox/dead-letter behavior remains unchanged,
  - daemon tick continues when connector delivery fails.

## Risks and Mitigations
- Risk: noisy transition emission for non-meaningful updates.
  - Mitigation: emit only when canonical status value changes.
- Risk: duplicate error records from repeated sync.
  - Mitigation: retain source-event-id dedupe contract.
- Risk: ordering ambiguity between primary daemon events and transition events.
  - Mitigation: emit through shared sequence counter path in daemon run loop.

## Validation Targets for Implementation Phase
- `cargo test -p orchestrator-cli runtime_daemon`
- `cargo test -p orchestrator-cli --test cli_e2e`
- `cargo test -p orchestrator-cli --test cli_smoke`
