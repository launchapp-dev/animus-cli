# TASK-077 Requirements: MCP List Tool Pagination and Response Size Guardrails

## Phase
- Workflow phase: `requirements`
- Workflow ID: `054d2a70-95b3-4561-9ab6-081e0a2cf81c`
- Task: `TASK-077`
- Snapshot date: `2026-02-27`

## Objective
Prevent MCP list tool responses from exceeding context limits by introducing:
- default pagination (`limit`/`offset`) on all MCP list tools,
- automatic response summarization when payloads remain too large,
- a `max_tokens` hint parameter to bound returned content size deterministically.

The tools must remain request/response safe and must not emit context-window
blowing payloads.

## Current Baseline Audit

| Surface | Current location | Current behavior | Gap |
| --- | --- | --- | --- |
| MCP tool execution wrapper | `crates/orchestrator-cli/src/services/operations/ops_mcp.rs` (`run_tool`) | executes AO CLI and returns parsed `data` as `result` | no generic pagination, no token/size guardrail contract |
| List tool schemas | `ops_mcp.rs` input types (`ProjectRootInput`, `TaskListInput`, `IdInput`) | no `limit`, `offset`, or `max_tokens` for list tools | callers cannot bound response size |
| List summarization helper | `ops_mcp.rs` (`summarize_list_if_needed`) | always trims fields only for `ao.task.list`, `ao.task.prioritized`, `ao.requirements.list` | not size-aware, no token budget, no workflow/project/checkpoint protection |
| Underlying CLI list handlers | `runtime_project_task/task.rs`, `runtime_project_task/project.rs`, `ops_requirements.rs`, `ops_workflow.rs` | list commands return full arrays from services | MCP proxies unbounded arrays from these commands |
| Existing bounded MCP patterns | `ops_mcp.rs` (`daemon_events_poll_limit`, `output_tail_limit`) | deterministic default+clamp limit with structured metadata | list tools do not reuse this bounded pattern |

## Scope
In scope for implementation after this requirements phase:
- Add pagination inputs (`limit`, `offset`) and token hint input (`max_tokens`)
  to all MCP list tools:
  - `ao.project.list`
  - `ao.task.list`
  - `ao.requirements.list`
  - `ao.workflow.list`
  - `ao.workflow.checkpoints.list`
- Add shared bounded pagination behavior with deterministic defaults and clamps.
- Add automatic size-aware summarization fallback for oversized list payloads.
- Return structured list metadata (pagination and size guard details) with each
  list response.
- Add focused tests for pagination, token-hint enforcement, summarization
  fallback, and non-list regression behavior.

Out of scope:
- Changing non-list MCP tool contracts.
- Streaming MCP responses.
- Manual edits to `.ao/*.json`.
- Rewriting underlying core service list APIs in this task slice.

## Constraints
- Determinism:
  - preserve source ordering from underlying CLI list output,
  - pagination is stable for identical source data and parameters.
- Safety:
  - list responses must always be bounded by paging and size-guard behavior.
- Compatibility:
  - non-list MCP tool behavior remains unchanged.
- Schema stability:
  - list responses provide explicit metadata about pagination and summarization.
- Repository safety:
  - implementation remains in Rust crate surfaces; no direct state-file patching.

## Response Contract

### List Input Contract
All in-scope list tools accept optional:
- `limit: usize`
- `offset: usize`
- `max_tokens: usize`

Normalization:
- `limit`: default `25`, min `1`, max `200`
- `offset`: default `0`
- `max_tokens`: default `3000`, min `256`, max `12000`

### List Output Contract
Each list tool response includes:
- `schema`: `ao.mcp.list.result.v1`
- `tool`
- `items`: paged items after size-guard processing
- `pagination`:
  - `limit`
  - `offset`
  - `returned`
  - `total`
  - `has_more`
  - `next_offset` (nullable)
- `size_guard`:
  - `max_tokens_hint`
  - `estimated_tokens`
  - `mode` (`full` | `summary_fields` | `summary_only`)
  - `truncated` (`true` when summary mode changed payload detail)

### Summarization Modes
Deterministic fallback sequence:
1. Try paged full items.
2. If estimated tokens exceed `max_tokens`, apply tool-specific summary fields.
3. If still oversized, return `summary_only` payload with:
   - counts/ids/status-oriented digest,
   - no verbose nested blobs.

## Functional Requirements

### FR-01: List Tool Schema Expansion
Add `limit`, `offset`, `max_tokens` inputs to all in-scope MCP list tools.

### FR-02: Deterministic Pagination
List tools must apply normalized `offset` then `limit` to the source list and
return deterministic pagination metadata.

### FR-03: Size Estimation and Guarding
Implement deterministic token estimation (`serialized_json_char_count / 4`,
rounded up) and enforce `max_tokens` using the summarization mode sequence.

### FR-04: Tool-Specific Summary Field Profiles
Define stable summary field allow-lists for each in-scope list tool so large
entries (notably workflow payloads) can be reduced consistently.

### FR-05: Summary-Only Hard Fallback
If summarized page payload still exceeds `max_tokens`, return a compact
`summary_only` digest with deterministic metadata and counts.

### FR-06: Bounded Defaults and Clamps
`limit` and `max_tokens` must use deterministic defaults and clamps; zero or
empty-equivalent values normalize to minimum safe values.

### FR-07: Non-List Regression Safety
All non-list MCP tools keep current behavior and payload shape.

### FR-08: Test Coverage
Add tests for:
- input normalization (`limit`/`offset`/`max_tokens`),
- pagination metadata correctness,
- summary mode transitions (`full` -> `summary_fields` -> `summary_only`),
- deterministic ordering and `next_offset`,
- non-list tool behavior unchanged.

## Acceptance Criteria
- `AC-01`: All in-scope MCP list tools accept `limit`, `offset`, and
  `max_tokens`.
- `AC-02`: With defaults, list tools return bounded page-size responses.
- `AC-03`: `offset` and `limit` produce deterministic `items`, `has_more`, and
  `next_offset`.
- `AC-04`: Oversized full payloads automatically downgrade to summary fields.
- `AC-05`: If still oversized, tools return deterministic `summary_only`
  responses.
- `AC-06`: Response metadata reports applied pagination and size-guard mode.
- `AC-07`: Non-list MCP tools remain behaviorally unchanged.
- `AC-08`: Tests cover pagination, size guardrails, and regression safety.

## Testable Acceptance Checklist
- `T-01`: parse/normalize tests for list input defaults and clamps.
- `T-02`: pagination tests for `offset`, `limit`, `total`, `has_more`,
  `next_offset`.
- `T-03`: workflow-list fixture test that triggers `summary_fields` mode.
- `T-04`: forced low `max_tokens` test that triggers `summary_only` mode.
- `T-05`: tool-specific summary profile tests verify retained fields are stable.
- `T-06`: regression tests verify non-list tools retain existing response shape.

## Verification Matrix

| Requirement area | Verification method |
| --- | --- |
| FR-01, FR-02, FR-06 | `ops_mcp.rs` unit tests for input and pagination metadata |
| FR-03, FR-04, FR-05 | list-size guard tests with large synthetic workflow/task payloads |
| FR-07 | targeted non-list MCP regression assertions |
| FR-08 | focused `cargo test -p orchestrator-cli services::operations::ops_mcp` |

## Implementation Notes Input (Next Phase)
Primary target:
- `crates/orchestrator-cli/src/services/operations/ops_mcp.rs`

Likely touched areas within that file:
- list input structs and tool handlers,
- generic list-result shaping helper,
- pagination normalization constants/helpers,
- size estimation and summarization fallback helpers,
- list-tool-specific summary field profiles,
- `#[cfg(test)]` coverage for new list guardrails.

Secondary targets only if needed:
- list command arg wiring surfaces in CLI types/runtime handlers, if deciding to
  push pagination deeper than MCP post-processing.

## Deterministic Deliverables for Implementation Phase
- All MCP list tools support bounded pagination and token-hint input.
- Structured, size-safe list responses with explicit pagination metadata.
- Automatic summarization fallback that prevents context-window blowups.
- Focused tests proving deterministic, bounded behavior.
