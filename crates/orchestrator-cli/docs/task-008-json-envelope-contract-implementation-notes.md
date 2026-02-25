# TASK-008 Implementation Notes: JSON Envelope Contract Regression Coverage

## Purpose
Translate TASK-008 requirements into concrete, low-risk test changes in
`orchestrator-cli` that lock envelope shape and mapped exit-code behavior.

## Non-Negotiable Constraints
- Keep scope to test and helper updates; do not change CLI envelope semantics.
- Keep work inside `crates/orchestrator-cli/`.
- Keep tests deterministic and isolated from user/global AO state.
- Avoid manual mutation of `.ao` state files.

## Proposed Change Surface

### New test module
- `crates/orchestrator-cli/tests/cli_json_contract.rs`
  - success envelope contract test (`version` or `planning vision get`)
  - error mapping tests for `invalid_input`, `not_found`, `conflict`,
    `unavailable`, and fallback `internal`
  - assertions for both parsed envelope fields and process exit status

### Harness extension
- `crates/orchestrator-cli/tests/support/test_harness.rs`
  - add helper returning both parsed JSON payload and raw process exit status
  - keep existing `run_json_ok` / `run_json_err` for compatibility
  - support per-command extra env when needed (runner timeout)

### Optional minor touchpoints
- `crates/orchestrator-cli/tests/cli_smoke.rs`
  - keep as fast smoke; avoid duplicating full contract matrix

## Scenario Notes for Determinism
- `invalid_input`:
  - use `task status --status not-a-status` so parsing fails before any state
    dependency.
- `not_found`:
  - use a guaranteed-missing task ID in fresh temp project root.
- `conflict`:
  - create architecture entity once, then create same ID again.
- `unavailable`:
  - use isolated temp config dir and
    `agent runner-status --start-runner false`.
  - set `AO_RUNNER_CONNECT_TIMEOUT_SECS=1` in test env to keep runtime bounded.
- `internal`:
  - invoke command with `--project-root` targeting a regular file, forcing
    runtime bootstrap failure outside classified message patterns.

## Assertion Contract
For every mapped failure case:
- parse JSON from `stderr`
- assert:
  - `schema == "ao.cli.v1"`
  - `ok == false`
  - `error.code` matches expected mapping
  - `error.exit_code` matches expected mapping
  - OS process exit status equals `error.exit_code`

For success case:
- parse JSON from `stdout`
- assert:
  - `schema == "ao.cli.v1"`
  - `ok == true`
  - `data` is present
  - process exit status is `0`

## Implementation Sequence
1. Extend harness with a status-aware JSON execution helper.
2. Add `cli_json_contract.rs` with one success and five error mapping cases.
3. Run targeted tests and tighten any fragile assertions.
4. Run existing smoke/e2e tests to verify no regressions in current coverage.

## Risks and Mitigations
- Risk: flaky unavailable case due to runner connect timeout.
  - Mitigation: explicit short timeout env var in test command.
- Risk: OS-specific bootstrap error text for internal fallback case.
  - Mitigation: assert mapped `code`/`exit_code`, not exact error string.
- Risk: overlap with existing smoke tests causing duplication drift.
  - Mitigation: keep new file focused on contract mapping matrix only.

## Validation Targets for Implementation Phase
- `cargo test -p orchestrator-cli --test cli_json_contract`
- `cargo test -p orchestrator-cli --test cli_smoke --test cli_e2e`
