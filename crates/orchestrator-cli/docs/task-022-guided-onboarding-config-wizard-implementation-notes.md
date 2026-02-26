# TASK-022 Implementation Notes: Guided Onboarding and Config Wizard

## Purpose
Translate TASK-022 requirements into an implementation-phase checklist that is
aligned with the current repository state and keeps changes repository-safe.

## Current Status (Already Landed)
- `ao setup` command and args are wired (`SetupArgs`, `main.rs`, `ops_setup.rs`).
- `ao doctor --fix` is wired with deterministic action reporting (`DoctorArgs`,
  `ops_doctor.rs`).
- Doctor checks/remediation metadata exist in `orchestrator-core` (`doctor.rs`).
- Daemon config read/write/update uses typed APIs with atomic writes
  (`daemon_config.rs`), and runtime daemon/scheduler paths consume those APIs.
- End-to-end coverage exists in `crates/orchestrator-cli/tests/setup_doctor_e2e.rs`.

## Non-Negotiable Constraints
- Keep changes Rust-only under `crates/`.
- Keep `.ao` mutations behind AO APIs (no ad-hoc manual JSON edits).
- Preserve `ao.cli.v1` envelope + exit-code behavior.
- Keep command behavior outside setup/doctor/daemon-config alignment unchanged.

## Implementation Delta for Next Phase

### 1. Contract hardening (no command-surface churn)
- Keep `setup` and `doctor` command names/flags stable.
- Ensure setup/doctor payload keys remain deterministic and backwards-safe for
  automation callers.
- If output shape changes are required, gate them behind explicit versioned
  fields instead of breaking existing keys.

### 2. Diagnostics/remediation quality
- Keep doctor checks stable by check ID and remediation metadata.
- Extend checks/remediation only for safe local fixes; do not add external
  installer behavior.
- Ensure setup blocked-item derivation remains tied to actionable doctor checks.

### 3. Config write boundary enforcement
- Maintain `orchestrator-core` as the only writer for daemon config paths.
- Avoid introducing direct filesystem writes to `.ao/pm-config.json` in CLI
  handlers.
- Preserve atomic write semantics in all new config mutation branches.

### 4. Test coverage maintenance
- Keep `setup_doctor_e2e` as the primary acceptance harness for:
  - guided/non-interactive guardrails,
  - plan/apply payload contract,
  - idempotent apply behavior,
  - doctor diagnostics and fix action reporting.
- Add focused tests only when introducing new check IDs, remediation actions, or
  payload fields.

## Implementation Sequence
1. Re-validate scope against `task-022-...-requirements.md` acceptance criteria.
2. Apply minimal code changes within existing setup/doctor/core config files.
3. Update/add tests alongside behavior changes.
4. Run targeted checks for touched crates/tests.
5. Confirm no unrelated command behavior drift.

## File Boundaries
- CLI/setup:
  - `crates/orchestrator-cli/src/cli_types.rs`
  - `crates/orchestrator-cli/src/main.rs`
  - `crates/orchestrator-cli/src/services/operations/ops_setup.rs`
  - `crates/orchestrator-cli/src/services/operations/ops_doctor.rs`
- Core:
  - `crates/orchestrator-core/src/doctor.rs`
  - `crates/orchestrator-core/src/daemon_config.rs`
  - `crates/orchestrator-core/src/lib.rs` (exports only as needed)
- Tests:
  - `crates/orchestrator-cli/tests/setup_doctor_e2e.rs`
  - related module tests in setup/doctor/core files

## Risks and Mitigations
- Risk: payload drift breaks automation callers.
  - Mitigation: preserve existing keys and add assertions in e2e tests.
- Risk: remediation expands into unsafe side effects.
  - Mitigation: restrict fixes to local, deterministic file/bootstrap actions.
- Risk: daemon config behavior regresses under idempotent reruns.
  - Mitigation: keep idempotence assertions and atomic-write tests intact.
