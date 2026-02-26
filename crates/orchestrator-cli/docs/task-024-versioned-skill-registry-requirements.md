# TASK-024 Requirements: Versioned Skill Registry Lifecycle Commands

## Phase
- Workflow phase: `requirements`
- Workflow ID: `9935d2a3-d425-4a2d-a2e3-e5aeda61d2c8`
- Task: `TASK-024`

## Objective
Define a deterministic, repository-safe `ao skill` lifecycle that supports:
- `search`
- `install`
- `list`
- `update`
- `publish`

The lifecycle must resolve versions with explicit precedence rules and produce a
reproducible lockfile so repeated runs on the same inputs produce identical
resolution results.

## Existing Baseline Audit

| Capability area | Current location | Current state | Gap |
| --- | --- | --- | --- |
| CLI command surface | `crates/orchestrator-cli/src/cli_types.rs` | no `skill` top-level command group | no lifecycle entry point |
| Runtime dispatch | `crates/orchestrator-cli/src/main.rs`, `crates/orchestrator-cli/src/services/operations.rs` | no `handle_skill` wiring | no execution path |
| Skill state persistence | `crates/orchestrator-cli/src/services/operations/ops_common.rs` (generic helpers only) | no skill registry/lock store | no managed versioned skill state |
| Resolution behavior | n/a | no skill version resolver | no precedence contract |
| Reproducibility proof | `crates/orchestrator-cli/tests/` | no skill lockfile tests | no deterministic install/update guarantees |

## Scope
In scope for the implementation phase after this requirements pass:
- Introduce top-level `ao skill` command group with subcommands:
  - `ao skill search`
  - `ao skill install`
  - `ao skill list`
  - `ao skill update`
  - `ao skill publish`
- Add version-aware skill metadata and source registry model.
- Define and implement a deterministic resolution precedence chain.
- Add project-scoped lockfile behavior for reproducible installs/updates.
- Preserve AO output semantics for both JSON and non-JSON modes.
- Add test coverage for lifecycle flows, precedence, and lock determinism.

Out of scope for this task:
- Executing skill payloads at runtime.
- Interactive auth/login or external registry identity flows.
- Web UI/TUI affordances for skill management.
- Manual edits to `.ao/*.json` outside AO command flows.

## Constraints
- Keep implementation Rust-only under `crates/`.
- Preserve `ao.cli.v1` response envelope behavior when `--json` is enabled.
- Preserve current exit-code mapping:
  - `2` invalid input
  - `3` not found
  - `4` conflict
  - `5` unavailable
  - `1` internal
- Persist state via atomic JSON writes (existing `write_json_pretty` path).
- Keep resolution deterministic:
  - stable ordering of candidate lists and lock entries
  - deterministic tie-break rules when multiple candidates match
  - no non-deterministic lockfile fields (for example wall-clock timestamps)

## State and Versioning Contract
Project-scoped files introduced by this task:
- `.ao/state/skills-registry.v1.json`
- `.ao/state/skills-lock.v1.json`

Minimum lock entry fields:
- `name`
- `version`
- `source`
- `integrity`
- `artifact`

Rules:
- Lock entries are unique by `name` + `source`.
- Lock entries are serialized in stable order (`name`, then `source`).
- Re-running `install`/`update` with unchanged inputs must produce byte-stable
  lockfile content.

## Resolution Precedence Contract
For `install` and `update`, version resolution must follow this order:
1. Explicit CLI constraints (`--version`, `--source`, `--registry`).
2. Existing lockfile pin for the target skill (when present).
3. Project skill registry constraints/defaults.
4. Registry catalog candidates.

Additional resolver rules:
- Prefer stable releases over pre-release versions unless explicitly allowed.
- When multiple versions remain, choose highest semver.
- If still tied, apply deterministic lexical tie-break on source identifier.

## Command Behavior Contract

| Command | Required behavior | Mutation |
| --- | --- | --- |
| `skill search` | Return matching skills from configured registry sources in deterministic order | none |
| `skill install` | Resolve one deterministic version and record install + lock entry | registry + lockfile |
| `skill list` | Show installed skills with resolved version, source, and lock status | none |
| `skill update` | Re-resolve one/all skills under precedence rules and rewrite lock when changed | registry + lockfile |
| `skill publish` | Validate package metadata, enforce version uniqueness, and register new version | registry catalog |

## Error Contract
- Unsatisfied version constraint: `invalid_input` (`2`)
- Missing skill/version/source: `not_found` (`3`)
- Duplicate publish (`name` + `version` at same source): `conflict` (`4`)
- Registry backend unavailable: `unavailable` (`5`)

All JSON errors must remain in `ao.cli.v1` envelope shape.

## Acceptance Criteria
- `AC-01`: `ao skill` top-level command and all five lifecycle subcommands are
  available in CLI help/dispatch.
- `AC-02`: `skill search` outputs deterministic result ordering for identical
  query + registry inputs.
- `AC-03`: `skill install` writes both installed-state and lockfile entries with
  resolved version, source, and integrity.
- `AC-04`: `skill list` reflects persisted install state and lock alignment.
- `AC-05`: `skill update` applies resolver precedence and only rewrites lockfile
  when resolved content changes.
- `AC-06`: `skill publish` rejects duplicate versions in the same registry with
  `conflict` semantics.
- `AC-07`: Resolver precedence follows the required order and is test-covered.
- `AC-08`: Lockfile serialization is reproducible (stable ordering and
  byte-stable content across repeated no-op runs).
- `AC-09`: JSON output for success/error remains compliant with `ao.cli.v1`
  envelope contract.
- `AC-10`: Implementation remains repository-safe and avoids manual `.ao` state
  file edits.

## Verification Matrix

| Requirement | Verification method |
| --- | --- |
| `AC-01` | CLI parsing/dispatch integration test for `skill` command family |
| `AC-02` | search contract tests with fixture registry and stable ordering assertions |
| `AC-03`, `AC-04` | install/list integration tests asserting persisted registry + lock state |
| `AC-05`, `AC-07` | update + precedence tests (CLI and unit-level resolver cases) |
| `AC-06` | publish duplicate negative test (`conflict`) |
| `AC-08` | deterministic lockfile test (repeat command, compare bytes) |
| `AC-09` | JSON envelope assertions for representative success/error skill commands |
| `AC-10` | file-path assertions restricting writes to expected AO-managed skill files |

## Deterministic Deliverables for Implementation Phase
- CLI command/arg additions in:
  - `crates/orchestrator-cli/src/cli_types.rs`
  - `crates/orchestrator-cli/src/main.rs`
  - `crates/orchestrator-cli/src/services/operations.rs`
- New skill operations implementation module(s) under
  `crates/orchestrator-cli/src/services/operations/`.
- Skill lifecycle tests under `crates/orchestrator-cli/tests/`.
- Task-level implementation notes aligned to this requirements contract.
