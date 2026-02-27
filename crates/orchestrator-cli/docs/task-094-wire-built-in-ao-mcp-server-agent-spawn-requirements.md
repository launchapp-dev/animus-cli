# TASK-094 Requirements: Wire Built-In AO MCP Server into Daemon Agent Spawn with Per-Profile Tool Scoping

## Phase Context
- Workflow phase: `requirements`
- Workflow ID: `02df5ca9-bd93-49da-9950-19e50a22e44d`
- Task: `TASK-094`
- Baseline audited: `2026-02-27`

## Objective
When daemon workflow execution spawns an agent, runtime contract generation must
be driven by the selected agent profile's MCP configuration so the runner:
- connects that agent to AO's built-in MCP server (`ao mcp serve`), and
- enforces profile-scoped MCP tool access through deterministic prefix filters.

## Current Baseline (Audited)

| Surface | Location | Current behavior | Gap to close |
| --- | --- | --- | --- |
| Phase agent profile resolution | `crates/orchestrator-cli/src/services/runtime/runtime_daemon/daemon_scheduler_phase_exec.rs` (`phase_agent_id_for`, `run_workflow_phase_with_agent`) | phase agent id is resolved, but runtime contract build is generic (`build_runtime_contract(tool, model, prompt)`) | no profile MCP server or tool-policy data is propagated into runtime contract |
| Runtime-contract helper entrypoint | `crates/orchestrator-cli/src/shared/runner.rs` (`build_runtime_contract`) | MCP endpoint/agent id sourced from env (`AO_MCP_ENDPOINT`, `MCP_ENDPOINT`, `OPENCODE_MCP_ENDPOINT`, `AO_MCP_AGENT_ID`) | daemon path is env-driven, not profile-driven |
| Runtime-contract MCP payload builder | `crates/orchestrator-core/src/runtime_contract.rs` (`build_runtime_contract`) | emits `mcp.agent_id`, `mcp.endpoint`, `mcp.enforce_only`, `mcp.allowed_tool_prefixes`; no explicit caller-provided stdio transport input | cannot encode built-in AO stdio launch from agent profile |
| Agent profile schema | `crates/orchestrator-core/src/agent_runtime_config.rs` (`AgentProfile`) and `crates/orchestrator-core/config/agent-runtime-config.v2.json` | profile carries model/tool/runtime knobs | no `mcp_servers` and no profile-native `tool_policy` available for daemon spawn mapping |
| Runner native MCP enforcement | `crates/agent-runner/src/runner/process.rs` (`resolve_mcp_tool_enforcement`, `apply_native_mcp_policy`) | supports both endpoint and stdio transport + provider native lock-down; defaults allowed prefixes when enforcement enabled and list is empty | daemon does not yet provide profile-derived MCP transport/prefix policy |

## Scope
In scope:
- Add profile-readable MCP server configuration for daemon phase agents.
- Derive daemon runtime contract MCP fields from selected phase profile.
- Prefer built-in AO MCP server wiring via stdio command (`ao --project-root <root> mcp serve`).
- Propagate effective profile tool policy into `mcp.allowed_tool_prefixes`.
- Ensure `apply_native_mcp_policy` consumes profile-derived scope without policy broadening.
- Add focused regression tests in touched crates.

Out of scope:
- New AO MCP tools or behavioral changes in `ao mcp serve` handlers.
- Project-wide custom MCP server merge logic (`TASK-095` scope).
- Manual edits to `/.ao/*.json` state.
- Unrelated runner/provider launch refactors.

## Deterministic Mapping Contract

### MCP Server Selection
- Source profile: `agents.<phase_agent_id>`.
- Server key for this task: `mcp_servers.ao`.
- Supported server source for this task: built-in AO MCP server.
- Daemon runtime contract must set MCP transport to stdio launch of AO CLI using the active project root.

### Tool Policy Precedence
- Effective policy order:
  1. server-level policy (`mcp_servers.ao.tool_policy`) when present
  2. profile-level policy (`tool_policy`) otherwise
- Effective policy result must be translated to normalized lowercase tool prefixes.
- Those prefixes must be emitted into runtime contract `mcp.allowed_tool_prefixes`.

### Runtime Contract Expectations for Daemon Spawn
When built-in AO MCP is enabled for the phase profile, daemon-generated runtime contract must include:
- `mcp.agent_id = "ao"`
- `mcp.stdio.command = <resolved ao binary>`
- `mcp.stdio.args = ["--project-root", "<project_root>", "mcp", "serve"]`
- `mcp.enforce_only = true` when selected CLI supports MCP
- `mcp.allowed_tool_prefixes = <policy-derived prefixes>`

## Constraints
- Deterministic source of truth: daemon behavior must come from checked-in/runtime-loaded profile config, not ambient env vars.
- Fail closed: if MCP-only is requested by profile but transport/policy resolution is invalid, return actionable error and do not spawn unlocked.
- Backward compatibility: profiles without MCP config keep current behavior.
- Provider compatibility: codex/claude/gemini/opencode native adapters must continue functioning under enforcement.

## Functional Requirements
- `FR-01`: Daemon phase execution resolves effective MCP config from the selected phase agent profile.
- `FR-02`: Built-in AO MCP profile config maps to runtime-contract stdio transport for `ao mcp serve`.
- `FR-03`: Effective profile tool policy maps to explicit runtime-contract `mcp.allowed_tool_prefixes`.
- `FR-04`: `run_workflow_phase_with_agent` injects profile-derived MCP fields before `AgentRunRequest` submission.
- `FR-05`: Runner enforcement consumes provided profile-derived prefixes without silently broadening scope.
- `FR-06`: Existing fail-closed checks remain in place (missing transport under enforcement, unknown CLI adapter under enforcement).
- `FR-07`: Non-MCP profiles remain launch-compatible.
- `FR-08`: Targeted tests cover schema parsing, contract assembly, and enforcement behavior.

## Acceptance Criteria
- `AC-01`: Phase with AO MCP-enabled profile produces runtime contract containing populated `mcp.stdio.command`/`mcp.stdio.args`.
- `AC-02`: Runtime contract includes `mcp.agent_id="ao"` and profile-derived `mcp.allowed_tool_prefixes`.
- `AC-03`: For MCP-capable CLIs, runtime contract sets `mcp.enforce_only=true` in the profile-enabled path.
- `AC-04`: `apply_native_mcp_policy` applies provider-specific MCP lock-down using the profile-provided transport.
- `AC-05`: Out-of-scope tool calls are rejected by `is_tool_call_allowed` under enforced profile policy.
- `AC-06`: Missing/invalid MCP transport when enforcement is required returns deterministic, actionable failure.
- `AC-07`: Profiles without MCP server config continue working without new enforcement side effects.
- `AC-08`: Existing env-driven generic path remains unchanged for non-daemon callers unless explicit profile-derived MCP payload is provided.
- `AC-09`: Targeted tests for touched modules pass.

## Validation Plan (Implementation Phase)
- `cargo test -p orchestrator-core agent_runtime_config`
- `cargo test -p orchestrator-core runtime_contract`
- `cargo test -p orchestrator-cli daemon_scheduler`
- `cargo test -p agent-runner runner::process`

## Primary Change Surface (Implementation Input)
- `crates/orchestrator-core/src/agent_runtime_config.rs`
- `crates/orchestrator-core/config/agent-runtime-config.v2.json`
- `crates/orchestrator-core/src/runtime_contract.rs`
- `crates/orchestrator-cli/src/shared/runner.rs`
- `crates/orchestrator-cli/src/services/runtime/runtime_daemon/daemon_scheduler_phase_exec.rs`
- `crates/agent-runner/src/runner/process.rs`
