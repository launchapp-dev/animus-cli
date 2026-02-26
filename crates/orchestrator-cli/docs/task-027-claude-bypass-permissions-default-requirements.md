# TASK-027 Requirements: Default Claude Permission Bypass to Opt-In

## Phase
- Workflow phase: `requirements`
- Workflow ID: `9872abd3-0ba3-4663-84e4-a7b990d78ceb`
- Task: `TASK-027`

## Objective
Change AO's default Claude launch behavior so permission bypass is opt-in.
When `AO_CLAUDE_BYPASS_PERMISSIONS` is unset, AO must run Claude with normal
permission checks instead of injecting `--permission-mode bypassPermissions`.

## Existing Baseline Audit

| Coverage area | Current location | Current state | Gap |
| --- | --- | --- | --- |
| CLI shared runtime contract override | `crates/orchestrator-cli/src/shared/runner.rs` (`claude_bypass_permissions_enabled`) | unset `AO_CLAUDE_BYPASS_PERMISSIONS` defaults to `true` | bypass mode is silently enabled by default |
| Daemon workflow runtime override | `crates/orchestrator-cli/src/services/runtime/runtime_daemon/daemon_scheduler_runtime_support.rs` (`claude_bypass_permissions_enabled`) | unset env defaults to `true` | daemon phase execution inherits unsafe default |
| Claude permission-mode injection | `runner.rs` and `daemon_scheduler_runtime_support.rs` (`inject_claude_permission_mode`) | adds `--permission-mode bypassPermissions` when bypass is enabled | default path violates least-privilege expectation |
| Test coverage | `crates/orchestrator-cli/src/shared/runner.rs` tests and `crates/orchestrator-cli/src/services/runtime/runtime_daemon/daemon_scheduler.rs` tests | tests currently assert bypass enabled when env is unset | tests encode the unsafe default contract |

## Scope
In scope for implementation after this requirements phase:
- Change Claude bypass default to `false` in both runtime paths:
  - `crates/orchestrator-cli/src/shared/runner.rs`
  - `crates/orchestrator-cli/src/services/runtime/runtime_daemon/daemon_scheduler_runtime_support.rs`
- Preserve explicit opt-in behavior:
  - `AO_CLAUDE_BYPASS_PERMISSIONS=true` still injects bypass mode.
- Preserve explicit disable behavior:
  - `AO_CLAUDE_BYPASS_PERMISSIONS=false` keeps normal permissions.
- Update and expand tests to assert the new default contract for both direct CLI
  and daemon-managed phase execution paths.

Out of scope for this task:
- Any changes to Codex or Gemini launch overrides.
- Any new environment variable names or deprecation flows.
- Any changes to Claude's own CLI semantics outside AO argument injection.
- Manual edits to `/.ao/*.json`.

## Behavior Contract

| Case ID | Scenario | Expected behavior |
| --- | --- | --- |
| `CB-01` | `tool != claude` | no Claude permission-mode injection changes |
| `CB-02` | `tool = claude`, env unset | do **not** inject `--permission-mode bypassPermissions` |
| `CB-03` | `tool = claude`, env set truthy (`1/true/yes/on` or non-false token) | inject `--permission-mode bypassPermissions` |
| `CB-04` | `tool = claude`, env set falsy (`0/false/no/off`) | do **not** inject bypass mode |
| `CB-05` | env explicitly false in daemon path | daemon runtime contract mirrors CLI shared behavior |

## Constraints
- Preserve current env-value parsing semantics (`false/no/off/0` disable; other
  non-empty values enable) while changing only the default fallback value.
- Keep behavior deterministic and side-effect free outside launch arg mutation.
- Keep both code paths aligned so daemon-driven and direct `agent run` behavior
  cannot diverge.
- No destructive git operations; repository-safe edits only.

## Acceptance Criteria
- `AC-01`: In `shared/runner.rs`, unset `AO_CLAUDE_BYPASS_PERMISSIONS` no
  longer injects `--permission-mode bypassPermissions`.
- `AC-02`: In daemon runtime support, unset
  `AO_CLAUDE_BYPASS_PERMISSIONS` no longer injects bypass mode.
- `AC-03`: Explicit truthy `AO_CLAUDE_BYPASS_PERMISSIONS` still injects bypass
  mode in both code paths.
- `AC-04`: Explicit falsy `AO_CLAUDE_BYPASS_PERMISSIONS` continues to disable
  bypass mode in both code paths.
- `AC-05`: Existing and new tests deterministically enforce the default-off
  contract and opt-in behavior.

## Verification Matrix

| Requirement | Verification method |
| --- | --- |
| `AC-01`, `AC-03`, `AC-04` | unit tests in `crates/orchestrator-cli/src/shared/runner.rs` covering default-off, truthy opt-in, and falsy disable |
| `AC-02`, `AC-03`, `AC-04` | daemon/runtime tests in `crates/orchestrator-cli/src/services/runtime/runtime_daemon/daemon_scheduler.rs` validating mirrored behavior |
| `AC-05` | targeted test run for `orchestrator-cli` runtime/shared modules |

## Deterministic Deliverables for Implementation Phase
- Minimal code change: fallback default flips from `true` to `false` in both
  Claude bypass helpers.
- Test updates that encode default-off behavior and explicit opt-in path.
- No unrelated runtime-contract or scheduler behavior changes.
