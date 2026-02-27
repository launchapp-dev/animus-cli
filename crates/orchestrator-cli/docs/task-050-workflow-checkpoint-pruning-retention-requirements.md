# TASK-050 Requirements: Workflow Checkpoint Pruning and Retention Policy

## Phase
- Workflow phase: `requirements`
- Workflow ID: `dbe83be0-9396-45b1-ac7e-8e9db5bb2db0`
- Task: `TASK-050`
- Requirement: unlinked in current task metadata

## Objective
Add deterministic checkpoint retention so workflow state does not grow
unbounded, while preserving debuggability and compatibility.

Target behavior from task brief:
- support retention by keeping the latest `N` checkpoints per phase (default
  `10`),
- or prune checkpoints older than a configurable age,
- add `ao workflow checkpoints prune`,
- optionally support auto-prune on workflow completion.

## Current Baseline Audit

| Surface | Current location | Current behavior | Gap |
| --- | --- | --- | --- |
| Checkpoint persistence | `crates/orchestrator-core/src/workflow/state_manager.rs` (`save_checkpoint`) | increments `checkpoint_count`, appends to `checkpoint_metadata.checkpoints`, writes full workflow snapshot per checkpoint | no retention or prune API; metadata and checkpoint files grow without bound |
| Checkpoint metadata model | `crates/orchestrator-core/src/types.rs` (`WorkflowCheckpoint`) | stores `number`, `timestamp`, `reason`, `machine_state`, `status` | no explicit phase identifier for per-phase retention grouping |
| Workflow service API | `crates/orchestrator-core/src/services.rs` + `services/workflow_impl.rs` | exposes `list_checkpoints` + `get_checkpoint` only | no service-level prune operation |
| CLI workflow checkpoints command | `crates/orchestrator-cli/src/cli_types/workflow_types.rs` + `services/operations/ops_workflow.rs` | supports `workflow checkpoints list/get` | no prune command or retention-mode arguments |
| Workflow completion flow | `crates/orchestrator-core/src/services/workflow_impl.rs` and daemon scheduler flows | saves checkpoints across run/resume/pause/complete transitions | no optional auto-prune hook after completion |

## Problem Statement
Workflows can emit large checkpoint histories (example from task brief: 1500+
checkpoints for a 4-phase pipeline). Current storage is append-only, which
inflates:
- `.ao/workflow-state/<workflow>.json` via unbounded checkpoint metadata,
- `.ao/workflow-state/checkpoints/<workflow>/checkpoint-*.json` file count.

This creates avoidable disk overhead and longer checkpoint enumeration costs.

## Scope
In scope for implementation after this requirements phase:
- Add `ao workflow checkpoints prune`.
- Add deterministic retention policy support:
  - count mode: keep latest `N` checkpoints per phase (default `10`),
  - age mode: prune checkpoints older than configured age.
- Add service/core pruning implementation so CLI does not mutate `.ao` files
  directly.
- Preserve checkpoint number monotonicity and existing `get/list` semantics.
- Add focused regression tests for retention behavior and CLI contract.
- Optional: add auto-prune on workflow completion behind explicit opt-in toggle.

Out of scope:
- Deleting workflow records (`.ao/workflow-state/<id>.json`) entirely.
- Pruning decision history entries.
- Changing workflow state-machine semantics.
- Manual edits to `.ao/*.json`.

## Constraints
- Keep prune behavior deterministic and repository-safe.
- Live prune must be explicit and safely gated:
  - dry-run preview first,
  - destructive apply path with confirmation token.
- Keep checkpoint numbering stable:
  - no renumbering existing checkpoint ids/files,
  - `checkpoint_count` remains monotonic.
- Changes must remain additive to existing JSON envelope (`ao.cli.v1`) behavior.
- Preserve compatibility with legacy checkpoint data that lacks phase metadata.

## Retention Policy Contract

### Policy Modes
- `count` mode (default): keep latest `N` checkpoints per phase (`N=10`
  default).
- `age` mode: prune checkpoints whose timestamp is older than
  `now - max_age_hours`.

### Mode Selection
- CLI must expose explicit retention inputs so mode selection is deterministic.
- Count and age inputs must not silently conflict; command must reject ambiguous
  combinations or apply a clearly documented precedence rule.

### Phase Resolution Contract
Per checkpoint, resolve phase id in deterministic order:
1. explicit `phase_id` on checkpoint metadata (new writes),
2. fallback inference from persisted checkpoint snapshot (`current_phase` or
   current phase index),
3. fallback bucket `"unknown"` for unresolved legacy data.

### Prune Mutation Contract
When pruning applies:
- remove selected checkpoint files from
  `.ao/workflow-state/checkpoints/<workflow_id>/`,
- remove corresponding entries from `workflow.checkpoint_metadata.checkpoints`,
- preserve surviving entries and order deterministically,
- keep `checkpoint_count` unchanged (monotonic id source),
- persist updated workflow atomically.

## Functional Requirements

### FR-01: New Command Surface
- Add `workflow checkpoints prune` subcommand with workflow id input plus
  retention policy arguments.

### FR-02: Dry-Run Contract
- `--dry-run` must produce deterministic plan output and perform zero mutations.

### FR-03: Confirmation-Gated Apply
- Live prune path must require explicit confirmation token when candidates exist.

### FR-04: Count-Based Retention
- Default behavior keeps latest `N` checkpoints per phase (`N=10` default).

### FR-05: Age-Based Retention
- Age mode prunes checkpoints older than configured age using UTC timestamps.

### FR-06: Legacy Compatibility
- Pruning must work on historical checkpoint data that predates phase metadata.

### FR-07: Service-Level API
- Add a workflow-service prune operation so command handlers remain thin and
  `.ao` mutations stay service-driven.

### FR-08: Output Determinism
- Command output must include deterministic summary fields:
  - total checkpoints scanned,
  - candidate count,
  - pruned count,
  - kept count,
  - skipped/fallback inference counts where relevant.

### FR-09: Optional Auto-Prune on Completion
- If opt-in is enabled, workflow completion path triggers prune using configured
  retention policy.
- Auto-prune failures must be non-fatal to workflow completion.

### FR-10: Regression Coverage
- Add tests for mode parsing/validation, prune selection, dry-run no-mutation,
  live prune mutation, and compatibility constraints.

## Acceptance Criteria
- `AC-01`: `ao workflow checkpoints prune` exists.
- `AC-02`: default prune behavior keeps latest `10` checkpoints per phase.
- `AC-03`: configurable `keep-per-phase` values are accepted and validated.
- `AC-04`: age-based pruning is supported and validated.
- `AC-05`: dry-run returns deterministic candidate summary and makes no file or
  workflow-state mutations.
- `AC-06`: live prune deletes targeted checkpoint files and removes matching
  metadata entries.
- `AC-07`: surviving checkpoints remain addressable via `workflow checkpoints
  get`; pruned ones return not-found.
- `AC-08`: `checkpoint_count` stays monotonic after prune.
- `AC-09`: legacy checkpoints without phase metadata are handled deterministically.
- `AC-10`: optional auto-prune path (if enabled) runs post-completion and does
  not block successful completion on prune errors.
- `AC-11`: targeted tests cover retention mode behavior and compatibility.

## Testable Acceptance Checklist
- `T-01`: CLI parse test for `workflow checkpoints prune` args and validation.
- `T-02`: state-manager/core test for count-mode retention grouping by phase.
- `T-03`: state-manager/core test for age-mode retention cutoff behavior.
- `T-04`: dry-run test verifies zero mutations to checkpoint files and workflow
  metadata.
- `T-05`: live prune test verifies file deletions + metadata updates.
- `T-06`: legacy checkpoint test without `phase_id` proves deterministic fallback.
- `T-07`: workflow service test verifies list/get behavior after pruning.
- `T-08`: optional auto-prune completion test verifies non-fatal behavior when
  prune fails.

## Verification Matrix

| Requirement area | Verification method |
| --- | --- |
| FR-01, FR-02, FR-03 | CLI parse tests + command-handler dry-run/apply tests |
| FR-04, FR-05 | core retention selection tests (count + age) |
| FR-06 | legacy-data compatibility tests using fixtures without `phase_id` |
| FR-07 | workflow service API tests |
| FR-08 | JSON contract assertions for prune summary fields |
| FR-09 | workflow completion/daemon path test when auto-prune enabled |
| FR-10 | targeted `cargo test -p orchestrator-core` and `-p orchestrator-cli` |

## Implementation Notes Input (Next Phase)
Primary change targets:
- `crates/orchestrator-core/src/types.rs`
- `crates/orchestrator-core/src/workflow/state_manager.rs`
- `crates/orchestrator-core/src/services.rs`
- `crates/orchestrator-core/src/services/workflow_impl.rs`
- `crates/orchestrator-cli/src/cli_types/workflow_types.rs`
- `crates/orchestrator-cli/src/services/operations/ops_workflow.rs`

Likely test targets:
- `crates/orchestrator-core/src/workflow/tests.rs`
- `crates/orchestrator-core/src/services/tests.rs`
- `crates/orchestrator-cli/src/cli_types/mod.rs`
- `crates/orchestrator-cli/tests/cli_smoke.rs`
- focused CLI integration tests for prune command behavior

Optional auto-prune toggle surfaces (if included in this slice):
- `crates/orchestrator-core/src/daemon_config.rs`
- daemon runtime wiring under
  `crates/orchestrator-cli/src/services/runtime/runtime_daemon/`

## Deterministic Deliverables for Implementation Phase
- Workflow checkpoint prune command with dry-run and apply paths.
- Retention engine supporting count-per-phase default and age-based mode.
- Service-driven checkpoint pruning with compatibility for legacy data.
- Focused tests proving deterministic behavior and contract stability.
