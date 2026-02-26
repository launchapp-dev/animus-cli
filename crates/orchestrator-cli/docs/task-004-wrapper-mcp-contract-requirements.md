# TASK-004 Requirements: llm-cli-wrapper and llm-mcp-server Contract Hardening

## Phase
- Workflow phase: `requirements`
- Workflow ID: `744dbf14-b316-4323-baac-6a5d452fd4d6`
- Task: `TASK-004`
- Linked requirement: `REQ-004`

## Objective
Define a deterministic integration contract between `llm-cli-wrapper` and
`llm-mcp-server` covering:
- MCP server binary path resolution,
- build/start invocation behavior,
- endpoint and readiness checks,
- and deterministic failure semantics when MCP is unavailable.

## Existing Baseline Audit

| Coverage area | Current location | Current state | Gap |
| --- | --- | --- | --- |
| MCP binary resolution | `crates/llm-cli-wrapper/src/mcp_manager.rs` | `McpServerManager::new` defaults to `crates/llm-mcp-server/target/release/llm-mcp-server`; `with_binary` allows override | default is current-working-directory dependent and does not define explicit precedence across execution contexts |
| MCP build invocation | `crates/llm-cli-wrapper/src/mcp_manager.rs` | `build_server` runs `cargo build --release --manifest-path crates/llm-mcp-server/Cargo.toml` | command is also cwd-dependent; failures are not classified for predictable remediation |
| Readiness validation | `crates/llm-cli-wrapper/src/mcp_manager.rs` | `wait_for_ready` polls only `GET /health` | no assertion on endpoint discovery (`/agents`) or agent endpoint availability |
| Server endpoint semantics | `crates/llm-mcp-server/src/http.rs`, `crates/llm-mcp-server/src/main.rs` | exposes `/health`, `/agents`, `/agents/:agent_id`, `/mcp/:agent_id`; main currently registers `pm`, `em`, `review` | wrapper-side checks do not validate these contract expectations |
| Contract regression coverage | `crates/llm-cli-wrapper/src/mcp_manager.rs` tests | verifies endpoint string formatting and custom binary setter | no integration smoke proving build/startup plus health and agent endpoint behavior |

## Scope
In scope for the implementation phase after this requirements pass:
- Define deterministic binary resolution precedence for MCP server startup.
- Define build invocation contract that is executable from repository root.
- Extend readiness checks from health-only to contract-level endpoint checks.
- Define deterministic wrapper error contract for MCP server unavailability with
  remediation guidance.
- Add contract tests that fail on startup/build/endpoint-regression behavior.

Out of scope:
- Changing MCP JSON-RPC protocol method semantics.
- Reworking agent-runner runtime policy behavior.
- Expanding tool catalog behavior in `llm-mcp-server`.

## Constraints
- Keep implementation Rust-only inside workspace crates.
- Do not manually edit `.ao/*.json`; phase artifacts live in repo docs/code.
- Keep all network checks local to loopback (`127.0.0.1`).
- Keep contract tests deterministic in fresh temp directories and isolated ports.
- Keep behavior repository-safe; no destructive git commands in validation flows.

## Functional Requirements

### FR-01: Binary Resolution Contract
- Wrapper must resolve MCP server binary using deterministic precedence:
  1. explicit `with_binary(...)` path,
  2. optional environment override (`LLM_MCP_SERVER_BINARY`),
  3. workspace-resolved default release path for `llm-mcp-server`.
- Resolution failures must report attempted location(s) and next-step remediation.

### FR-02: Build Invocation Contract
- If resolved binary is missing, wrapper must invoke a documented build command:
  - `cargo build --release --manifest-path crates/llm-mcp-server/Cargo.toml`
- Build invocation must be executable from repository root.
- Build failure output must map to a deterministic wrapper error classification.

### FR-03: Endpoint and Readiness Contract
- Wrapper readiness must verify:
  - `GET /health` succeeds and returns MCP server health payload.
  - `GET /agents` succeeds and returns agent discovery payload.
  - at least one expected agent endpoint is resolvable via
    `GET /agents/:agent_id` and endpoint path `/mcp/:agent_id`.
- Endpoint URL generation and verification must stay consistent with
  `llm-mcp-server` routing contract.

### FR-04: Unavailability/Error Contract
- MCP unavailability/build/startup failures must return deterministic, structured
  wrapper errors with:
  - stable error code/category,
  - concise failure reason,
  - explicit remediation hint (for example: build command or endpoint to probe).

### FR-05: CI Contract Regression Coverage
- Contract tests must cover:
  - binary resolution behavior,
  - startup + health + agent endpoint smoke path,
  - deterministic unavailable/failure error behavior.
- CI must execute these tests so startup/endpoint regressions fail fast.

## Acceptance Criteria
- `AC-01`: A documented integration contract specifies required startup inputs,
  endpoint discovery behavior, and expected health semantics.
- `AC-02`: A reproducible smoke test validates server startup plus at least one
  health and one agent-related endpoint interaction.
- `AC-03`: Wrapper behavior on MCP server unavailability returns deterministic,
  structured error output with clear remediation hint.
- `AC-04`: Build and startup commands used by the integration path are
  documented and executable from repository root.
- `AC-05`: Contract tests run in CI and fail on breaking changes to
  wrapper-to-server startup or endpoint resolution behavior.

## Verification Matrix

| Requirement | Verification method |
| --- | --- |
| `AC-01` | This requirements doc + aligned implementation notes checked in repo |
| `AC-02` | New integration smoke test that starts MCP server and validates `/health` + `/agents` or `/agents/:agent_id` |
| `AC-03` | Unit/integration test asserting structured unavailability error code and remediation text |
| `AC-04` | Documentation + test setup executing documented build/start from repo-root context |
| `AC-05` | `cargo test -p llm-cli-wrapper` path in CI covering new contract tests |

## Deterministic Deliverables for Implementation Phase
- `llm-cli-wrapper` startup contract hardening in `src/mcp_manager.rs`.
- Error contract additions in wrapper error surface (`src/error.rs` and/or typed
  startup error helpers).
- New contract-focused tests under `crates/llm-cli-wrapper/tests/`.
- Updated MCP integration documentation in `crates/llm-cli-wrapper/` to match
  final startup/build/endpoint contract.
