# TASK-043 Requirements: MCP Agent Output Tailing for Workflow Phases

## Phase
- Workflow phase: `requirements`
- Workflow ID: `e63386f5-83d5-421b-9078-c5db95b6980a`
- Task: `TASK-043`

## Objective
Add a deterministic MCP tool, `ao.output.tail`, that returns the most recent
agent output/thinking/error events for a running or recently completed workflow
phase using request/response semantics (no streaming dependency).

## Current Baseline Audit

| Surface | Current location | Current behavior | Gap |
| --- | --- | --- | --- |
| MCP output tools | `crates/orchestrator-cli/src/services/operations/ops_mcp.rs` | exposes `ao.output.run`, `ao.output.monitor`, `ao.output.jsonl`, `ao.output.artifacts` | no tail-oriented MCP tool for incremental visibility |
| MCP tool execution model | `ops_mcp.rs` (`AoMcpServer::run_tool`) | executes AO CLI and parses one JSON envelope from stdout | not suitable for stream-style observation flows |
| Output monitor command | `crates/orchestrator-cli/src/services/operations/ops_output.rs` (`OutputCommand::Monitor`) | reads all JSONL entries for `run_id` and applies optional task/phase filtering | no bounded last-N tail contract and no event-type filtering |
| Run JSONL lookup behavior | `ops_output.rs` (`resolve_run_dir_for_lookup`) | deterministic run-dir lookup (scoped canonical path, then legacy fallbacks) | currently not exposed as MCP tail contract |
| Agent event schema | `crates/protocol/src/agent_runner.rs` (`AgentRunEvent`) | persisted events include `output_chunk`, `error`, `thinking`, etc. | no MCP response contract to return filtered tail slices |
| Workflow phase run id shape | `runtime_daemon/daemon_scheduler_phase_exec.rs` | phase runs are emitted as `wf-<workflow_id>-<phase_id>-<target>-<uuid>` | no documented task-id -> run-id resolution contract for MCP output tailing |

## Scope
In scope for implementation after this requirements phase:
- Add MCP tool `ao.output.tail`.
- Tool input supports either `run_id` or `task_id` lookup.
- Read `events.jsonl` from the resolved run directory.
- Return the last `N` matching events with deterministic ordering.
- Support event-type filtering: `output`, `error`, `thinking`.
- Add focused tests for lookup, filtering, bounds, and malformed-line handling.

Out of scope:
- Replacing/removing existing `ao.output.monitor` behavior.
- Introducing streaming MCP responses.
- Changing persisted `AgentRunEvent` schema.
- Manual edits to `.ao/*.json`.

## Constraints
- Deterministic resolution:
  - exactly one of `run_id` or `task_id` must be provided.
- Request/response only:
  - implementation must not rely on follow/stream loops.
- File safety:
  - run lookup must keep existing safe-run-id validation and scoped lookup
    behavior.
- Compatibility:
  - existing MCP tools and CLI output commands must remain backward-compatible.
- Bounded output:
  - `limit` must be clamped and validated to avoid unbounded payloads.
- Parse robustness:
  - malformed JSONL lines must be skipped without crashing the tool.

## Functional Requirements

### FR-01: New MCP Tool Surface
- Add tool `ao.output.tail` in `ops_mcp.rs`.
- Input schema includes:
  - `run_id` (optional),
  - `task_id` (optional),
  - `limit` (optional),
  - `event_types` (optional array),
  - `project_root` (optional override).
- Validation requires exactly one identifier: `run_id` xor `task_id`.

### FR-02: Deterministic Run Resolution
- If `run_id` is provided, resolve run directory using existing run lookup
  precedence contract.
- If `task_id` is provided:
  - resolve a workflow for that task from current workflow state,
  - resolve the most recent phase run directory for that workflow using
    deterministic ordering.
- If no run can be resolved, return not-found style error payload.

### FR-03: Tail Extraction From Event Log
- Read `<run_dir>/events.jsonl` for the resolved run.
- Parse lines as `AgentRunEvent`; skip invalid lines.
- Keep only events matching the target run id.
- Apply filtering before tail slicing.
- Return last `N` matching events in chronological order (oldest to newest
  within the tail window).

### FR-04: Event-Type Filter Contract
- Supported filter values:
  - `output` -> `AgentRunEvent::OutputChunk`
  - `error` -> `AgentRunEvent::Error`
  - `thinking` -> `AgentRunEvent::Thinking`
- If `event_types` is omitted, default to `output` + `thinking`.
- Unknown filter values must produce deterministic input-validation errors.

### FR-05: Structured MCP Result Shape
- Successful tool result must include structured data with:
  - response schema id,
  - `resolved_run_id`,
  - `resolved_from` (`run_id` or `task_id`),
  - `events_path`,
  - applied `limit`,
  - applied `event_types`,
  - `count`,
  - `events` array.
- Each returned event includes at minimum:
  - normalized event type (`output`/`error`/`thinking`),
  - `run_id`,
  - content text,
  - source event kind metadata (`output_chunk`/`error`/`thinking`),
  - stream metadata when present (`output` entries).

### FR-06: Bounded and Safe Limits
- `limit` defaults to a deterministic value (recommended: `50`).
- `limit` is clamped to a max deterministic bound (recommended: `500`).
- `limit <= 0` must normalize to minimum safe bound (`1`) or reject with
  deterministic validation error (implementation chooses one and tests it).

### FR-07: Regression Safety
- Existing behavior for:
  - `ao.output.run`,
  - `ao.output.monitor`,
  - `ao.output.jsonl`,
  remains unchanged.

### FR-08: Test Coverage
- Add unit tests covering:
  - identifier validation,
  - run resolution behavior,
  - malformed-line skipping,
  - filter mapping,
  - limit clamping and deterministic ordering,
  - structured payload fields.

## Acceptance Criteria
- `AC-01`: `ao.output.tail` exists and is discoverable as an MCP tool.
- `AC-02`: Tool accepts `run_id` xor `task_id` and rejects ambiguous input.
- `AC-03`: Tool reads `events.jsonl` for resolved run and returns bounded last-N
  events.
- `AC-04`: Tool supports `output|error|thinking` filter semantics.
- `AC-05`: Tail results are deterministic and chronologically ordered within the
  tail window.
- `AC-06`: Malformed JSONL lines do not crash the tool.
- `AC-07`: Tool returns structured non-stream MCP responses with explicit
  metadata.
- `AC-08`: Existing output MCP tools remain behaviorally unchanged.
- `AC-09`: Focused tests cover resolution/filter/bounds/error contracts.

## Testable Acceptance Checklist
- `T-01`: `ops_mcp` test for `ao.output.tail` input validation (`run_id` xor
  `task_id`).
- `T-02`: run-id path test validates scoped/legacy lookup compatibility.
- `T-03`: task-id resolution test picks deterministic latest workflow phase run.
- `T-04`: mixed-event fixture test verifies filter mapping and default filter
  behavior.
- `T-05`: limit tests verify default, clamp, and tail ordering.
- `T-06`: malformed-line fixture verifies deterministic skip behavior.
- `T-07`: structured payload test verifies schema id and required metadata
  fields.

## Verification Matrix

| Requirement area | Verification method |
| --- | --- |
| FR-01, FR-05 | `ops_mcp` tool registration + response-shape tests |
| FR-02 | run resolution tests with task/workflow/run fixtures |
| FR-03, FR-04, FR-06 | JSONL parsing, filter, and limit tests on synthetic logs |
| FR-07 | targeted regression tests for existing output MCP tool arg builders |
| FR-08 | focused `orchestrator-cli` unit tests for touched modules |

## Implementation Notes Input (Next Phase)
Primary source targets:
- `crates/orchestrator-cli/src/services/operations/ops_mcp.rs`
- `crates/orchestrator-cli/src/services/operations/ops_output.rs`
- `crates/orchestrator-cli/src/shared/runner.rs` (reuse helpers only as needed)
- `crates/protocol/src/agent_runner.rs` (event-kind contract reference only)

Likely test targets:
- `crates/orchestrator-cli/src/services/operations/ops_mcp.rs` (`#[cfg(test)]`)
- `crates/orchestrator-cli/src/services/operations/ops_output.rs` (`#[cfg(test)]`)

## Deterministic Deliverables for Implementation Phase
- New MCP tool `ao.output.tail` with bounded request/response tail contract.
- Deterministic `task_id`/`run_id` run resolution behavior.
- Event-type filter support (`output`, `error`, `thinking`).
- Focused regression tests proving ordering, bounds, and parse robustness.
