# TASK-025 Requirements: Enforce Sandbox and Tool Policy Controls

## Phase
- Workflow phase: `requirements`
- Workflow ID: `61012efe-24d8-408c-a2f0-59d063241ab4`
- Requirement: `REQ-025`
- Task: `TASK-025`

## Objective
Define a deterministic, repository-safe policy contract that adds:
- per-agent and per-task sandbox mode controls,
- explicit tool allow/deny enforcement,
- auditable elevated execution gates,
- actionable doctor diagnostics for policy health.

The contract must work for direct `ao agent run` execution and daemon-managed
workflow phase runs.

## Existing Baseline Audit

| Surface | Current implementation | Baseline behavior | Gap vs REQ-025 |
| --- | --- | --- | --- |
| Agent runtime config | `crates/orchestrator-core/src/agent_runtime_config.rs`, `crates/orchestrator-core/config/agent-runtime-config.v2.json` | Supports agent/phase tool-model runtime selection and command-phase `tools_allowlist` | No sandbox mode model and no task-specific execution policy fields |
| Runtime contract generation | `crates/orchestrator-core/src/runtime_contract.rs`, `crates/orchestrator-cli/src/shared/runner.rs` | Emits CLI launch shape and MCP-only `allowed_tool_prefixes` | No sandbox or deny policy payload, no task policy override channel |
| Runner tool enforcement | `crates/agent-runner/src/runner/process.rs` | Enforces MCP-only allow-prefix checks and locked server semantics | No denylist semantics, no per-task policy, no explicit elevated execution lifecycle |
| Workspace/env protection | `crates/agent-runner/src/sandbox/workspace_guard.rs`, `crates/agent-runner/src/sandbox/env_sanitizer.rs` | Enforces cwd inside project/worktree and fixed env allowlist | Not policy-driven per agent/task and no policy observability |
| Daemon phase metadata | `crates/orchestrator-cli/src/services/runtime/runtime_daemon/daemon_scheduler_phase_exec.rs`, `daemon_scheduler_project_tick.rs`, `daemon_run.rs` | Persists phase metadata (agent/tool/model hashes) and run artifacts | No policy snapshot hash/details and no elevation audit records in phase event stream |
| Doctor diagnostics | `crates/orchestrator-core/src/doctor.rs`, `crates/orchestrator-cli/src/services/operations/ops_planning/mod.rs` | Only checks `cwd_resolvable` and optional `PROJECT_ROOT` env | No policy configuration or enforcement health checks |

## Scope
In scope for implementation after this phase:
- Introduce a typed sandbox/tool policy model that can be resolved per run from
  agent defaults plus task overrides.
- Enforce resolved tool allow/deny policy in the runner for emitted tool calls.
- Enforce resolved sandbox mode at run launch boundaries.
- Add deterministic elevated execution requests for policy violations that may
  be explicitly approved.
- Persist elevation request/approval/outcome artifacts for auditability.
- Extend doctor output with policy-specific checks and failure details.
- Add tests for policy resolution, enforcement, elevation gating, and doctor
  diagnostics.

Out of scope for this task:
- OS kernel sandboxing or container/jail orchestration.
- Interactive TTY approval prompts.
- Web UI policy editing flows.
- Manual edits to `.ao` JSON state files.

## Constraints
- Preserve existing command names and existing safe defaults for users without
  explicit policy configuration.
- Keep `ao.cli.v1` envelope behavior unchanged in JSON mode.
- Keep `workspace_guard` repository-bound checks intact and fail closed.
- Keep policy evaluation deterministic (same input context -> same decision).
- Keep enforcement semantics explicit and auditable in run/phase artifacts.
- Deny rules must take precedence over allow rules.
- Elevated approvals must be operation-bound and single-use to prevent replay.

## Policy Model Contract

### Sandbox Modes
Resolved sandbox mode enum:
- `read_only`
- `workspace_write`
- `danger_full_access`

Default behavior when not configured:
- `workspace_write`

### Tool Policy
Resolved tool policy must support:
- `allow_prefixes`
- `allow_exact`
- `deny_prefixes`
- `deny_exact`

Decision rules:
1. Normalize tool identifiers to lowercase before evaluation.
2. `deny_exact`/`deny_prefixes` always block even when also allowlisted.
3. If allow rules are empty, use existing MCP defaults for MCP-only mode and
   preserve current behavior for non-MCP mode.
4. Unknown/empty tool names fail closed.

### Policy Resolution Precedence
Resolved policy for a run must follow:
1. task-level override (if present)
2. phase-level runtime override
3. agent profile default
4. global default

Resolution output must include:
- resolved sandbox mode
- resolved allow/deny tool sets
- policy source trace (which level supplied each field)
- stable policy hash for audit correlation

## Elevated Execution Contract
When policy would block an operation that is explicitly elevatable:
- runner must emit/return `ELEVATION_REQUIRED` with:
  - `elevation_request_id`
  - blocked action metadata (tool, sandbox target, phase/run identity)
  - remediation guidance
- no side effects are executed before approval

Elevation records must persist:
- request record (`requested_at`, requester/run context, requested action,
  policy hash, reason)
- approval record (`approved`, approver identity, comment, approved_at)
- outcome record (`success`, `message`, `recorded_at`)

Approval safety rules:
- approval must bind to exact run scope (workflow/task/phase/run id + policy
  hash + requested action)
- approval is single-use
- approval mismatch fails closed with deterministic error

## Observability and Audit Requirements
Phase/run artifacts must include policy context:
- resolved policy hash
- resolved sandbox mode
- policy decision events (`allowed`, `blocked`, `elevation_required`,
  `elevation_approved`, `elevation_denied`)

Daemon phase events must expose policy metadata without leaking secrets.

## Doctor Check Contract
`ao doctor` must include policy checks:
- `policy_config_loadable`
- `policy_schema_valid`
- `policy_phase_bindings_valid`
- `policy_elevation_store_writable`
- `policy_runtime_defaults_resolvable`

Result grading:
- `unhealthy` if any policy check fails hard (invalid schema, unresolved policy
  references, non-writable required store path)
- `degraded` for soft warnings
- `healthy` when all checks pass

## Acceptance Criteria
- `AC-01`: Policy model supports sandbox mode + tool allow/deny fields at
  agent and task levels.
- `AC-02`: Policy resolution precedence is deterministic and serialized in
  run/phase metadata.
- `AC-03`: Runner blocks disallowed tool calls with deterministic
  `POLICY_VIOLATION` errors.
- `AC-04`: Deny rules always override allow rules.
- `AC-05`: Resolved sandbox mode is enforced before launching side-effecting
  execution.
- `AC-06`: Elevatable policy violations return `ELEVATION_REQUIRED` and create
  auditable request records.
- `AC-07`: Approved elevation can be consumed once and only for its bound
  action scope.
- `AC-08`: Elevation request/approval/outcome artifacts are persisted and
  queryable for audit.
- `AC-09`: `ao doctor` reports policy checks and grading (`healthy`,
  `degraded`, `unhealthy`) deterministically.
- `AC-10`: Existing workflows with no explicit policy config remain functional
  with current safe defaults.
- `AC-11`: Existing command-phase `tools_allowlist` behavior remains intact for
  command mode phases.
- `AC-12`: Workspace boundary enforcement remains unchanged and continues to
  fail closed.

## Verification Matrix

| Requirement | Verification method |
| --- | --- |
| `AC-01`, `AC-02` | Unit tests for config parsing + policy resolution precedence and hash stability |
| `AC-03`, `AC-04` | Runner tests for allow/deny combinations and deny-over-allow behavior |
| `AC-05` | Runner launch tests validating sandbox-mode gating decisions |
| `AC-06`, `AC-07`, `AC-08` | Integration tests for elevation request, approval binding, single-use consumption, and persisted outcomes |
| `AC-09` | Doctor tests asserting policy check entries and result grading logic |
| `AC-10`, `AC-11`, `AC-12` | Regression tests for existing daemon/task execution and workspace guard behavior |

## Deterministic Deliverables for Next Phase
- Typed policy structs + validation in core config/runtime layers.
- Runner enforcement for sandbox mode and tool allow/deny semantics.
- Elevated execution request/approval/outcome persistence and enforcement path.
- Doctor checks covering policy config and audit store health.
- End-to-end tests for policy resolution, enforcement, elevation, and regressions.
