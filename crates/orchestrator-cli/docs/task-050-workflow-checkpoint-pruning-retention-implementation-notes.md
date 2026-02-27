# TASK-050 Implementation Notes: Workflow Checkpoint Pruning and Retention

## Phase Context
- Workflow phase: `requirements`
- Workflow ID: `dbe83be0-9396-45b1-ac7e-8e9db5bb2db0`
- Task: `TASK-050`

## Purpose
Translate TASK-050 requirements into a low-risk implementation slice that
introduces bounded workflow checkpoint retention without breaking existing
checkpoint lookup semantics.

## Non-Negotiable Constraints
- Keep all `.ao` state mutations service-driven (no direct manual JSON patching
  from command handlers).
- Keep checkpoint ids monotonic; do not renumber historical checkpoints.
- Preserve existing `ao.cli.v1` envelope behavior.
- Preserve backward compatibility for older checkpoint payloads that do not
  include phase metadata.
- Keep auto-prune on completion optional and non-fatal when enabled.

## Chosen Strategy
- Add retention support in core (`WorkflowStateManager`) and expose it through
  `WorkflowServiceApi`.
- Extend workflow checkpoint metadata with optional `phase_id` for new writes.
- Add a new CLI command `ao workflow checkpoints prune` that delegates to the
  service and supports dry-run + apply.
- Keep retention policy explicit with two modes:
  - count per phase (default),
  - max-age.

## Proposed Change Surface

### 1) Core checkpoint model and retention policy types
- `crates/orchestrator-core/src/types.rs`
  - extend `WorkflowCheckpoint` with optional phase metadata (for example
    `phase_id: Option<String>` with serde default compatibility).
  - add prune input/output structs if service API returns structured summary.

- `crates/orchestrator-core/src/lib.rs`
  - re-export any new prune policy/result types required by CLI crate.

### 2) State-manager prune implementation
- `crates/orchestrator-core/src/workflow/state_manager.rs`
  - add a prune API, for example:
    - `prune_checkpoints(workflow_id, policy, dry_run) -> PruneResult`.
  - update `save_checkpoint` to persist `phase_id` on new checkpoint metadata.
  - apply pruning by:
    - selecting candidate checkpoint numbers deterministically,
    - deleting checkpoint files when not dry-run,
    - removing matching metadata entries,
    - saving updated workflow atomically,
    - preserving `checkpoint_count` as monotonic high-water mark.

### 3) Workflow service API plumbing
- `crates/orchestrator-core/src/services.rs`
  - add workflow prune method to `WorkflowServiceApi`.

- `crates/orchestrator-core/src/services/workflow_impl.rs`
  - implement prune method in both `InMemoryServiceHub` and `FileServiceHub`.
  - file-backed implementation delegates to `WorkflowStateManager`.
  - in-memory implementation should mirror retention semantics over
    `checkpoint_metadata.checkpoints` for test parity.

### 4) CLI command surface and handler wiring
- `crates/orchestrator-cli/src/cli_types/workflow_types.rs`
  - add `WorkflowCheckpointCommand::Prune(...)` with retention args.
  - include dry-run and confirmation inputs.
  - validate positive numeric retention values through shared parsers.

- `crates/orchestrator-cli/src/services/operations/ops_workflow.rs`
  - wire prune command to workflow service prune method.
  - return deterministic JSON payload for dry-run and apply modes.
  - keep confirmation behavior aligned with existing destructive workflow
    commands.

- `crates/orchestrator-cli/tests/cli_smoke.rs`
  - ensure help output includes prune subcommand description.

### 5) Optional auto-prune on completion (if implemented in same slice)
- Option A (service-level hook):
  - invoke prune after transitions to terminal `Completed` status in workflow
    service methods.
- Option B (daemon-level hook):
  - invoke prune from daemon workflow completion handling path.
- In either option:
  - gate with explicit opt-in config/env,
  - treat failures as non-fatal and log/report outcome.

## Retention Selection Guidance

### Count Mode
- Group checkpoints by resolved phase id.
- Sort each group by checkpoint number descending.
- Keep first `N`, mark remainder as prune candidates.

### Age Mode
- Compute UTC cutoff from `max_age_hours`.
- Mark checkpoints with `timestamp < cutoff` as prune candidates.

### Phase Resolution for Legacy Data
- use `checkpoint.phase_id` when present,
- else infer from checkpoint snapshot workflow phase fields,
- else bucket as `"unknown"` so behavior stays deterministic.

## Suggested Implementation Sequence
1. Add core types for retention policy/result and backward-compatible
   checkpoint phase metadata.
2. Implement and test state-manager prune selection/mutation logic.
3. Expose prune through `WorkflowServiceApi` and file/in-memory hub
   implementations.
4. Add CLI command args + handler wiring with dry-run and confirmation path.
5. Add optional auto-prune completion hook if scope/time allows.
6. Run targeted tests and fix regressions introduced by TASK-050.

## Testing Plan
- Core unit tests:
  - count-mode retention by phase,
  - age-mode retention cutoff,
  - checkpoint-count monotonicity after prune,
  - legacy checkpoint entries without phase id.
- Core service tests:
  - list/get behavior after prune,
  - in-memory vs file-backed parity.
- CLI tests:
  - argument parsing and validation,
  - help text coverage,
  - dry-run output contract.
- Integration tests:
  - live prune removes files and metadata entries deterministically.
- Optional auto-prune tests:
  - completion-triggered prune when enabled,
  - non-fatal behavior on prune errors.

## Validation Targets
- `cargo test -p orchestrator-core workflow::tests`
- `cargo test -p orchestrator-core services::tests::workflow_service_exposes_decisions_and_checkpoints`
- `cargo test -p orchestrator-cli cli_types::tests`
- `cargo test -p orchestrator-cli --test cli_smoke`
- targeted prune-specific tests in touched modules

## Risks and Mitigations
- Risk: ambiguous retention mode behavior.
  - Mitigation: explicit mode validation and deterministic command contract.
- Risk: legacy checkpoint data missing phase context.
  - Mitigation: fallback inference order plus `"unknown"` phase bucket.
- Risk: pruning breaks checkpoint lookup expectations.
  - Mitigation: preserve numbering/high-water semantics and add list/get
    regression tests.
- Risk: auto-prune side effects on completion path.
  - Mitigation: opt-in only and non-fatal error handling.

## Deliverables for Next Phase
- Service-backed checkpoint prune capability with deterministic selection.
- CLI `workflow checkpoints prune` command with dry-run and apply flows.
- Backward-compatible checkpoint metadata evolution for phase-aware retention.
- Focused tests proving behavior correctness and compatibility.
