# TASK-026 Requirements: Event Notification Connector Framework

## Phase
- Workflow phase: `requirements`
- Workflow ID: `58f507ce-293c-4238-a251-857ff08a77f7`
- Task: `TASK-026`
- Requirement: `REQ-026`

## Objective
Lock the implementation contract for daemon notification connectors against the
current codebase, and define the remaining delta needed to fully satisfy
`REQ-026` acceptance intent.

## Current Baseline (Implemented)

| Surface | Current location | Current status |
| --- | --- | --- |
| Connector framework | `crates/orchestrator-cli/src/services/runtime/runtime_daemon/daemon_notifications.rs` | Implemented with typed adapters and routing runtime |
| Built-in connectors | `daemon_notifications.rs` | Implemented: `webhook`, `slack_webhook` |
| Subscription filtering | `NotificationSubscription::matches` | Implemented with event type wildcard support and optional project/workflow/task filters |
| Retry + dead-letter | `daemon_notifications.rs` outbox/dead-letter paths | Implemented with bounded exponential backoff and durable JSONL state |
| Lifecycle observability | `notification-delivery-*` event emissions | Implemented and emitted through daemon event stream |
| Daemon config wiring | `runtime_daemon.rs`, `cli_types.rs` | Implemented via `ao daemon config --notification-config-{json,file}` and clear flag |
| Operator docs | `crates/orchestrator-cli/docs/task-026-notification-operator-guide.md` | Implemented |

## Remaining Gap to Close in This Task
- Add explicit `task-state-change` daemon event coverage so subscriptions can
  target task lifecycle transitions directly (not only aggregate queue metrics).
- Ensure notification delivery failures/dead-letter lifecycle events are visible
  through `ao errors` surfaces (currently `ops_errors` sync focuses on `log`
  events).
- Clarify and freeze credential handling contract in requirements/docs:
  - scoped config stores credential references (env var names),
  - raw secret values remain out of repo state,
  - all emitted failure metadata remains redacted.

## Scope
In scope for implementation after this requirements phase:
- Preserve and validate current connector adapter model and existing connector
  types (`webhook`, `slack_webhook`).
- Add/verify event subscription coverage for:
  - workflow checkpoints,
  - run failures,
  - task state changes.
- Add task status transition event emission (`task-state-change`).
- Extend error observability so notification failures/dead-letters are queryable
  via `ao errors` commands.
- Update docs and tests to reflect final contract.

Out of scope for this task:
- Replacing the daemon event envelope schema (`ao.daemon.event.v1`).
- Introducing external secret manager dependencies.
- Adding desktop-wrapper or non-Rust dependencies.

## Constraints
- Do not persist raw credentials/tokens in `.ao` state, daemon events, or logs.
- Keep daemon scheduling resilient: notification failures must not halt project
  tick execution.
- Preserve backward compatibility for existing daemon config keys and current
  notification config schema/version (`ao.daemon-notification-config.v1`, v1).
- Keep retry and flush behavior bounded and deterministic.

## Functional Requirements

### FR-01: Connector Contract Stability
- Keep the adapter framework and existing connector types operational.
- Preserve deterministic connector selection and subscription matching behavior.
- Maintain extension path for future connector types without changing dispatch
  core semantics.

### FR-02: Subscription Event Coverage
- Subscription filters must support and document routing for:
  - workflow checkpoint events (`workflow-phase-*` checkpoint events),
  - run failure events (`workflow-phase-contract-violation`, relevant failure/log
    signals),
  - task state transitions (`task-state-change`).
- `task-state-change` must include enough context for filtering and operator
  triage (`task_id`, prior status, next status, optional workflow/task linkage).

### FR-03: Credential Handling and Redaction
- Notification config stores only credential references (env var names).
- Resolved secret values are never serialized to daemon config, outbox,
  dead-letter, or lifecycle events.
- Delivery errors remain actionable while redacted.

### FR-04: Retry, Dead-Letter, and Error Visibility
- Keep durable outbox/dead-letter behavior and retry classification intact.
- Notification delivery failures and dead-letter transitions must be visible in:
  - `ao daemon events`, and
  - `ao errors list/get/stats` outputs.

### FR-05: Documentation Completeness
- Requirements, implementation notes, and operator guide must align with actual
  command/config/runtime behavior.
- Examples must be safe (no inline raw secrets).

## Acceptance Criteria
- `AC-01`: Existing connector adapters (`webhook`, `slack_webhook`) continue to
  function with current config contract.
- `AC-02`: Subscriptions can reliably target workflow checkpoint and run failure
  events.
- `AC-03`: Daemon emits `task-state-change` events for task status transitions
  relevant to workflow execution.
- `AC-04`: Notification delivery failures and dead-letter outcomes are visible in
  both daemon event stream and `ao errors` surfaces.
- `AC-05`: Credential handling remains reference-based with no raw secret leakage
  in persisted state or emitted payloads.
- `AC-06`: TASK-026 docs are internally consistent and implementation-aligned.

## Testable Acceptance Checklist
- `T-01`: Unit tests for subscription matching across workflow, failure, and
  task-state-change event classes.
- `T-02`: Unit tests for task-state-change payload contract.
- `T-03`: Unit tests for credential redaction and missing-env failure paths.
- `T-04`: Integration tests for retry -> sent and retry -> dead-letter flows.
- `T-05`: Regression test confirming daemon run continues when notification
  delivery fails.
- `T-06`: `ops_errors` tests proving notification failures/dead-letters are
  surfaced correctly.

## Acceptance Verification Matrix
| Requirement area | Verification method |
| --- | --- |
| Connector/runtime stability | Existing + updated daemon notification runtime tests |
| Subscription coverage | Matcher tests and daemon event-driven integration tests |
| Task state change coverage | New event emission and payload assertions |
| Retry/dead-letter behavior | Outbox/dead-letter lifecycle tests |
| Error observability | `ops_errors` ingestion tests for notification lifecycle events |
| Documentation alignment | Doc review against CLI/runtime behavior |

## Implementation Notes (Input to Next Phase)
Primary source targets:
- `crates/orchestrator-cli/src/services/runtime/runtime_daemon/daemon_scheduler_project_tick.rs`
  - derive and emit task status transition events.
- `crates/orchestrator-cli/src/services/runtime/runtime_daemon/daemon_run.rs`
  - ensure task-state-change and notification lifecycle events are emitted in
    deterministic order.
- `crates/orchestrator-cli/src/services/operations/ops_errors.rs`
  - ingest notification failure/dead-letter lifecycle events.
- `crates/orchestrator-cli/docs/task-026-notification-operator-guide.md`
  - align examples and troubleshooting with final behavior.

## Deterministic Deliverables for Implementation Phase
- Task state transition event coverage (`task-state-change`).
- Notification lifecycle ingestion in `ao errors` flows.
- Updated tests validating event routing, retry/dead-letter, and redaction.
- Updated TASK-026 docs aligned to implemented behavior.
