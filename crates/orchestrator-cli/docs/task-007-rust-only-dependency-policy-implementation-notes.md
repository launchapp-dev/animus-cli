# TASK-007 Implementation Notes: Rust-Only Dependency Policy Checks

## Purpose
Translate TASK-007 requirements into a minimal, deterministic implementation
slice that enforces Rust-only dependency boundaries through automated checks.

## Non-Negotiable Constraints
- Keep enforcement scoped to Rust workspace crate manifests and Rust dependency
  metadata.
- Keep checks non-mutating and deterministic.
- Keep implementation inside existing first-class crates under `crates/`.
- Do not manually edit `.ao` state files.
- Preserve existing release packaging behavior.

## Proposed Change Surface

### Policy checker implementation
- Add a focused policy checker module or test helper in `orchestrator-cli`:
  - `crates/orchestrator-cli/tests/rust_only_dependency_policy.rs`
  - optional helper(s): `crates/orchestrator-cli/tests/support/`
- Read workspace member manifests from root `Cargo.toml`.
- Parse dependency sections and target-specific dependency tables.
- Normalize dependency names and honor renamed dependencies via `package`.
- Compare against denylist and prefix rules from requirements.

### CI integration
- Add a CI workflow dedicated to policy validation:
  - `.github/workflows/rust-only-dependency-policy.yml`
- Run a deterministic command such as:
  - `cargo test -p orchestrator-cli rust_only_dependency_policy`
- Trigger on `pull_request` and `push` for active development branches.

### Documentation updates
- Update task artifact references in `README.md`.
- Keep policy class definitions in the task requirements doc and mirror any
  finalized list in top-level docs if enforcement list changes.

## Suggested Enforcement Algorithm
1. Resolve workspace members from root `[workspace].members`.
2. For each member `Cargo.toml`, collect declared dependency entries from:
   - `dependencies`
   - `dev-dependencies`
   - `build-dependencies`
   - `target.*.(dependencies|dev-dependencies|build-dependencies)`
3. Derive effective package name:
   - use key name by default
   - override with `package` field when present
4. Lowercase and compare against:
   - exact denylist (`tauri`, `tauri-build`, `wry`, `tao`, `gtk`, `gtk4`,
     `webkit2gtk`, `webview2`, `webview2-com`)
   - prefix denylist (`tauri-plugin-`)
5. Emit sorted violations with:
   - manifest path
   - dependency section
   - dependency key
   - resolved package name
6. Fail check on any violation.

## Test Plan
- Positive baseline test: current repository passes policy scan.
- Negative tests:
  - exact-match prohibited dependency
  - prohibited prefix dependency
  - renamed dependency mapping to prohibited package
  - target-specific dependency section handling
- Determinism test:
  - violation output is stable/sorted.

## Risks and Mitigations
- Risk: false negatives on renamed dependencies.
  - Mitigation: always inspect `package` field before matching.
- Risk: false positives from non-Rust assets.
  - Mitigation: scope scanning strictly to workspace crate `Cargo.toml`.
- Risk: policy drift over time.
  - Mitigation: keep denylist centralized in one checker constant and mirrored
    in task policy docs.
