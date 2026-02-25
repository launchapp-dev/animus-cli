# TASK-023 Implementation Notes: Auth Profile Rotation and Model Failover Runtime

## Purpose
Translate `TASK-023` requirements into concrete implementation slices for standalone daemon runtime execution while preserving deterministic behavior and secret-safe diagnostics.

## Non-Negotiable Constraints
- Preserve Rust-only implementation across existing crates.
- Keep `.ao` state changes inside AO command persistence paths.
- Keep `ao.cli.v1` response envelope semantics unchanged.
- Do not log raw secret values.
- Preserve existing behavior when no auth profile configuration is provided.

## Proposed Change Surface

### 1) Runtime config schema and validation
- `crates/orchestrator-core/src/agent_runtime_config.rs`
  - add auth profile schema types
  - add optional auth chain references on `AgentProfile` and `AgentRuntimeOverrides`
  - extend validation and accessors for resolved auth profile chain
- `crates/orchestrator-core/config/agent-runtime-config.v2.json`
  - add empty/default auth profile sections so generated defaults remain explicit
- `crates/orchestrator-cli/src/services/operations/ops_workflow.rs`
  - ensure `workflow agent-runtime set/get/validate` round-trips new fields

### 2) Phase execution and failover orchestration
- `crates/orchestrator-cli/src/services/runtime/runtime_daemon/daemon_scheduler_phase_exec.rs`
  - add nested execution loop: target -> auth profile -> attempt
  - wire auth/profile-aware retry and failover outcomes
- `crates/orchestrator-cli/src/services/runtime/runtime_daemon/daemon_scheduler_failover.rs`
  - split failure classification into:
    - retry-on-same-profile
    - rotate-auth-profile
    - failover-model-target
    - terminal-failure
- `crates/orchestrator-cli/src/services/runtime/runtime_daemon/daemon_scheduler_phase_targets.rs`
  - keep target planning deterministic; expose source metadata for diagnostics payloads
- `crates/orchestrator-cli/src/services/runtime/runtime_daemon/daemon_scheduler_runtime_support.rs`
  - include tunable policy fields if needed (`backoff_base_ms`, `backoff_cap_ms`, optional per-profile attempts)

### 3) Agent runner env injection for selected auth profile
- `crates/agent-runner/src/runner/supervisor.rs`
  - read selected profile/env mapping from runtime contract context
  - map source env var names to provider-required env keys before process spawn
- `crates/agent-runner/src/sandbox/env_sanitizer.rs`
  - allow required provider env keys and profile source env aliases without broadening unrelated environment exposure
- `crates/agent-runner/src/providers/mod.rs`
  - keep availability checks compatible with profile-based env aliasing

### 4) Diagnostics command surface
- `crates/orchestrator-cli/src/cli_types.rs`
  - add `workflow agent-runtime diagnostics` subcommand args
  - extend `daemon events` args with optional failover filters
- `crates/orchestrator-cli/src/services/operations/ops_workflow.rs`
  - implement diagnostics payload for resolved target/auth plan and readiness checks
- `crates/orchestrator-cli/src/services/runtime/runtime_daemon/daemon_events.rs`
  - apply optional filtering by event type/workflow/task/phase before print

### 5) Event emission and payloads
- `crates/orchestrator-cli/src/services/runtime/runtime_daemon/daemon_scheduler_project_tick.rs`
  - emit structured events:
    - `workflow-phase-retry-scheduled`
    - `workflow-phase-auth-rotated`
    - `workflow-phase-model-failover`
    - `workflow-phase-fallback-exhausted`
  - include `workflow_id`, `task_id`, `phase_id`, target/profile identifiers, attempt index, and failure class
  - exclude secret payloads

## Runtime Algorithm (Deterministic)
1. Resolve ordered model targets with existing planner rules.
2. Resolve ordered auth profiles for each target.
3. For each target:
   - for each auth profile:
     - run attempt loop with bounded backoff
     - classify errors and choose one:
       - retry same tuple
       - rotate auth profile
       - failover model target
       - terminal error
4. Emit failover/rotation diagnostics events at each transition.
5. Return deterministic terminal error with per-target/profile summary when exhausted.

## Diagnostics Payload Guidance

### `workflow agent-runtime diagnostics` response keys
- `phase_id`
- `execution_targets` (ordered)
- `auth_profile_chains` (per target)
- `retry_policy`
- `readiness`
- `warnings`

### Daemon failover event payload keys
- `workflow_id`
- `task_id`
- `phase_id`
- `target` (`tool`, `model`, `index`)
- `auth_profile_id` (if applicable)
- `attempt`
- `classification`
- `reason`

## Testing Plan
- `crates/orchestrator-core/src/agent_runtime_config.rs` tests:
  - new schema validation
  - backward compatibility for legacy/no-auth configs
- `crates/orchestrator-cli/src/services/runtime/runtime_daemon/daemon_scheduler.rs` tests:
  - retry/backoff path
  - profile rotation path
  - model failover path
  - exhaustion summary determinism
- `crates/orchestrator-cli` CLI tests:
  - diagnostics command output
  - daemon event filtering behavior
- `crates/agent-runner` tests:
  - env alias mapping
  - no secret leakage in error output

## Implementation Sequence
1. Extend runtime config schema + validation.
2. Implement auth chain resolution helpers.
3. Update daemon phase execution loop with profile-aware classification.
4. Add runner env alias injection support.
5. Add diagnostics command and daemon event filter support.
6. Add tests and regression coverage.

## Risks and Mitigations
- Risk: auth-profile logic changes existing fallback behavior.
  - Mitigation: explicit legacy path with no profile config and regression tests.
- Risk: secret leakage through diagnostics.
  - Mitigation: central redaction helper and snapshot tests.
- Risk: event volume increases with retries.
  - Mitigation: bounded event payload size and selective event emission per transition.
