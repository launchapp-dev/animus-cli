# TASK-004 Implementation Notes: llm-cli-wrapper and llm-mcp-server Contract

## Purpose
Translate TASK-004 requirements into a low-risk, deterministic implementation
plan that hardens startup/build/endpoint behavior between:
- `crates/llm-cli-wrapper`
- `crates/llm-mcp-server`

## Non-Negotiable Constraints
- Keep changes scoped to wrapper/MCP integration behavior and related docs/tests.
- Preserve repository-safe operation and deterministic local/CI execution.
- Avoid manual `.ao` state edits.
- Keep behavior loopback-only (`127.0.0.1`) for readiness checks.

## Proposed Change Surface

### 1) Binary path resolution hardening
- File: `crates/llm-cli-wrapper/src/mcp_manager.rs`
- Add deterministic resolution helper with explicit precedence:
  - explicit `with_binary` override,
  - optional environment override,
  - workspace default release binary path.
- Include attempted-path reporting in failure context for faster remediation.

### 2) Build invocation hardening
- File: `crates/llm-cli-wrapper/src/mcp_manager.rs`
- Normalize build command construction so documented command is runnable from
  repository root:
  - `cargo build --release --manifest-path crates/llm-mcp-server/Cargo.toml`
- Capture build exit details and map failures to deterministic error category.

### 3) Endpoint/readiness contract checks
- File: `crates/llm-cli-wrapper/src/mcp_manager.rs`
- Extend readiness from health-only to:
  - `/health`,
  - `/agents`,
  - `/agents/:agent_id` for at least one expected agent (for current server:
    `pm`, `em`, or `review`).
- Keep `get_endpoint` URL format aligned with `llm-mcp-server` routing.

### 4) Deterministic failure surface
- Files: `crates/llm-cli-wrapper/src/error.rs`, `crates/llm-cli-wrapper/src/mcp_manager.rs`
- Introduce stable wrapper error categories for MCP startup path (build failure,
  spawn failure, readiness timeout, endpoint contract violation) with concise
  remediation hints.

### 5) Contract regression tests
- Files:
  - `crates/llm-cli-wrapper/src/mcp_manager.rs` (unit tests),
  - `crates/llm-cli-wrapper/tests/` (new integration test module).
- Coverage targets:
  - binary resolution precedence,
  - build invocation behavior,
  - startup smoke with `/health` + `/agents`/`/agents/:agent_id`,
  - deterministic unavailable/failure error behavior.

## Implementation Sequence
1. Add resolution/build helpers and typed error categories in wrapper.
2. Wire startup flow to use deterministic path/build behavior.
3. Extend readiness checks to endpoint contract assertions.
4. Add/expand unit and integration tests for startup and failure paths.
5. Refresh `crates/llm-cli-wrapper/MCP_INTEGRATION.md` so operator docs match
   actual behavior.

## Risk Notes and Mitigations
- Risk: path-resolution regressions when wrapper is executed outside repo root.
  - Mitigation: precedence tests with temp working directories and explicit
    override/env cases.
- Risk: flaky startup integration tests due to port contention.
  - Mitigation: bind randomized free ports and keep bounded retry windows.
- Risk: doc drift between implementation and usage instructions.
  - Mitigation: treat MCP integration doc update as same-task deliverable and
    validate commands during implementation testing.

## Validation Targets for Implementation Phase
- `cargo test -p llm-cli-wrapper`
- `cargo test -p llm-mcp-server`
- Targeted contract/smoke test execution for new MCP startup and endpoint checks.
