# TASK-007 Requirements: Rust-Only Dependency Guardrail Policy Checks

## Phase
- Workflow phase: `requirements`
- Workflow ID: `d28ddbdf-2b9d-499d-8b24-b8561b31ad7d`
- Task: `TASK-007`

## Objective
Define a deterministic, repository-safe policy check that prevents desktop-wrapper
dependency drift in this Rust-first workspace and fails CI when violations are
introduced.

## Existing Baseline Audit
- Workspace policy already states Rust-only constraints in:
  - `AGENTS.md`
  - `README.md`
- No CI workflow currently enforces dependency boundary policy.
- Existing GitHub Actions file (`.github/workflows/release.yml`) builds and
  publishes binaries only; it does not run policy validation.
- This repository includes non-Rust web UI assets under
  `crates/orchestrator-web-server/web-ui/`; dependency policy checks for this
  task must target Rust crate manifests and Rust dependency metadata only.

## Scope
In scope for implementation after this requirements phase:
- Add an automated Rust dependency policy check that runs in CI.
- Fail CI when prohibited desktop-wrapper crate families are declared in
  workspace crate dependencies.
- Cover direct dependency declarations in:
  - `[dependencies]`
  - `[dev-dependencies]`
  - `[build-dependencies]`
  - target-specific dependency sections (for example
    `[target.'cfg(...)'.dependencies]`).
- Handle renamed dependencies correctly via `package = "crate-name"` metadata.
- Add policy documentation that clearly lists allowed vs prohibited dependency
  classes and the enforcement model.

Out of scope for this task:
- Replacing the release workflow artifact publishing logic.
- Enforcing rules on third-party transitive dependencies outside workspace
  manifest declarations.
- Modifying runtime command behavior unrelated to dependency-policy enforcement.
- Manual edits to `.ao` state files.

## Dependency Policy Contract

### Allowed Dependency Classes
- General Rust crates needed for CLI/runtime/server behavior.
- Testing, linting, observability, serialization, and networking crates that do
  not embed desktop shell frameworks.
- Existing workspace member crates under `crates/`.

### Prohibited Dependency Classes
Desktop-wrapper framework families and closely related shell bindings,
including (non-exhaustive, enforceable baseline list):
- `tauri`
- `tauri-build`
- `tauri-plugin-*` (prefix)
- `wry`
- `tao`
- `gtk`
- `gtk4`
- `webkit2gtk`
- `webview2`
- `webview2-com`

Policy should be implemented using normalized crate names (lowercase) and must
evaluate the resolved package name when dependencies are renamed.

## Constraints
- Keep enforcement deterministic:
  - stable/sorted violation output
  - no nondeterministic ordering in reports
- Keep checks repository-safe and non-mutating.
- Keep implementation Rust-first and compatible with current workspace layout.
- Do not require manual maintenance in generated lock/state artifacts for policy
  checks to run.
- Keep `.ao` mutations command-driven only.

## Acceptance Criteria
- `AC-01`: A repository-hosted automated policy check exists and can be invoked
  by CI without manual steps.
- `AC-02`: CI fails when any workspace crate declares a prohibited dependency
  from the defined desktop-wrapper classes.
- `AC-03`: Renamed dependency declarations are still detected (for example,
  `foo = { package = "tauri", ... }`).
- `AC-04`: Target-specific and build/dev dependency sections are included in the
  scan.
- `AC-05`: Current repository state passes the new policy check.
- `AC-06`: Documentation clearly lists allowed dependency classes, prohibited
  classes, and the intended enforcement boundary (workspace Rust manifests).
- `AC-07`: Violation output is deterministic and actionable (manifest path,
  section, dependency name, remediation hint).

## Verification Matrix

| Requirement | Verification method |
| --- | --- |
| `AC-01` | CI workflow/job executes policy check command on push/PR |
| `AC-02` | Negative fixture or controlled manifest injection test causes deterministic failure |
| `AC-03` | Unit/integration test for renamed dependency mapping (`package` field) |
| `AC-04` | Tests covering standard, build, dev, and target-specific sections |
| `AC-05` | Baseline run against current workspace succeeds in CI |
| `AC-06` | Doc review for explicit allowed/prohibited classes and scope |
| `AC-07` | Assertion test on sorted/stable violation formatting |

## Deterministic Deliverables for Next Phase
- Policy check implementation in Rust (test and/or helper module) committed
  under `crates/orchestrator-cli`.
- CI workflow update to run policy checks.
- Documentation update in `README.md` and/or task-scoped docs with the final
  enforced dependency class list.
- Tests that prove detection coverage and avoid regressions.
