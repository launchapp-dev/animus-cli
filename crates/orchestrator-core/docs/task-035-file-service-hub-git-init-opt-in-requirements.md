# TASK-035 Requirements: Make FileServiceHub Git Init Opt-In

## Phase
- Workflow phase: `requirements`
- Workflow ID: `8d14d7b7-bb6d-4c5f-851a-518cbf5747c7`
- Task: `TASK-035`
- Linked requirement: `REQ-022`

## Objective
Remove surprising git side effects from `FileServiceHub` bootstrap. Constructing
or loading a file-backed service hub must not implicitly run `git init` or
create a bootstrap commit. Git initialization must be explicit and opt-in.

## Current Baseline Audit

| Surface | Current location | Current state | Gap |
| --- | --- | --- | --- |
| Hub construction | `crates/orchestrator-core/src/services.rs` (`FileServiceHub::new`) | always calls `bootstrap_project_base_configs` | construction performs hidden side effects |
| Project bootstrap | `crates/orchestrator-core/src/services.rs` (`bootstrap_project_base_configs`) | unconditionally calls `ensure_project_git_repository` | `.ao` bootstrap is coupled to git bootstrap |
| Git bootstrap helper | `crates/orchestrator-core/src/services.rs` (`ensure_project_git_repository`) | runs `git init` and creates empty `HEAD` commit with hardcoded AO bootstrap identity if missing | behavior is not caller-controlled |
| Project CRUD bootstrap hooks | `crates/orchestrator-core/src/services/project_impl.rs` (`create`, `upsert`, `load`) | each path invokes `bootstrap_project_base_configs` for target path | project metadata operations mutate git state |
| Regression expectations | `crates/orchestrator-core/src/services/tests.rs` (`file_hub_project_create_bootstraps_base_configs_for_project_path`) | asserts git repo and `HEAD` exist after project create | tests currently encode implicit git side effects |

## Problem Statement
`FileServiceHub` bootstrap currently mutates git state as a hidden side effect
of object construction and project metadata workflows. This behavior is
surprising, makes read-oriented operations mutating by default, and creates
empty commits with a hardcoded identity without explicit user intent.

## Scope
In scope for implementation after this requirements phase:
- Decouple `.ao` bootstrap from git bootstrap inside `orchestrator-core`.
- Make default `FileServiceHub` construction and base project bootstrap
  git-neutral.
- Keep a dedicated explicit API path for git repository initialization.
- Update call sites and tests so git initialization happens only from
  intentional code paths.
- Update local docs and implementation notes to reflect new contract.

Out of scope:
- Changing `.ao` schema or persistence formats.
- Redesigning daemon/worktree git flows unrelated to bootstrap side effects.
- Adding desktop-wrapper dependencies.
- Manual edits to `/.ao/*.json`.

## Constraints
- Preserve existing `.ao` bootstrap behavior:
  - create project root when missing,
  - initialize `.ao` state/config files when missing.
- No git subprocess calls during default `FileServiceHub::new` path.
- Explicit git bootstrap path must keep deterministic error context.
- Behavior must stay cross-platform (Unix + non-Unix).
- Tests must not depend on global git user config.

## Functional Requirements

### FR-01: Git-Neutral Hub Construction
- `FileServiceHub::new(project_root)` must not initialize a git repository.
- `FileServiceHub::new(project_root)` must not create an empty bootstrap commit.

### FR-02: Git-Neutral Base Config Bootstrap
- `bootstrap_project_base_configs` (or its replacement default path) must only
  bootstrap `.ao` files and related AO state, not git state.

### FR-03: Explicit Git Bootstrap Path
- A separate explicit API must exist for initializing git state when a caller
  intentionally opts in.
- The explicit API may perform repository init and optional `HEAD` seeding using
  current bootstrap behavior, but only when called directly by a caller.

### FR-04: Call-Site Intentionality
- `project_impl` and other `FileServiceHub` call sites must stop relying on
  implicit git side effects from base bootstrap.
- Any caller that needs git must invoke explicit git bootstrap intentionally.

### FR-05: Regression Coverage
- Add or update tests proving:
  - default `FileServiceHub` bootstrap does not create `.git`,
  - `.ao` bootstrap still occurs,
  - explicit git bootstrap path still works when invoked.

### FR-06: Documentation Consistency
- Requirements and implementation notes must capture the new contract:
  git bootstrap is opt-in; `.ao` bootstrap remains default.

## Acceptance Criteria
- `AC-01`: Constructing `FileServiceHub` in a fresh directory does not create a
  `.git/` directory.
- `AC-02`: Constructing `FileServiceHub` in a fresh directory does not create a
  `HEAD` commit.
- `AC-03`: Project create/load/upsert bootstrap `.ao` files without implicit git
  init side effects.
- `AC-04`: An explicit git bootstrap path remains available and functional for
  callers that intentionally request it.
- `AC-05`: Tests covering both default and explicit paths pass in
  `orchestrator-core`.
- `AC-06`: No `.ao` schema or CLI output contract regressions are introduced.

## Testable Acceptance Checklist
- `T-01`: Unit/integration test that `FileServiceHub::new` on non-git temp dir
  leaves `.git` absent.
- `T-02`: Unit/integration test that `.ao/core-state.json` and required AO
  config files are still bootstrapped.
- `T-03`: Unit/integration test for explicit git bootstrap helper creating repo
  and `HEAD` commit when invoked.
- `T-04`: Existing `orchestrator-core` tests updated so they no longer assume
  implicit git bootstrap.
- `T-05`: `cargo test -p orchestrator-core` passes.

## Verification Matrix
| Requirement area | Verification method |
| --- | --- |
| No implicit git mutation | constructor/bootstrap tests on fresh temp dirs |
| AO bootstrap preservation | `.ao` file existence assertions |
| Explicit opt-in git flow | dedicated helper test invoking opt-in API |
| Non-regression | existing `orchestrator-core` test suite pass |

## Deterministic Deliverables for Implementation Phase
- Default `FileServiceHub` bootstrap path without git subprocess side effects.
- Explicit opt-in git bootstrap API retained for intentional callers.
- Updated test coverage for both default and explicit behavior.
- Updated task docs reflecting the new bootstrap contract.
