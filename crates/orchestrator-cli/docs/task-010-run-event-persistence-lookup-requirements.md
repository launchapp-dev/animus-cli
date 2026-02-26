# TASK-010 Requirements: Traceability Checks for Run/Event Persistence and Lookup

## Phase
- Workflow phase: `requirements`
- Workflow ID: `c0cdfee7-07eb-4911-91bb-ab72790e0af2`
- Task: `TASK-010`

## Objective
Define deterministic traceability checks that prove run-event persistence and
run-event lookup stay aligned under repository-scoped runtime directories.

The core contract to preserve:
- persisted run data lands in the canonical scoped runtime path
- lookup/read paths resolve the same canonical location first
- legacy lookup paths remain explicit compatibility fallbacks

## Existing Baseline Audit

| Coverage area | Current location | Current state | Gap |
| --- | --- | --- | --- |
| Runner-side run event persistence | `crates/agent-runner/src/runner/event_persistence.rs` | writes `events.jsonl` and `json-output.jsonl` to `~/.ao/<repo-scope>/runs/<run_id>` via `project_runs_root` | no explicit cross-crate traceability proof vs CLI lookup/read behavior |
| CLI run directory resolution | `crates/orchestrator-cli/src/shared/runner.rs` (`run_dir`) | resolves canonical scoped runtime run directory by default | no dedicated regression checks tying resolver output to persisted runner artifacts |
| Agent status fallback lookup | `crates/orchestrator-cli/src/shared/parsing.rs` (`read_agent_status`) | reads `events.jsonl` from `run_dir(...)` when runner query fails | no explicit tests for scoped-runtime fallback behavior and event-path traceability |
| Output lookup compatibility | `crates/orchestrator-cli/src/services/operations/ops_output.rs` | searches candidates in this order: scoped path, `<project>/.ao/runs`, `<project>/.ao/state/runs` | no regression tests for candidate precedence and fallback compatibility |
| Existing test coverage | `event_persistence.rs` and scattered CLI shared tests | partial unit coverage exists for persistence and env precedence | no end-to-end lookup traceability matrix for scoped runtime directories |

## Canonical Path Contract

For a valid `project_root` and `run_id`, canonical run JSONL directory is:
- `~/.ao/<repo-scope>/runs/<run_id>`

Where:
- `<repo-scope>` is derived from canonical project path using sanitized repo
  name + first 12 hex chars of SHA256 digest.

Lookup precedence for output operations must be deterministic:
1. canonical scoped path (`run_dir(project_root, run_id, None)`)
2. legacy `<project_root>/.ao/runs/<run_id>`
3. legacy `<project_root>/.ao/state/runs/<run_id>`

## Scope
In scope for implementation after this requirements phase:
- Add regression tests that verify persistence and lookup agreement for
  canonical scoped runtime run directories.
- Add regression tests that verify deterministic lookup precedence and legacy
  fallback behavior for `output` run/jsonl lookups.
- Add regression tests for `agent status` fallback path parsing when runner
  lookup is unavailable.
- Keep existing runtime command behavior unchanged while adding traceability
  guarantees.

Out of scope for this task:
- changes to daemon event storage (`daemon-events.jsonl`)
- changes to artifact blob storage under `.ao/artifacts`
- protocol/schema changes for `AgentRunEvent`
- manual edits to `.ao/*.json`

## Constraints
- Tests must be deterministic and isolated:
  - no dependence on host/global AO state
  - temp roots and explicit env isolation for `HOME`, `XDG_CONFIG_HOME`,
    `AO_CONFIG_DIR` as needed
- Runtime safety:
  - no destructive git operations
  - no writes outside test-owned directories
- Keep repository-scoped runtime model intact:
  - do not remap canonical run persistence location as part of this task
- Maintain backwards compatibility:
  - preserve legacy lookup fallbacks in `ops_output` candidate traversal

## Traceability Scenario Matrix

| Case ID | Scenario | Entry point | Required assertions |
| --- | --- | --- | --- |
| `TR-01` | Runner persists events to canonical scoped path | `agent-runner::RunEventPersistence::persist` | `events.jsonl` created at `~/.ao/<repo-scope>/runs/<run_id>/events.jsonl` |
| `TR-02` | Runner persists JSON output lines alongside events | `agent-runner::RunEventPersistence::persist` | `json-output.jsonl` created in same canonical run dir and includes parsed JSON payload lines |
| `TR-03` | CLI resolver matches canonical scoped path model | `orchestrator-cli::run_dir` | computed path points to `~/.ao/<repo-scope>/runs/<run_id>` when no override is passed |
| `TR-04` | Agent status fallback reads scoped run log | `read_agent_status` | status resolves from canonical `events.jsonl` and reports resolved `events_path` |
| `TR-05` | Output run lookup reads canonical scoped run dir first | `OutputCommand::Run` | when both scoped and legacy dirs exist, scoped path is chosen deterministically |
| `TR-06` | Output lookup preserves legacy compatibility | `OutputCommand::Run` / `OutputCommand::Jsonl` | legacy `<project>/.ao/runs` and `.ao/state/runs` are used when canonical path is absent |
| `TR-07` | Output JSONL lookup remains deterministic | `get_run_jsonl_entries` | merged rows are stable-sorted by timestamp hint and include source-file origin |
| `TR-08` | Unsafe run IDs are rejected in lookup APIs | `get_run_jsonl_entries` | traversal-like run IDs fail with deterministic validation error |

## Acceptance Criteria
- `AC-01`: Tests prove runner persistence writes `events.jsonl` and
  `json-output.jsonl` to canonical scoped runtime run directories (`TR-01`,
  `TR-02`).
- `AC-02`: Tests prove CLI `run_dir` computes canonical scoped run directory
  consistent with runner-side persistence (`TR-03`).
- `AC-03`: Tests prove `agent status` fallback reads and reports scoped
  `events.jsonl` path when runner query is unavailable (`TR-04`).
- `AC-04`: Tests prove `output run` prefers canonical scoped run dir over
  legacy candidates when both exist (`TR-05`).
- `AC-05`: Tests prove output lookup still supports legacy fallback paths for
  compatibility (`TR-06`).
- `AC-06`: Tests prove `output jsonl` merged view remains deterministic and
  traceable by source-file metadata (`TR-07`).
- `AC-07`: Tests prove invalid run IDs are rejected for lookup surfaces with
  deterministic errors (`TR-08`).
- `AC-08`: Existing CLI and runner behavior remains unchanged outside added
  traceability coverage.

## Verification Matrix

| Requirement | Verification method |
| --- | --- |
| `AC-01` | unit tests in `crates/agent-runner/src/runner/event_persistence.rs` |
| `AC-02` | unit tests covering `run_dir` path derivation in `orchestrator-cli` shared layer |
| `AC-03` | unit/integration tests for `read_agent_status` fallback parsing with scoped run fixtures |
| `AC-04`, `AC-05`, `AC-06`, `AC-07` | tests for `ops_output` run/jsonl lookup precedence, fallback, and validation behavior |
| `AC-08` | targeted crate test runs for `agent-runner` and `orchestrator-cli` plus existing smoke suites |

## Deterministic Deliverables for Implementation Phase
- Add traceability-focused tests across runner persistence and CLI lookup/read
  surfaces.
- Add minimal helper seams only where needed to keep tests deterministic.
- Keep persistence and lookup behavior stable while formalizing path and
  precedence guarantees.
