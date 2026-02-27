# TASK-035 Implementation Notes: FileServiceHub Git Init Opt-In

## Purpose
Convert `FileServiceHub` bootstrap into a split contract:
- default path initializes AO project state only,
- git initialization is available through an explicit opt-in path.

## Non-Negotiable Constraints
- Keep changes scoped to `crates/orchestrator-core` unless a call-site fix in
  another crate is required by compilation/tests.
- Preserve existing `.ao` bootstrap semantics and persistence contracts.
- Avoid direct/manual edits to `/.ao/*.json`.
- Keep behavior deterministic and cross-platform.

## Proposed Change Surface

### Bootstrap Contract Split
- `crates/orchestrator-core/src/services.rs`
  - make base bootstrap function git-neutral.
  - keep/add a dedicated explicit git bootstrap helper (reusing current
    `ensure_project_git_repository` logic).
  - ensure `FileServiceHub::new` only calls the git-neutral base bootstrap.

### Call-Site Review
- `crates/orchestrator-core/src/services/project_impl.rs`
  - preserve `.ao` bootstrap calls for project path safety.
  - do not trigger git bootstrap implicitly through base config bootstrap.
  - if any path truly requires git side effects, invoke explicit helper in that
    path and document why.

### Test Updates
- `crates/orchestrator-core/src/services/tests.rs`
  - update tests that currently expect implicit git bootstrap from project
    create.
  - add explicit regression checks for:
    - no `.git` creation on default bootstrap path,
    - `.ao` base files still created,
    - explicit git bootstrap helper still initializes git when requested.

## Suggested API Shape
Implementation can choose one of these equivalent patterns:
- expose two internal helpers:
  - `bootstrap_project_base_configs(...)` (AO-only),
  - `bootstrap_project_git_repository(...)` (explicit git path), or
- add an options struct to bootstrap with default `git = false`.

Preferred for minimal risk in this codebase:
- keep base bootstrap method name as-is and remove implicit git side effect,
- add a clearly named explicit helper for opt-in git bootstrap.

## Sequencing Plan
1. Refactor `services.rs` so base bootstrap is AO-only.
2. Add explicit git bootstrap entrypoint that wraps existing git init/seed
   logic.
3. Update tests to assert new default behavior and explicit opt-in behavior.
4. Run `cargo test -p orchestrator-core` and fix regressions introduced by the
   change.

## Risks and Mitigations
- Risk: hidden call sites depend on implicit `.git` + `HEAD`.
  - Mitigation: search for assumptions and add explicit helper calls only where
    actually required.
- Risk: behavior drift in project create/load tests.
  - Mitigation: separate AO bootstrap assertions from git bootstrap assertions.
- Risk: explicit helper preserves hardcoded commit identity.
  - Mitigation: keep this behavior only on explicit path for this task; document
    follow-up if identity configurability is needed.

## Validation Targets
- `cargo test -p orchestrator-core services::tests`
- `cargo test -p orchestrator-core`
