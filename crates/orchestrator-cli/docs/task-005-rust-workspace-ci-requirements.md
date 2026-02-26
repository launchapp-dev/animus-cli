# TASK-005 Requirements: CI Workflow for Rust-Only Workspace Checks

## Phase
- Workflow phase: `requirements`
- Workflow ID: `c3330498-09f2-456e-90b8-465730a0223d`
- Task: `TASK-005`

## Objective
Define deterministic CI coverage for core Rust workspace validation by running
package-scoped compile checks and binary help-command smoke checks for:
- `ao` (`orchestrator-cli`)
- `llm-cli-wrapper`

## Existing Baseline Audit

| Coverage area | Current location | Current state | Gap |
| --- | --- | --- | --- |
| Rust dependency policy guardrail | `.github/workflows/rust-only-dependency-policy.yml` | Runs targeted policy test for prohibited desktop-wrapper dependencies | Does not run package compile checks or CLI help smoke |
| Release build path | `.github/workflows/release.yml` | Builds release binaries for selected branches/tags | Not a general PR/push CI gate for developer feedback |
| Web UI CI | `.github/workflows/web-ui-ci.yml` | Covers web-ui test/build/smoke flows | Not scoped to Rust CLI package checks |

## Scope
In scope for implementation after this requirements phase:
- Add a dedicated Rust CI workflow in `.github/workflows/` that runs on
  `pull_request` and `push`.
- Execute package-level checks:
  - `cargo check --locked -p orchestrator-cli`
  - `cargo check --locked -p llm-cli-wrapper`
- Execute smoke help commands:
  - `cargo run --locked -p orchestrator-cli -- --help`
  - `cargo run --locked -p llm-cli-wrapper -- --help`
- Keep workflow non-interactive and deterministic for CI environments.
- Update top-level documentation to reference the task artifact docs.

Out of scope for this task:
- Replacing or restructuring release packaging workflow behavior.
- Adding cross-platform CI matrices for this validation slice.
- Expanding checks to additional crates beyond `orchestrator-cli` and
  `llm-cli-wrapper`.
- Running web-ui Node.js test/build pipelines.
- Manual edits to `.ao` state files.

## Constraints
- Keep checks repository-safe and non-mutating.
- Use Rust-only tooling in this workflow (no desktop-wrapper or UI runtime
  dependencies).
- Keep commands deterministic (`--locked`) and explicit at package granularity.
- Keep CI failure output actionable via step-level command naming.
- Preserve existing workflows and avoid regressions in unrelated CI jobs.

## CI Contract
The new CI workflow must:
- install Rust toolchain on `ubuntu-latest`,
- run the two package-level `cargo check` commands,
- run both `--help` smoke commands and fail on non-zero exit status.

No command in this task should require daemon startup, external services, or
workspace state mutation.

## Acceptance Criteria
- `AC-01`: A new repository-hosted CI workflow exists for Rust workspace
  package checks and CLI help smoke checks.
- `AC-02`: Workflow triggers on `pull_request` and `push`.
- `AC-03`: `cargo check --locked -p orchestrator-cli` is executed in CI.
- `AC-04`: `cargo check --locked -p llm-cli-wrapper` is executed in CI.
- `AC-05`: `cargo run --locked -p orchestrator-cli -- --help` succeeds in CI.
- `AC-06`: `cargo run --locked -p llm-cli-wrapper -- --help` succeeds in CI.
- `AC-07`: Documentation includes discoverable TASK-005 requirements and
  implementation notes references.

## Verification Matrix

| Requirement | Verification method |
| --- | --- |
| `AC-01`, `AC-02` | Workflow file presence and trigger block review |
| `AC-03`, `AC-04` | CI step definitions include both package check commands |
| `AC-05`, `AC-06` | CI step definitions include both help smoke commands |
| `AC-07` | README notes section links TASK-005 docs |

## Deterministic Deliverables for Implementation Phase
- New workflow file under `.github/workflows/` for Rust package CI checks.
- README update referencing TASK-005 task-scoped docs.
- Local command validation mirroring CI commands before phase handoff.
