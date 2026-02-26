# TASK-022 Requirements: Guided Onboarding and Configuration Wizard

## Phase
- Workflow phase: `requirements`
- Workflow ID: `2525eccd-7658-4192-a0a0-e843a04c30e1`
- Task: `TASK-022`
- Project root: `/Users/samishukri/ao-cli`

## Objective
Deliver a production-ready AO onboarding flow that provides:
- guided setup for first-run operators (`ao setup` on interactive terminals),
- deterministic non-interactive setup for automation (`ao setup --non-interactive ...`),
- doctor-driven diagnostics and explicit safe remediation (`ao doctor`, `ao doctor --fix`),
- API-only writes for onboarding/daemon config surfaces.

## Current Repository Snapshot

| Surface | Current path(s) | Current behavior | Status for TASK-022 |
| --- | --- | --- | --- |
| Setup command surface | `crates/orchestrator-cli/src/cli_types.rs`, `crates/orchestrator-cli/src/main.rs`, `crates/orchestrator-cli/src/services/operations/ops_setup.rs` | `ao setup` supports guided/non-interactive, `--plan`, `--doctor-fix` | implemented |
| Doctor command surface | `crates/orchestrator-cli/src/cli_types.rs`, `crates/orchestrator-cli/src/services/operations/ops_doctor.rs`, `crates/orchestrator-core/src/doctor.rs` | `ao doctor` diagnostics + `--fix` remediation action reporting | implemented |
| Daemon config API boundary | `crates/orchestrator-core/src/daemon_config.rs`, `crates/orchestrator-cli/src/services/runtime/runtime_daemon.rs`, `crates/orchestrator-cli/src/services/runtime/runtime_daemon/daemon_scheduler_git_ops.rs` | typed load/update/write helpers used by setup/daemon/scheduler paths | implemented |
| Atomic config persistence | `crates/orchestrator-core/src/daemon_config.rs` | temp-file write + atomic rename for `.ao/pm-config.json` | implemented |
| Acceptance-oriented tests | `crates/orchestrator-cli/tests/setup_doctor_e2e.rs`, module tests in `ops_setup.rs`, `ops_doctor.rs`, `doctor.rs`, `daemon_config.rs` | guardrails, plan/apply contract, idempotence, doctor payload/action checks | implemented |

## Scope Locked for This Task
In scope:
- Maintain `ao setup` as the onboarding command (no command rename in this task).
- Preserve and harden two onboarding modes:
  - guided (`ao setup` on TTY),
  - non-interactive (`--non-interactive` + explicit `--auto-*` flags).
- Preserve and harden setup plan/apply contract:
  - read-only plan stage,
  - explicit apply stage with deterministic changed/unchanged domain metadata.
- Preserve and harden doctor diagnostics/remediation contract:
  - stable check IDs/status/details/remediation metadata,
  - explicit `--fix` execution mode with deterministic action results.
- Keep onboarding/daemon config writes routed through `orchestrator-core` helpers.

Out of scope:
- Web UI onboarding or desktop wrapper onboarding.
- Auto-installing third-party CLIs/package managers.
- Manual edits to `.ao/*.json` project state/config files.
- Command-surface redesign beyond `setup` and `doctor`.

## Constraints
- Preserve `ao.cli.v1` envelope behavior for `--json`.
- Preserve existing exit-code mapping (`2/3/4/5/1`); validation failures stay `invalid_input` (`2`).
- Keep repository safety: project-root scoped writes only, no hidden side effects.
- Keep remediation deterministic and explicit (`--fix` is opt-in).
- Keep config writes atomic.
- Do not expose secrets in setup/doctor output.

## Functional Contract

### FR-01: Mode Selection and Prompting
- Guided mode is selected when `--non-interactive` is absent.
- Guided mode must fail fast when stdin/stdout are not interactive terminals.
- Non-interactive mode must not prompt and must require explicit `--auto-merge`, `--auto-pr`, and `--auto-commit-before-merge`.

### FR-02: Setup Plan/Apply Payload Contract
- Plan stage (`--plan`) returns:
  - `stage = "plan"`,
  - `mode`,
  - environment summary,
  - required daemon-config changes,
  - blocked items from doctor findings.
- Apply stage returns:
  - `stage = "apply"`,
  - deterministic changed/unchanged domain metadata,
  - doctor before/after summaries when applicable.

### FR-03: Doctor Diagnostics and Remediation
- `ao doctor` must return stable checks with:
  - `id`,
  - `status` (`ok|warn|fail`),
  - `details`,
  - `remediation` (`id`, `available`, `details`, optional `command`).
- `ao doctor --fix` must report remediation actions deterministically with:
  - `id`,
  - `status` (`applied|skipped|failed`),
  - `details`.

### FR-04: API-Only Config Writes
- Setup/daemon config mutations must use `orchestrator-core` daemon config APIs.
- CLI setup/doctor/daemon handlers must not directly write `.ao/pm-config.json`.

### FR-05: Idempotence and Persistence Safety
- Re-running setup apply with identical effective inputs must become no-op (`daemon_config_updated = false`).
- Failed write paths must not leave partially-written daemon config files.

### FR-06: Backward Compatibility
- Behavior outside setup/doctor/daemon-config alignment remains unchanged.
- Existing daemon automation semantics remain compatible with existing config values.

## Acceptance Criteria
- `AC-01`: `ao setup` supports guided and non-interactive modes.
- `AC-02`: `ao setup --plan` in non-interactive terminal contexts fails guided mode with invalid input semantics.
- `AC-03`: `ao setup --non-interactive --plan` without explicit `--auto-*` values fails deterministically with invalid input semantics.
- `AC-04`: non-interactive setup plan output includes stable `stage`, `mode`, and `apply.applied=false` metadata.
- `AC-05`: setup apply persists daemon config and idempotent re-run reports `daemon_config_updated=false`.
- `AC-06`: doctor diagnostics include stable check/remediation fields.
- `AC-07`: `ao doctor --fix` reports non-empty remediation action results with stable action fields.
- `AC-08`: setup/doctor/daemon config write paths use `orchestrator-core` config helpers rather than ad-hoc file writes.
- `AC-09`: daemon config persistence remains atomic.
- `AC-10`: JSON output remains envelope-compatible with existing `ao.cli.v1` conventions.

## Verification Matrix
| Acceptance area | Verification method |
| --- | --- |
| `AC-01` to `AC-07` | `crates/orchestrator-cli/tests/setup_doctor_e2e.rs` and setup/doctor module tests |
| `AC-08` | code-path audit in setup/doctor/runtime daemon handlers + daemon config API usage |
| `AC-09` | `crates/orchestrator-core/src/daemon_config.rs` unit tests and atomic write implementation |
| `AC-10` | CLI JSON envelope assertions in test harness usage |

## Deterministic Deliverables for Next Phase
- Keep all onboarding and remediation changes scoped to:
  - `crates/orchestrator-cli/src/services/operations/ops_setup.rs`
  - `crates/orchestrator-cli/src/services/operations/ops_doctor.rs`
  - `crates/orchestrator-core/src/doctor.rs`
  - `crates/orchestrator-core/src/daemon_config.rs`
  - `crates/orchestrator-cli/tests/setup_doctor_e2e.rs`
- Preserve repository-safe behavior and avoid broad command-surface churn.
