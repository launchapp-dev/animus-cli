# Dependency Policy

AO enforces a strict Rust-only dependency policy. No desktop shell frameworks or native webview dependencies are permitted in the workspace.

## Rust-Only Rule

AO is a CLI-first tool. All functionality must be implemented in pure Rust without relying on platform-specific GUI frameworks.

## Prohibited Packages

The following packages are explicitly prohibited:

### Exact Matches

| Package | Reason |
|---------|--------|
| `tauri` | Desktop shell framework |
| `tauri-build` | Tauri build tooling |
| `wry` | Webview rendering library |
| `tao` | Window management library |
| `gtk` | GTK bindings |
| `gtk4` | GTK4 bindings |
| `webkit2gtk` | WebKit2GTK bindings |
| `webview2` | Windows WebView2 bindings |
| `webview2-com` | WebView2 COM bindings |

### Prefix Matches

| Prefix | Reason |
|--------|--------|
| `tauri-plugin-*` | Any Tauri plugin |

The policy check is case-insensitive and resolves renamed dependencies (via `package = "..."` in Cargo.toml) to catch aliased violations.

## CI Enforcement

The policy is enforced in two ways:

### Integration Test

`crates/orchestrator-cli/tests/rust_only_dependency_policy.rs` scans every workspace member's `Cargo.toml` for prohibited dependencies. This test runs as part of the standard `cargo test --workspace` suite.

The test:
1. Reads the workspace root `Cargo.toml` to enumerate all members
2. Parses each member's `Cargo.toml` including `[dependencies]`, `[dev-dependencies]`, `[build-dependencies]`, and `[target.*.dependencies]` sections
3. Resolves renamed packages (the `package` field in dependency declarations)
4. Checks each resolved package name against the prohibited list
5. Fails with a sorted, deterministic violation report if any prohibited packages are found

### GitHub Actions Workflow

`.github/workflows/rust-only-dependency-policy.yml` runs the policy test in CI on every push and pull request, providing an additional gate beyond local testing.

## Additional Constraints

The dependency policy test also enforces:

- **Workspace axum pin** -- The `axum` dependency must be pinned to `0.8` at the workspace level, and consuming crates must use `workspace = true` rather than declaring their own version
- **llm-cli-wrapper tokio-process ban** -- The `llm-cli-wrapper` crate must not depend on `tokio-process` (process spawning is handled by the agent-runner layer)

## Adding New Dependencies

When adding a new dependency to any crate:

1. Verify it is not on the prohibited list
2. Run `cargo test -p orchestrator-cli -- rust_only_dependency_policy` to confirm
3. Prefer workspace-level dependency declarations in the root `Cargo.toml` for dependencies used by multiple crates
4. Use feature flags for optional integrations (e.g., `jira`, `linear`, `gitlab` on `orchestrator-providers`)
