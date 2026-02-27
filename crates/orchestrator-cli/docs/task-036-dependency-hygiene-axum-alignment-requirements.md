# TASK-036 Requirements: Dead Dependency Cleanup and Axum Version Alignment

## Phase
- Workflow phase: `requirements`
- Workflow ID: `d0eba80a-7aef-4127-b8ce-8d45822a23b6`
- Task: `TASK-036`

## Objective
Define a deterministic, repository-safe dependency cleanup that removes an
unused crate and eliminates Axum major-version drift across workspace crates.

## Current Baseline Audit

| Surface | Current location | Current behavior | Gap |
| --- | --- | --- | --- |
| Dead process dependency | `crates/llm-cli-wrapper/Cargo.toml` | declares `tokio-process = "0.2"` | no `tokio_process` usage in `crates/llm-cli-wrapper/src` |
| Axum in CLI crate | `crates/orchestrator-cli/Cargo.toml` | `axum = "0.8"` | version diverges from other Axum users |
| Axum in web server crate | `crates/orchestrator-web-server/Cargo.toml` | `axum = { version = "0.7", features = ["macros"] }` | major-version mismatch with CLI |
| Axum in MCP server crate | `crates/llm-mcp-server/Cargo.toml` | `axum = { version = "0.7", features = ["ws"] }` | major-version mismatch with CLI |
| Workspace dependency governance | root `Cargo.toml` | no shared `axum` entry under `[workspace.dependencies]` | no single source of truth for Axum version |

## Scope
In scope for implementation after this requirements phase:
- Remove `tokio-process = "0.2"` from `crates/llm-cli-wrapper/Cargo.toml`.
- Align direct Axum dependency declarations in:
  - `crates/orchestrator-cli/Cargo.toml`
  - `crates/orchestrator-web-server/Cargo.toml`
  - `crates/llm-mcp-server/Cargo.toml`
  to a single major line (`0.8`).
- Preserve required Axum feature coverage:
  - `macros` for `orchestrator-web-server`
  - `ws` for `llm-mcp-server`
- Apply only minimal companion dependency adjustments if required for Axum
  `0.8` compatibility.

Out of scope for this task:
- Endpoint, protocol, or runtime behavior redesign.
- Refactors unrelated to dependency/version alignment.
- Manual edits to `.ao/*.json`.

## Constraints
- Keep changes deterministic and limited to the dependency-alignment surface.
- Preserve existing HTTP route behavior and MCP transport behavior.
- Avoid unnecessary dependency churn (no broad ecosystem upgrades).
- Keep all first-class workspace crates buildable after changes.
- Keep `.ao` mutation policy intact (CLI-driven only).

## Functional Requirements

### FR-01: Remove Unused `tokio-process`
- `llm-cli-wrapper` must no longer declare `tokio-process`.
- No source files in `llm-cli-wrapper` may rely on `tokio-process` symbols.

### FR-02: Single Axum Major Across Workspace Axum Consumers
- All workspace crates that directly depend on Axum in this task must use Axum
  `0.8`.
- Dependency declarations must be consistent and auditable.

### FR-03: Preserve Axum Feature Requirements
- `orchestrator-web-server` must keep `macros` support.
- `llm-mcp-server` must keep WebSocket (`ws`) support.

### FR-04: Compatibility-Focused Companion Updates
- Update `tower`/`tower-http` declarations only if required to compile with the
  chosen Axum version.
- Keep updates minimal and directly justified by compiler compatibility.

### FR-05: Build Integrity
- Workspace crates touched by this task must pass `cargo check` after changes.
- No new warnings/errors introduced by dependency version alignment.

## Acceptance Criteria
- `AC-01`: `crates/llm-cli-wrapper/Cargo.toml` no longer contains
  `tokio-process`.
- `AC-02`: Repository search confirms no active `tokio_process` usage remains
  in `crates/llm-cli-wrapper/src`.
- `AC-03`: Direct Axum dependency declarations for
  `orchestrator-cli`/`orchestrator-web-server`/`llm-mcp-server` align to Axum
  `0.8`.
- `AC-04`: Required Axum feature flags remain present (`macros`, `ws`) in the
  relevant crates.
- `AC-05`: `cargo check -p llm-cli-wrapper -p llm-mcp-server -p orchestrator-web-server -p orchestrator-cli`
  succeeds.
- `AC-06`: No unrelated dependency/version edits are introduced outside task
  scope.

## Testable Acceptance Checklist
- `T-01`: manifest diff verifies `tokio-process` removal from
  `llm-cli-wrapper`.
- `T-02`: manifest diff verifies Axum `0.8` alignment in all three target
  crates.
- `T-03`: build verification command for touched crates succeeds.
- `T-04`: targeted source compile checks confirm no API breakage from Axum
  upgrade path.

## Acceptance Verification Matrix

| Requirement area | Verification method |
| --- | --- |
| FR-01 | manifest diff + `rg -n "tokio_process"` in `llm-cli-wrapper/src` |
| FR-02 | manifest diff across three crates and/or workspace dependency source of truth |
| FR-03 | manifest review of retained Axum feature flags |
| FR-04 | compile verification after any companion version adjustments |
| FR-05 | targeted `cargo check` across touched crates |

## Implementation Notes (Input to Next Phase)
Primary code/manifests to edit:
- `Cargo.toml` (workspace dependency source-of-truth, if adopted)
- `crates/llm-cli-wrapper/Cargo.toml`
- `crates/orchestrator-web-server/Cargo.toml`
- `crates/llm-mcp-server/Cargo.toml`
- `crates/orchestrator-cli/Cargo.toml` (if declaration normalization is needed)

Potential compatibility touchpoints if compile requires source updates:
- `crates/orchestrator-web-server/src/services/web_server.rs`
- `crates/llm-mcp-server/src/http.rs`
- `crates/orchestrator-cli/src/services/tui/mcp_bridge.rs`

## Deterministic Deliverables for Implementation Phase
- Dead dependency removed from `llm-cli-wrapper`.
- Axum major version aligned to `0.8` for all targeted crates.
- Any required minimal companion dependency tweaks captured in manifest diffs.
- Successful targeted `cargo check` evidence for touched crates.
