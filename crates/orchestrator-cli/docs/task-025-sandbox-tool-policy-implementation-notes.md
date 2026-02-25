# TASK-025 Implementation Notes: Sandbox and Tool Policy Enforcement

## Purpose
Translate `TASK-025` requirements into implementation slices that add policy
controls without broad behavioral drift outside runner/daemon execution safety.

## Non-Negotiable Constraints
- Keep all implementation in Rust crates under `crates/`.
- Preserve `ao.cli.v1` output envelope and current exit-code semantics.
- Keep `.ao` state mutations command-driven; do not rely on manual JSON edits.
- Preserve workspace-bound execution checks in runner sandbox guard.
- Preserve current behavior for projects with no explicit policy overrides.

## Proposed Change Surface

### Policy model and configuration
- `crates/orchestrator-core/src/agent_runtime_config.rs`
  - add typed policy fields for sandbox mode and tool allow/deny lists on
    agent/phase runtime definitions.
  - add validation for policy field shape and normalization.
- `crates/orchestrator-core/config/agent-runtime-config.v2.json`
  - add default policy block values (safe baseline).

### Task-level override channel
- `crates/orchestrator-core/src/types.rs`
  - add optional task execution policy override shape.
- `crates/orchestrator-core/src/services/task_impl.rs`
  - persist task policy override updates.
- `crates/orchestrator-cli/src/services/runtime/runtime_project_task/task.rs`
  - pass task policy override through `--input-json` update path and surface in
    read/get output.

### Runtime contract and context propagation
- `crates/orchestrator-core/src/runtime_contract.rs`
  - include resolved policy payload fields in runtime contract.
- `crates/orchestrator-cli/src/shared/runner.rs`
  - include resolved policy for direct `agent run`.
- `crates/orchestrator-cli/src/services/runtime/runtime_daemon/daemon_scheduler_phase_exec.rs`
  - resolve effective policy per phase/task and inject into run context.

### Runner enforcement and elevation
- `crates/agent-runner/src/runner/process.rs`
  - enforce sandbox mode at launch boundary.
  - enforce tool allow/deny policy for emitted tool calls.
  - emit deterministic policy/elevation errors and events.
- `crates/agent-runner/src/runner/supervisor.rs`
  - validate policy presence/shape in context before process spawn.

### Audit persistence and eventing
- `crates/orchestrator-cli/src/services/runtime/runtime_daemon/daemon_scheduler_project_tick.rs`
  - include policy decision/elevation signals in phase execution events.
- `crates/orchestrator-cli/src/services/runtime/runtime_daemon/daemon_run.rs`
  - expose policy audit payload in daemon event stream.
- `crates/orchestrator-core/src/domain_state.rs` or dedicated store module
  - persist elevated execution request/approval/outcome records.

### Doctor diagnostics
- `crates/orchestrator-core/src/doctor.rs`
  - add policy health checks and result grading logic.
- `crates/orchestrator-cli/src/services/operations/ops_planning/mod.rs`
  - include expanded doctor payload unchanged behind existing command surface.

### CLI command surface for elevation audit
- `crates/orchestrator-cli/src/cli_types.rs`
  - add explicit elevation request/approve/outcome/read commands (or extend an
    existing operations group) for auditable operator workflows.
- `crates/orchestrator-cli/src/services/operations/`
  - implement handlers and persistent store wiring for elevation actions.

## Execution Sequence
1. Introduce policy structs/enums and config validation.
2. Add task-level policy override model and persistence.
3. Implement policy resolution utility (task -> phase -> agent -> default).
4. Thread resolved policy into runtime contract/context for daemon + direct runs.
5. Enforce policy in agent-runner with deterministic deny/elevation behavior.
6. Add elevation persistence records and event payload emission.
7. Extend doctor checks for policy health.
8. Add tests and run targeted validation.

## Enforcement Guidance
- Normalize identifiers before matching.
- Evaluate deny rules before allow rules.
- Fail closed on parse/shape errors.
- Keep policy violation errors machine-parsable:
  - `POLICY_VIOLATION`
  - `ELEVATION_REQUIRED`

## Elevation Guidance
- Elevation approvals must bind to:
  - run identity
  - workflow/task/phase identity (when present)
  - requested action (tool/sandbox change)
  - resolved policy hash
- Approved elevation must be consumed once and produce an outcome record.

## Suggested Data Contracts

### Resolved policy payload (runtime context)
- `sandbox_mode`
- `tool_policy.allow_prefixes`
- `tool_policy.allow_exact`
- `tool_policy.deny_prefixes`
- `tool_policy.deny_exact`
- `policy_hash`
- `policy_sources`

### Elevation record
- `id`
- `run_id`
- `workflow_id`
- `task_id`
- `phase_id`
- `agent_id`
- `policy_hash`
- `requested_action`
- `requested_sandbox_mode`
- `reason`
- `requested_at`
- `approved`
- `approved_by`
- `approved_at`
- `outcome`

## Testing Plan

### Core/config tests
- agent runtime config accepts valid policy fields.
- invalid sandbox values fail validation.
- policy hash and precedence resolution remain deterministic.

### Runner tests
- deny-over-allow semantics are enforced.
- unknown tool names fail closed.
- sandbox mode violations block launch.
- elevation required path emits deterministic error payload.
- approved elevation allows one matching operation only.

### CLI/daemon integration tests
- daemon phase events include policy metadata/signals.
- phase execution metadata artifacts include resolved policy hash.
- elevation records persist request/approval/outcome fields.
- doctor output includes new policy checks and correct health grading.

### Regression tests
- existing MCP-only allow-prefix behavior still works when no denylist is set.
- command mode `tools_allowlist` behavior remains unchanged.
- workspace guard behavior remains unchanged.

## Risks and Mitigations
- Risk: policy schema churn breaks existing config files.
  - Mitigation: keep new fields optional with stable defaults and clear errors.
- Risk: inconsistent enforcement between direct run and daemon phases.
  - Mitigation: share one policy resolution utility and one runner enforcement
    path.
- Risk: elevation token replay/mismatch.
  - Mitigation: operation-bound, hash-bound, single-use approvals with strict
    verification.
- Risk: doctor noise from optional paths.
  - Mitigation: classify optional-path issues as degraded warnings, not hard
    failures.
