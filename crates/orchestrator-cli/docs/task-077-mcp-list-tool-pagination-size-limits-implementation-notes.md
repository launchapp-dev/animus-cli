# TASK-077 Implementation Notes: MCP List Pagination and Response Size Guardrails

## Phase Context
- Workflow phase: `requirements`
- Workflow ID: `054d2a70-95b3-4561-9ab6-081e0a2cf81c`
- Task: `TASK-077`

## Purpose
Translate TASK-077 requirements into a deterministic implementation slice that
prevents oversized MCP list responses while preserving list usability.

## Non-Negotiable Constraints
- Keep the change scoped to MCP list tools and list-result shaping.
- Do not break non-list MCP tools.
- Keep ordering deterministic and metadata explicit.
- Do not manually edit `.ao/*.json`.

## Chosen Strategy
- Implement guardrails in `ops_mcp.rs` at the MCP boundary, after AO CLI list
  command execution and before returning `CallToolResult`.
- Add `limit`/`offset`/`max_tokens` directly to list tool input schemas so
  callers can control pagination and token budget.
- Use deterministic fallback levels (`full` -> `summary_fields` ->
  `summary_only`) when paged payloads exceed `max_tokens`.

This approach avoids broad core/service refactors and addresses the immediate
MCP context safety issue where responses are consumed.

## Proposed Change Surface

### 1) Add Shared List Guard Input Types
Target: `crates/orchestrator-cli/src/services/operations/ops_mcp.rs`

- Introduce list-capable input structs for list tools currently using
  `ProjectRootInput` or `IdInput` only.
- Add optional fields:
  - `limit: Option<usize>`
  - `offset: Option<usize>`
  - `max_tokens: Option<usize>`

### 2) Add Deterministic Pagination Normalization
Target: `ops_mcp.rs`

- Add constants and helpers:
  - default/max/min `limit`,
  - default/max/min `max_tokens`,
  - normalized `offset`.
- Reuse existing clamp style used by daemon/output MCP tooling.

### 3) Add Generic List Result Shaper
Target: `ops_mcp.rs`

- Add helper that:
  - identifies list tools in scope,
  - expects array result payload,
  - computes `total`, applies `offset` and `limit`,
  - emits structured list response with pagination metadata.
- For non-array list results, return deterministic structured error.

### 4) Add Size Guardrail and Summarization Fallback
Target: `ops_mcp.rs`

- Add deterministic token estimate helper:
  - `estimated_tokens = ceil(serialized_char_count / 4.0)`.
- Implement fallback sequence:
  1. full paged items,
  2. tool-specific field summaries,
  3. summary-only digest.
- Attach `size_guard` metadata:
  - hint, estimate, mode, truncated flag.

### 5) Add Tool-Specific Summary Profiles
Target: `ops_mcp.rs`

- Define summary field profiles for:
  - `ao.project.list`
  - `ao.task.list`
  - `ao.requirements.list`
  - `ao.workflow.list`
  - `ao.workflow.checkpoints.list`
- Keep profiles deterministic and stable for tests.

### 6) Wire List Handlers Through New Guarded Path
Target: `ops_mcp.rs`

- Keep existing `run_tool` path for non-list tools.
- Route in-scope list tools through a list-aware path that applies pagination
  and size guardrails before building `CallToolResult::structured`.

### 7) Tests
Primary target: `ops_mcp.rs` `#[cfg(test)]`

- Add tests for:
  - normalization defaults/clamps,
  - offset/limit pagination correctness and `next_offset`,
  - mode transitions under shrinking `max_tokens`,
  - workflow-list large fixture using summary fallback,
  - non-list regression (unchanged shape/behavior).

## Suggested Implementation Sequence
1. Introduce list input fields and normalization constants/helpers.
2. Add shared list response model + pagination shaper.
3. Add token estimation + summary fallback pipeline.
4. Wire list tool handlers to new path.
5. Add tests for pagination, guardrails, and non-list regression.
6. Run targeted crate tests and fix regressions.

## Validation Targets
- `cargo test -p orchestrator-cli services::operations::ops_mcp`
- Optional broader safety run:
  - `cargo test -p orchestrator-cli`

## Risks and Mitigations
- Risk: response-shape surprises for existing MCP consumers.
  - Mitigation: keep schema explicit and preserve top-level `tool` key.
- Risk: inaccurate token estimation.
  - Mitigation: deterministic approximation plus summary-only hard fallback.
- Risk: oversized workflow objects still leak through.
  - Mitigation: workflow-specific summary profile and strict mode downgrade.
- Risk: pagination logic drift across tools.
  - Mitigation: one shared list shaper with per-tool profile config only.

## Deliverables for Next Phase
- MCP list tools with deterministic pagination and token-hint controls.
- Structured list metadata for paging and truncation visibility.
- Automatic summarization guardrails preventing context-window blowups.
- Focused tests proving bounded behavior and regression safety.
