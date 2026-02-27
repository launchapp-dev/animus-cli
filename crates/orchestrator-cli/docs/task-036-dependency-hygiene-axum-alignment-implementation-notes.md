# TASK-036 Implementation Notes: Dead Dependency Cleanup and Axum Version Alignment

## Purpose
Translate TASK-036 requirements into a minimal implementation slice that removes
unused dependency declarations and aligns Axum versions without widening
behavioral scope.

## Non-Negotiable Constraints
- Keep changes scoped to workspace manifests and compile-fix source updates only
  if required by Axum `0.8` compatibility.
- Preserve runtime behavior for existing HTTP/MCP flows.
- Preserve required Axum feature coverage (`macros`, `ws`).
- Avoid unrelated dependency churn.
- Do not manually edit `.ao/*.json`.

## Proposed Change Surface

### 1) Remove dead dependency from `llm-cli-wrapper`
- File: `crates/llm-cli-wrapper/Cargo.toml`
- Remove:
  - `tokio-process = "0.2"`
- Confirm no `tokio_process` imports/usages in
  `crates/llm-cli-wrapper/src/**`.

### 2) Align Axum version declarations to `0.8`
- Target manifests:
  - `crates/orchestrator-cli/Cargo.toml`
  - `crates/orchestrator-web-server/Cargo.toml`
  - `crates/llm-mcp-server/Cargo.toml`
- Preferred deterministic pattern:
  - add `axum = "0.8"` in root `[workspace.dependencies]`
  - consume via `workspace = true` in crate manifests, preserving crate-specific
    feature flags (`macros`, `ws`) where required.

### 3) Keep compatibility updates minimal
- If Axum `0.8` requires companion updates:
  - adjust `tower` and/or `tower-http` versions only where compiler demands it.
- Do not proactively bump unrelated dependencies.

### 4) Source compatibility touchpoints (only if compiler indicates)
- `crates/orchestrator-web-server/src/services/web_server.rs`
- `crates/llm-mcp-server/src/http.rs`
- `crates/orchestrator-cli/src/services/tui/mcp_bridge.rs`
- Apply smallest possible API-adaptation changes required to compile.

## Suggested Implementation Sequence
1. Remove `tokio-process` from `llm-cli-wrapper` manifest.
2. Normalize Axum declarations to `0.8` across targeted manifests (prefer
   workspace source of truth).
3. Run targeted checks and resolve only compatibility errors introduced by step
   2.
4. Re-run targeted checks until clean.
5. Verify manifest diff is limited to task scope.

## Validation Plan
- Dependency usage check:
  - `rg -n "tokio_process|tokio-process" crates/llm-cli-wrapper`
- Build checks:
  - `cargo check -p llm-cli-wrapper -p llm-mcp-server -p orchestrator-web-server -p orchestrator-cli`
- Optional focused tests if needed after API adjustments:
  - `cargo test -p llm-mcp-server`
  - `cargo test -p orchestrator-web-server`

## Risks and Mitigations
- Risk: Axum `0.8` introduces API mismatches in server code.
  - Mitigation: apply narrow compile-driven changes only in affected files.
- Risk: companion crates (`tower`, `tower-http`) mismatch after Axum update.
  - Mitigation: bump only the minimum compatible versions in touched manifests.
- Risk: feature regression from dependency normalization.
  - Mitigation: explicitly preserve `macros` and `ws` features in manifests.

## Completion Evidence Expected in Implementation Phase
- Manifest diffs show:
  - removed `tokio-process` from `llm-cli-wrapper`
  - Axum `0.8` alignment in target crates
- Targeted `cargo check` command succeeds.
- No unrelated files changed beyond task scope.
