# TASK-005 Implementation Notes: Rust-Only Workspace CI Checks

## Purpose
Translate TASK-005 requirements into a focused CI addition that validates two
core Rust CLI packages with deterministic compile checks and help-command smoke
tests.

## Non-Negotiable Constraints
- Keep implementation scoped to CI workflow and documentation touchpoints.
- Keep commands explicit and package-scoped (`orchestrator-cli`,
  `llm-cli-wrapper`).
- Keep checks deterministic with `--locked`.
- Do not manually edit `.ao` state files.
- Do not alter release packaging semantics.

## Proposed Change Surface

### CI workflow
- Add a new workflow file:
  - `.github/workflows/rust-workspace-ci.yml`
- Workflow should include:
  - triggers: `pull_request`, `push`
  - checkout step
  - Rust toolchain setup (`dtolnay/rust-toolchain@stable`)
  - check steps:
    - `cargo check --locked -p orchestrator-cli`
    - `cargo check --locked -p llm-cli-wrapper`
  - smoke steps:
    - `cargo run --locked -p orchestrator-cli -- --help`
    - `cargo run --locked -p llm-cli-wrapper -- --help`

### Documentation
- Add TASK-005 references in `README.md` under CLI planning artifacts so the
  workflow intent and task outputs remain discoverable.

## Implementation Sequence
1. Create `.github/workflows/rust-workspace-ci.yml` with one Linux job.
2. Add deterministic package check and help smoke command steps.
3. Update `README.md` notes with TASK-005 doc links.
4. Validate locally by running:
   - `cargo check --locked -p orchestrator-cli`
   - `cargo check --locked -p llm-cli-wrapper`
   - `cargo run --locked -p orchestrator-cli -- --help`
   - `cargo run --locked -p llm-cli-wrapper -- --help`

## Risks and Mitigations
- Risk: CI runtime increase from separate `cargo run` smoke commands.
  - Mitigation: keep scope to two packages only and avoid extra test suites.
- Risk: accidental overlap with release-only workflow responsibilities.
  - Mitigation: keep this workflow strictly to check/smoke validation.
- Risk: command drift between docs and workflow steps.
  - Mitigation: keep exact commands duplicated in requirements + implementation
    notes and validate locally.

## Validation Targets for Implementation Phase
- `cargo check --locked -p orchestrator-cli`
- `cargo check --locked -p llm-cli-wrapper`
- `cargo run --locked -p orchestrator-cli -- --help`
- `cargo run --locked -p llm-cli-wrapper -- --help`
