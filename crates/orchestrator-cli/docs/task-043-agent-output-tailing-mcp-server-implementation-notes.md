# TASK-043 Implementation Notes: MCP Agent Output Tailing

## Phase Context
- Workflow phase: `requirements`
- Workflow ID: `e63386f5-83d5-421b-9078-c5db95b6980a`
- Task: `TASK-043`

## Purpose
Translate TASK-043 requirements into a concrete implementation slice for an MCP
tool that provides bounded, filterable tail visibility into agent phase output
without streaming semantics.

## Non-Negotiable Constraints
- Keep changes scoped to `orchestrator-cli` (plus protocol references only).
- Keep MCP tool behavior request/response only.
- Preserve existing output tool behavior (`ao.output.run|monitor|jsonl`).
- Keep run lookup deterministic and repository-safe.
- Do not manually edit `.ao/*.json`.

## Proposed Change Surface

### 1) Add `ao.output.tail` MCP Contract
- Target: `crates/orchestrator-cli/src/services/operations/ops_mcp.rs`
- Add new MCP input type (example fields):
  - `run_id: Option<String>`
  - `task_id: Option<String>`
  - `limit: Option<usize>`
  - `event_types: Option<Vec<String>>`
  - `project_root: Option<String>`
- Add new tool handler:
  - validates identifier xor semantics,
  - resolves run id,
  - returns structured payload via `CallToolResult::structured`,
  - emits structured error payloads for invalid input / not found / parse
    failures.

### 2) Add Reusable Output Tail Helpers
- Target: `crates/orchestrator-cli/src/services/operations/ops_output.rs`
- Extract/introduce helpers for:
  - reading and parsing `events.jsonl` into `AgentRunEvent`,
  - filtering by run id and selected event types,
  - applying deterministic tail slicing by limit,
  - shaping normalized tail records for MCP consumption.
- Reuse existing run-dir candidate resolution (`resolve_run_dir_for_lookup`)
  rather than duplicating path logic in MCP module.

### 3) Deterministic `task_id` -> `run_id` Resolution
- Primary intent: support tailing when caller knows task id but not phase run id.
- Candidate resolution strategy:
  - find workflow(s) for `task_id` from current workflow state,
  - choose deterministic workflow candidate (prefer running, then most recent
    started/completed),
  - match run directories using known phase run-id prefix pattern
    (`wf-<workflow_id>-...`),
  - select most recent run directory containing `events.jsonl`.
- Return not-found error if no candidate run directory exists.

### 4) Event Filter and Response Semantics
- Normalize event filters:
  - `output` => `OutputChunk`
  - `error` => `Error`
  - `thinking` => `Thinking`
- Default filter set when omitted:
  - `output`, `thinking`.
- Return each event with stable fields:
  - normalized type,
  - run id,
  - text content,
  - source event kind,
  - stream type for output chunks when available.
- Include response metadata:
  - schema id (`ao.output.tail.v1`),
  - resolution mode (`run_id` or `task_id`),
  - resolved run id,
  - events path,
  - applied limit/filter,
  - count.

### 5) Tests
- `ops_mcp.rs`:
  - input-validation tests for xor identifier rules,
  - response-shape tests for successful tail requests,
  - error-shape tests for invalid filter values / missing run data.
- `ops_output.rs`:
  - parsing tests for mixed `AgentRunEvent` fixtures,
  - malformed-line skip tests,
  - limit/tail ordering tests,
  - filter mapping tests.
- Add deterministic fixtures using temp directories and existing env-lock guard
  patterns already used in MCP/output tests.

## Suggested Implementation Sequence
1. Define tail record/filter types and parsing helpers in `ops_output.rs`.
2. Implement deterministic task->workflow->run resolution helper.
3. Add `ao.output.tail` MCP tool and response shaping in `ops_mcp.rs`.
4. Add unit tests for helper and tool behavior.
5. Run targeted tests and fix regressions.

## Validation Targets
- `cargo test -p orchestrator-cli services::operations::ops_output`
- `cargo test -p orchestrator-cli services::operations::ops_mcp`
- Optional broader pass:
  - `cargo test -p orchestrator-cli`

## Risks and Mitigations
- Risk: ambiguous task->run resolution when multiple workflows/runs exist.
  - Mitigation: encode deterministic tie-break rules and test them explicitly.
- Risk: large run logs causing oversized MCP payloads.
  - Mitigation: enforce default and max limits, tail after filter.
- Risk: behavior drift across output surfaces.
  - Mitigation: keep helper logic centralized and preserve existing command
    behavior with regression tests.
- Risk: malformed JSON lines in event log.
  - Mitigation: skip invalid entries deterministically; never crash the tool.

## Deliverables for Next Phase
- Implemented MCP tool `ao.output.tail`.
- Deterministic run-resolution and tail/filter helpers.
- Focused tests proving correctness of resolution, filters, and bounded output.
