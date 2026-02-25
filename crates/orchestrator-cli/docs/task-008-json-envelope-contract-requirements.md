# TASK-008 Requirements: Contract Tests for JSON Output Envelope Stability

## Phase
- Workflow phase: `requirements`
- Workflow ID: `392dadc3-7c41-4e01-bd50-c1e882dcf415`
- Task: `TASK-008`

## Objective
Define deterministic regression coverage for `ao --json` output so envelope shape
and error/exit-code mappings remain stable across representative command paths.

## Existing Baseline Audit

| Coverage area | Current location | Current state | Gap |
| --- | --- | --- | --- |
| Success envelope smoke checks | `crates/orchestrator-cli/tests/cli_smoke.rs` | asserts `schema=ao.cli.v1` and `ok=true` for a few commands | does not validate contract breadth or exit behavior |
| Error envelope smoke checks | `crates/orchestrator-cli/tests/support/test_harness.rs`, `crates/orchestrator-cli/tests/cli_e2e.rs` | validates `schema` and `ok=false`; one `not_found` assertion | no matrix for error-code mappings |
| Error classification logic | `crates/orchestrator-cli/src/shared/output.rs`, `crates/orchestrator-cli/src/shared.rs` tests | unit-level mapping checks for selected messages | no command-level proof that CLI status and envelope `error.exit_code` stay aligned |

## Scope
In scope for the implementation phase following this requirements pass:
- Add contract tests for `--json` success and error envelopes in
  `crates/orchestrator-cli/tests/`.
- Validate error-code mapping via representative command failures for:
  - `invalid_input` -> `2`
  - `not_found` -> `3`
  - `conflict` -> `4`
  - `unavailable` -> `5`
  - fallback `internal` -> `1`
- Assert process exit status matches envelope `error.exit_code` for mapped
  failures.
- Keep tests deterministic by using isolated temp project/config roots.

Out of scope for this task:
- Changing envelope schema (`ao.cli.v1`) or remapping exit-code semantics.
- Changing clap parse/help formatting behavior for pre-dispatch argument errors.
- Expanding to non-JSON output contracts.

## Constraints
- Only test failures that pass through `main.rs` error emission path
  (`emit_cli_error`), not clap parser preflight exits.
- Avoid external network/service dependencies; use commands that fail
  deterministically in local temp roots.
- Keep tests repository-safe:
  - no edits to `.ao/*.json` by hand
  - no destructive git operations
  - isolated filesystem state per test
- Keep runtime bounded:
  - use short runner connect timeout in tests that trigger unavailable mapping.

## JSON Envelope Contract
For `--json` calls that reach command dispatch:

Success contract:
- top-level keys: `schema`, `ok`, `data`
- `schema` is exactly `ao.cli.v1`
- `ok` is exactly `true`
- `data` exists (object/array/scalar accepted by command)
- payload emitted on `stdout`

Error contract:
- top-level keys: `schema`, `ok`, `error`
- `schema` is exactly `ao.cli.v1`
- `ok` is exactly `false`
- `error` object contains `code`, `message`, `exit_code`
- process exit status equals `error.exit_code`
- payload emitted on `stderr`

## Representative Command Matrix

| Case | Command pattern (with `--json`) | Expected `error.code` | Expected `error.exit_code` |
| --- | --- | --- | --- |
| Success envelope | `version` (or `planning vision get`) | n/a | process exit `0` |
| Invalid input mapping | `task status --id TASK-404 --status not-a-status` | `invalid_input` | `2` |
| Not-found mapping | `task get --id TASK-404` | `not_found` | `3` |
| Conflict mapping | create architecture entity `E1`, then create `E1` again | `conflict` | `4` |
| Unavailable mapping | `agent runner-status --start-runner false` with isolated empty runner config | `unavailable` | `5` |
| Internal fallback mapping | run a command with `--project-root` pointing to a regular file path | `internal` | `1` |

## Acceptance Criteria
- `AC-01`: At least one passing command verifies success envelope contract
  (`schema`, `ok=true`, `data` on stdout).
- `AC-02`: Command-level tests verify `invalid_input`, `not_found`, `conflict`,
  `unavailable`, and `internal` mappings with expected `error.code`.
- `AC-03`: For each mapped failure, process exit status equals envelope
  `error.exit_code`.
- `AC-04`: Error envelopes for all mapped failures preserve `schema=ao.cli.v1`
  and `ok=false`.
- `AC-05`: Tests are deterministic in isolated temp roots and do not depend on
  pre-existing daemon/runner state.
- `AC-06`: Existing JSON smoke tests continue to pass without behavior changes.

## Verification Matrix

| Requirement | Verification method |
| --- | --- |
| `AC-01`, `AC-04` | JSON envelope assertions in dedicated CLI contract tests |
| `AC-02`, `AC-03` | Per-case command invocations asserting both envelope fields and process exit code |
| `AC-05` | Harness-level temp dirs + explicit env isolation (`AO_CONFIG_DIR`/runner timeout) |
| `AC-06` | Run `cargo test -p orchestrator-cli --test cli_smoke --test cli_e2e --test <new_contract_test>` |

## Deterministic Deliverables for Implementation Phase
- Add a dedicated JSON contract test module under
  `crates/orchestrator-cli/tests/`.
- Extend test harness utilities as needed to expose command exit status with
  parsed JSON envelope.
- Add representative regression cases for each mapped error category plus
  success envelope contract assertions.
