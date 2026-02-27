# TASK-094 Implementation Notes: Profile-Driven AO MCP Wiring in Daemon Spawn

## Phase Context
- Workflow phase: `requirements`
- Workflow ID: `02df5ca9-bd93-49da-9950-19e50a22e44d`
- Task: `TASK-094`
- Prepared: `2026-02-27`

## Implementation Intent
Implement profile-driven MCP wiring only for daemon phase agent spawning, using
existing runner-native MCP enforcement paths rather than introducing parallel
policy mechanisms.

## Guardrails
- Keep changes narrow to profile parsing, runtime-contract assembly, daemon spawn
  wiring, and runner policy enforcement inputs.
- Preserve current non-daemon env-based MCP path unless explicitly overridden.
- Keep fail-closed semantics under explicit MCP-only enforcement.
- Do not manually edit `/.ao/*.json`.

## Planned Change Sequence

### 1) Extend profile schema access for MCP config
Target:
- `crates/orchestrator-core/src/agent_runtime_config.rs`
- `crates/orchestrator-core/config/agent-runtime-config.v2.json`

Work:
- Add typed profile fields for `mcp_servers` and `tool_policy` (with optional
  server-level policy override).
- Add helper accessors to resolve effective policy for a selected phase agent.
- Validate minimum constraints for TASK-094 path (AO built-in server entry).

### 2) Expand runtime-contract MCP input surface
Target:
- `crates/orchestrator-core/src/runtime_contract.rs`

Work:
- Extend contract builder input(s) to accept explicit MCP transport details
  (including stdio command/args) and explicit allowed prefixes.
- Keep old call patterns compatible to avoid regressions in unrelated flows.

### 3) Add explicit MCP inputs in shared runner helper
Target:
- `crates/orchestrator-cli/src/shared/runner.rs`

Work:
- Preserve existing env-derived fallback behavior.
- Add overload/helper path so daemon scheduler can pass resolved profile MCP
  contract inputs directly.

### 4) Wire daemon phase spawn to selected profile MCP config
Target:
- `crates/orchestrator-cli/src/services/runtime/runtime_daemon/daemon_scheduler_phase_exec.rs`

Work:
- Resolve selected phase agent profile once per run path.
- Map `mcp_servers.ao` built-in config to stdio launch:
  `ao --project-root <project_root> mcp serve`.
- Resolve effective tool policy (server override > profile policy) and map to
  normalized prefix list.
- Build runtime contract with explicit MCP payload before submitting
  `AgentRunRequest`.

### 5) Ensure runner enforcement honors explicit profile scope
Target:
- `crates/agent-runner/src/runner/process.rs`

Work:
- Ensure explicit `mcp.allowed_tool_prefixes` from profile policy is treated as
  authoritative for enforcement.
- Retain existing fail-closed transport/adapter checks.
- Keep native lock-down behavior for codex/claude/gemini/opencode.

## Test Plan (Implementation Phase)
- `cargo test -p orchestrator-core agent_runtime_config`
- `cargo test -p orchestrator-core runtime_contract`
- `cargo test -p orchestrator-cli daemon_scheduler`
- `cargo test -p agent-runner runner::process`

## Risk Register
- Risk: policy translation broadens scope accidentally.
  Mitigation: add exact-prefix translation tests; reject invalid/empty policy
  output in enforced mode.
- Risk: runtime-contract API change breaks legacy callers.
  Mitigation: additive API shape + compatibility tests.
- Risk: AO binary resolution differs by execution context.
  Mitigation: centralize binary/args resolution and test resulting argv.
- Risk: duplicated source-of-truth between env and profile paths.
  Mitigation: daemon path always prefers explicit profile-derived MCP contract.

## Done Criteria for Implementation Phase
- Daemon profile MCP config deterministically drives runtime-contract MCP fields.
- Runner receives and enforces profile-derived AO tool scope.
- Native provider MCP lock-down remains functional.
- Targeted tests pass in all touched crates.
