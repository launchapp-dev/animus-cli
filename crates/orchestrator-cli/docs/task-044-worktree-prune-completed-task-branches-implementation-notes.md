# TASK-044 Implementation Notes: Completed Task Worktree Pruning

## Purpose
Translate TASK-044 requirements into implementation slices for `orchestrator-cli`
and `orchestrator-core` with repository-safe cleanup semantics.

## Non-Negotiable Constraints
- Keep all changes in Rust workspace crates under `crates/`.
- Keep `.ao` state updates service-driven (no direct JSON patching).
- Preserve `ao.cli.v1` JSON envelope behavior and deterministic output ordering.
- Preserve existing non-prune worktree/git behavior.
- Keep daemon auto-prune best-effort (must not block successful task
  completion).

## Proposed Change Surface

### CLI command model
- `crates/orchestrator-cli/src/cli_types.rs`
  - add `GitWorktreeCommand::Prune(...)`.
  - add prune args (`repo`, `dry_run`, optional remote deletion controls,
    confirmation token).
  - update help strings to clarify safety and preview behavior.

### Git worktree operations
- `crates/orchestrator-cli/src/services/operations/ops_git/worktree.rs`
  - add prune handler branch in `handle_git_worktree`.
  - implement:
    - worktree discovery,
    - terminal-task filtering,
    - dry-run payload,
    - live prune execution and summary output.
  - preserve existing per-command JSON/non-JSON output conventions.

- `crates/orchestrator-cli/src/services/operations/ops_git/store.rs`
  - add reusable helper(s) for prune candidate derivation where appropriate
    (parsing/matching helpers should stay deterministic and testable).

### Daemon config and post-merge integration
- `crates/orchestrator-core/src/daemon_config.rs`
  - add persisted boolean for auto-pruning completed worktrees.
  - extend patch/update helpers and tests to preserve idempotency.

- `crates/orchestrator-cli/src/services/runtime/runtime_daemon.rs`
  - wire new daemon config flag in `ao daemon config` read/write output.
  - add run/start override arg plumbing where required.

- `crates/orchestrator-cli/src/services/runtime/runtime_daemon/daemon_run.rs`
  - map run override to env override lifecycle (set + restore).

- `crates/orchestrator-cli/src/services/runtime/runtime_daemon/daemon_scheduler_git_ops.rs`
  - trigger prune after successful merge completion path.
  - keep invocation local-only by default (no remote delete).
  - surface non-fatal errors/events for observability.

## Suggested Internal Structure
- Keep prune internals split into explicit stages:
  1. enumerate worktrees,
  2. match worktrees to task records,
  3. classify (`prunable` vs `skipped(reason)`),
  4. render dry-run response or execute actions,
  5. aggregate deterministic summary.
- Keep candidate ordering stable (sort by canonical worktree path before
  rendering/executing).

## Candidate Matching Guidance
- Prefer exact canonical path match against task `worktree_path`.
- Use branch-name match (`task.branch_name`) as secondary strategy.
- Use managed naming convention fallback only as tertiary strategy.
- If multiple tasks could match one worktree, treat as ambiguous and skip with
  explicit reason (no destructive guessing).

## Live Action Guidance
- Local prune:
  - run `git worktree remove --force <path>` from repo root,
  - if directory remains, remove recursively,
  - clear task `worktree_path` through task service update.
- Remote deletion (opt-in only):
  - resolve task branch deterministically,
  - apply safety skip rule for non-task/protected branch targets,
  - execute delete against selected remote and include per-branch result.
- Continue batch processing after individual failures; report failures in output.

## Daemon Auto-Prune Guidance
- Read auto-prune setting from config with env override precedence matching
  existing daemon override patterns.
- Invoke prune flow only after successful merge completion path.
- Auto-prune execution should not require interactive input and should not
  request remote branch deletion.
- Emit daemon event payload for prune run result (`attempted/pruned/skipped/failed`).

## Testing Plan
- `crates/orchestrator-cli/tests/cli_smoke.rs`
  - help coverage for new prune command/flags.
- `crates/orchestrator-cli/tests/cli_e2e.rs`
  - dry-run no-mutation contract,
  - live prune removes worktree and updates task metadata,
  - optional remote delete path and skip behavior.
- `crates/orchestrator-core/src/daemon_config.rs` tests
  - persisted default + patch idempotency for new config field.
- daemon runtime/scheduler tests
  - auto-prune invocation when enabled,
  - non-blocking behavior on prune errors,
  - event payload assertions.

## Risks and Mitigations
- Risk: incorrect task-worktree matching could prune wrong worktree.
  - Mitigation: strict matching order + ambiguity skip.
- Risk: prune failures regress workflow completion behavior.
  - Mitigation: daemon auto-prune best-effort and non-fatal error handling.
- Risk: output drift across dry-run/live responses.
  - Mitigation: shared response builder and stable field ordering.
- Risk: destructive remote branch deletion misuse.
  - Mitigation: explicit opt-in flag + safety skip rules + confirmation gate.
