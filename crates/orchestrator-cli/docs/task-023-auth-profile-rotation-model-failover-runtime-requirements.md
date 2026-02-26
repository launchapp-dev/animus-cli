# TASK-023 Requirements: Auth Profile Rotation and Model Failover Runtime

## Phase
- Workflow phase: `requirements`
- Workflow ID: `c432cfa5-2b20-493f-be00-d2d115103d6f`
- Task: `TASK-023`
- Requirement reference: `REQ-023`

## Objective
Define a deterministic runtime contract for provider auth profile rotation, retry/backoff, model fallback chaining, and operator diagnostics commands used by daemon-managed workflow phase execution.

## Existing Baseline Audit

| Capability area | Current implementation | Current behavior | Gap to close |
| --- | --- | --- | --- |
| Model fallback target planning | `crates/orchestrator-cli/src/services/runtime/runtime_daemon/daemon_scheduler_phase_targets.rs` | Builds ordered `(tool, model)` targets from override + configured fallback + env fallback + defaults, with dedupe | No auth profile dimension per target |
| Retry/backoff | `crates/orchestrator-cli/src/services/runtime/runtime_daemon/daemon_scheduler_phase_exec.rs` | Retries transient runner failures with exponential backoff (200ms -> max 3s) within a target | No typed policy for auth-specific retries or profile rotation attempts |
| Failover trigger classification | `crates/orchestrator-cli/src/services/runtime/runtime_daemon/daemon_scheduler_failover.rs` | Fails over model targets on provider exhaustion or target unavailability message patterns | Cannot differentiate profile rotation vs model failover decision path |
| Runtime config shape | `crates/orchestrator-core/src/agent_runtime_config.rs`, `crates/orchestrator-core/config/agent-runtime-config.v2.json` | Supports tool/model/fallback/reasoning/web_search/timeout/max_attempts overrides | No schema for named provider auth profiles or auth profile chains |
| Auth/environment handling | `crates/agent-runner/src/sandbox/env_sanitizer.rs`, `crates/agent-runner/src/runner/supervisor.rs`, `crates/agent-runner/src/providers/mod.rs` | Uses sanitized process env, checks required API key presence for tool | No profile aliases, no runtime key rotation across profile candidates |
| Operator diagnostics commands | `ao daemon events` (`daemon_events.rs`), `ao model status` (`ops_model/status.rs`) | Raw daemon event streaming and static model availability check | No dedicated runtime diagnostics command for resolved failover/auth plan; no failover-focused event filtering |

## Scope
In scope for implementation phase:
- Add provider auth profile definitions to workflow agent runtime config and validation.
- Add deterministic auth profile chain resolution for each phase execution target.
- Add auth profile rotation logic integrated with existing retry/backoff and model failover behavior.
- Preserve and extend model fallback chain behavior with explicit auth/profile-aware decision boundaries.
- Add diagnostics command surface for runtime resolution and failover/auth events.
- Add structured daemon/runtime events that explain retry, auth rotation, and model failover decisions.
- Add tests covering config validation, runtime behavior, and diagnostics output.
- Add deterministic terminal diagnostics summarizing all attempted `(target, auth_profile, attempt)` outcomes.

Out of scope for this task:
- External secret manager integration.
- Persisting raw API secrets in AO config files.
- Replacing the existing model routing defaults in `protocol` beyond failover/runtime needs.
- UI/web diagnostics panels.
- Changes to `ao.cli.v1` envelope schema or exit-code mapping.
- Adding randomized retry jitter to runtime behavior.

### Requirements-Phase Deliverables (This Phase)
- `task-023-auth-profile-rotation-model-failover-runtime-requirements.md` reflects final implementation scope, constraints, and acceptance criteria.
- `task-023-auth-profile-rotation-model-failover-runtime-implementation-notes.md` reflects crate/module-level ownership and execution sequence.
- No runtime behavior or schema code changes are performed in this requirements phase.

## Implementation Boundaries by Crate
- `crates/orchestrator-core`:
  - runtime config schema additions (`auth_profiles`, auth chain references, validation, compatibility).
- `crates/orchestrator-cli`:
  - daemon phase scheduler resolution/execution changes (profile rotation + model failover).
  - diagnostics command and daemon event filtering.
  - structured event emission for retry/rotation/failover transitions.
- `crates/agent-runner`:
  - selected auth profile env mapping into process launch environment.
  - sanitized env handling compatibility with alias mapping.
- Non-goal boundary:
  - no state-shape migration outside agent runtime config and related persistence/round-trip paths.

## Constraints
- Keep output deterministic:
  - stable ordering of model targets and auth profile candidates
  - stable field ordering in JSON payloads where practical
  - no nondeterministic retry jitter in tests
- Keep repository-safe behavior:
  - no direct manual edits of `.ao/*.json` at runtime; use existing command persistence paths
  - no mutation outside active run/workflow state paths
- Maintain backward compatibility:
  - existing configs without auth profile fields remain valid
  - fallback/retry behavior remains equivalent when auth profile rotation is not configured
- Secret safety:
  - never emit secret values in logs/events/diagnostics
  - diagnostics may only expose profile IDs, provider/tool IDs, and missing-env-key names

## Runtime Contract

### Auth Profile Data Model
Required additions to agent runtime config:
- Introduce optional named auth profiles keyed by profile ID.
- Each auth profile defines:
  - `provider` (normalized tool/provider id such as `codex`, `claude`, `gemini`, `opencode`)
  - `env_map` (map from required runtime env name to source env var name)
  - optional `priority` (lower number = earlier candidate)
  - optional `enabled` flag (default `true`)
- Agent profile and phase runtime overrides may reference `auth_profile_chain` (ordered profile IDs).

Validation rules:
- Empty IDs/keys are rejected.
- Unknown referenced profile IDs are rejected.
- Disabled profiles are excluded from runtime candidate chains.
- Profiles must not redefine duplicate source bindings within the same profile.

### Target + Auth Resolution Order
For each phase execution:
1. Resolve ordered model targets using existing precedence (override -> configured fallback -> env fallback -> defaults).
2. For each target, resolve ordered auth profile candidates:
   - phase runtime `auth_profile_chain`
   - agent profile default auth chain
   - provider-level matching profiles by `priority`, then profile ID
   - legacy implicit env behavior if no profiles match
3. Execute attempts in deterministic nested order:
   - target index first
   - auth profile index second
   - attempt count third

### Retry/Backoff and Rotation Policy
- Transient runner/process errors retry on the same `(target, auth_profile)` tuple.
- Auth/quota/rate-limit/credits exhaustion errors rotate to next auth profile for the same target before model failover.
- Target/tool/model availability errors fail over directly to the next model target.
- Backoff policy keeps current exponential shape unless explicitly overridden by phase runtime settings.
- Max attempts remain bounded by runtime config (`max_attempts`) and must not exceed current global clamp policy.

### Failure Classification Matrix
| Failure class | Condition examples | Action when more auth profiles remain | Action when auth profiles exhausted | Action when targets exhausted |
| --- | --- | --- | --- | --- |
| Transient runner/process | runner disconnect, socket timeout, broken pipe | Retry same tuple | Retry same tuple until attempt cap | Terminal |
| Provider auth/quota/rate-limit | `insufficient_quota`, credits exhausted, rate-limit | Rotate auth profile | Fail over model target | Terminal |
| Target unavailable/tool-model invalid | missing CLI, unsupported tool, unknown model | Fail over model target | Fail over model target | Terminal |
| Unclassified hard failure | deterministic parser/contract errors | Terminal | Terminal | Terminal |

### Diagnostics Command Contract

Command 1 (new):
- `ao workflow agent-runtime diagnostics --phase <PHASE_ID> [--pipeline <PIPELINE_ID>] [--complexity <low|medium|high>]`
- Returns:
  - resolved ordered execution targets (`tool`, `model`, `source`)
  - resolved auth profile chains per target (`profile_id`, `provider`, `readiness`)
  - retry/backoff policy (`max_attempts`, `backoff_base_ms`, `backoff_cap_ms`)
  - validation warnings/errors (for example missing referenced env vars)

Command 2 (extended filtering on existing events command):
- `ao daemon events` gains optional filters for failover diagnostics:
  - `--event-type`
  - `--workflow-id`
  - `--task-id`
  - `--phase`
- Supports targeted retrieval of failover and auth-rotation event history without post-hoc manual filtering.

Required event types for diagnostics:
- `workflow-phase-retry-scheduled`
- `workflow-phase-auth-rotated`
- `workflow-phase-model-failover`
- `workflow-phase-fallback-exhausted`

## Acceptance Criteria
- `AC-01`: Agent runtime config accepts optional auth profile definitions and auth profile chains with strict validation.
- `AC-02`: Legacy runtime config without auth profile fields remains valid and behaviorally unchanged.
- `AC-03`: Phase execution resolves deterministic target and auth profile order.
- `AC-04`: Auth/quota/rate-limit/credits exhaustion rotates auth profiles before model failover when additional profiles exist.
- `AC-05`: Transient runner/process failures retry with bounded exponential backoff on the same target/profile tuple.
- `AC-06`: Missing CLI/unsupported model/tool-unavailable failures trigger model failover, not profile rotation.
- `AC-07`: Exhausting all profiles for a target advances to the next fallback model target.
- `AC-08`: Exhausting all targets returns a deterministic terminal error summary containing target/profile attempt outcomes.
- `AC-09`: Diagnostics command returns resolved target/auth plan and readiness diagnostics in JSON and non-JSON modes.
- `AC-10`: `ao daemon events` failover filters return only matching auth/failover events.
- `AC-11`: Logs/events/diagnostics never expose secret values.
- `AC-12`: Tests cover config validation, retry/backoff behavior, auth rotation behavior, model failover behavior, and diagnostics command output.
- `AC-13`: Failure classification behavior follows the matrix above and remains deterministic for equivalent inputs.
- `AC-14`: Terminal exhaustion summaries include ordered attempt outcomes without secret values.
- `AC-15`: Requirements-phase artifacts are complete before implementation starts.

## Verification Matrix

| Requirement | Verification method |
| --- | --- |
| `AC-01`, `AC-02` | Unit tests in `orchestrator-core` for config schema parsing/validation and backward compatibility |
| `AC-03` | Deterministic ordering tests in daemon scheduler target/profile resolution |
| `AC-04` to `AC-08` | Runtime daemon unit/integration tests with injected failure classes and expected transition paths |
| `AC-09`, `AC-10` | CLI tests for diagnostics command payloads and daemon event filtering behavior |
| `AC-11` | Redaction tests asserting no secret values in emitted diagnostic payloads |
| `AC-12` | Targeted test runs across `orchestrator-core`, `orchestrator-cli`, and `agent-runner` suites for touched modules |
| `AC-13`, `AC-14` | Scheduler classification/unit tests + deterministic output snapshot tests |
| `AC-15` | Documentation review for requirements + implementation notes consistency |

## Deterministic Deliverables for Implementation Phase
- Runtime config schema updates for auth profiles and chain references.
- Daemon scheduler execution updates for profile rotation + model failover orchestration.
- Agent runner environment injection support for profile-selected credentials (name-based mappings only).
- Diagnostics command implementation and failover event filtering support.
- Tests and regression coverage for rotation/failover behavior and secret-safe diagnostics.
