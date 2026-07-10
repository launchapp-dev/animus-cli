# CI/CD Guide

Animus uses GitHub Actions for continuous integration and release automation. This guide covers the CI workflows, build commands, and release process.

## CI Workflows

### Rust Workspace CI (`rust-workspace-ci.yml`)

Runs on every push and pull request. The current jobs are:

- `bash scripts/check-doc-sync.sh` fails fast when `docs/reference/cli/index.md`, `docs/reference/mcp-tools.md`, `docs/guides/agents.md`, and the count-sensitive docs drift from the live Rust surface.
- `cargo fmt --all -- --check` enforces Rust formatting.
- `cargo check --workspace --all-targets` covers every crate (runtime-critical and otherwise) in one cached job — no per-crate matrix to drift against this doc.
- `cargo clippy --workspace --all-targets -- -D warnings` gates every PR on a clean strict lint (the same gate `cargo animus-lint-strict` runs locally).
- `cargo run --locked -p orchestrator-cli --bin animus -- --help` provides a final smoke check after the workspace compiles.
- Concurrency grouping cancels superseded runs on the same branch.

### Rust-Only Dependency Policy (`rust-only-dependency-policy.yml`)

Enforces the project rule that Animus is Rust-only -- no desktop shell frameworks (Tauri, Wry, Tao, GTK, WebKit). This workflow rejects PRs that introduce prohibited dependencies.

### Web UI CI

The web UI now lives in the external `launchapp-dev/animus-web-ui` repository
and ships its own CI. The Animus monorepo no longer builds or tests the
web bundle.

## Build Commands

Animus provides cargo aliases for building the release CLI package:

```bash
cargo animus-bin-check           # Check the animus CLI package compiles
cargo animus-bin-build           # Debug build of the animus CLI package
cargo animus-bin-build-release   # Release (optimized) build of the animus CLI package
```

The shipped binary set is:

| Binary | Crate | Purpose |
|--------|-------|---------|
| `animus` | `orchestrator-cli` | Main CLI |
| `animus-oai-runner` | external `launchapp-dev/animus-provider-oai-agent` plugin | OpenAI-compatible runner; install with `animus plugin install launchapp-dev/animus-provider-oai-agent` |
| `animus-workflow-runner-default` | external `workflow_runner` plugin | Preferred workflow phase execution binary required by daemon preflight |

`animus-runtime-shared` now holds the in-tree shared workflow execution code,
and the workflow phase execution binary is supplied by an external
`workflow_runner` plugin. Legacy `animus-workflow-runner` and
`ao-workflow-runner` names remain fallback resolution targets for older
environments.

The former `agent-runner` sidecar was removed in v0.5.4. Provider sessions now
run through `orchestrator-plugin-host::session` plus installed provider
plugins.

## Testing

Run all tests:

```bash
cargo test --workspace
```

Run tests for a specific crate:

```bash
cargo test -p protocol
cargo test -p orchestrator-cli
cargo test -p orchestrator-core
```

Integration tests live in `crates/orchestrator-cli/tests/` and cover:

- End-to-end smoke tests
- JSON output contract verification
- Workflow state machine transitions
- Dependency policy enforcement

## Release Process (`release.yml`)

Releases are triggered by pushing a tag matching `v*` or a branch matching `version/**`. Manual dispatch is also supported for dry-run validation.

### Release Steps

1. **Rust gates** -- Runs workspace checks and tests for the runtime crates
2. **Cross-platform builds** -- Compiles release binaries for all targets
3. **Packaging** -- Creates archives with binaries and metadata
4. **Publishing** -- Uploads artifacts (for tag pushes, creates a GitHub release)

### Build Targets

| Target | OS | Runner |
|--------|----|--------|
| `x86_64-unknown-linux-gnu` | Linux | `ubuntu-latest` |
| `x86_64-apple-darwin` | macOS (Intel) | `macos-15-intel` |
| `aarch64-apple-darwin` | macOS (Apple Silicon) | `macos-14` |
| `x86_64-pc-windows-msvc` | Windows | `windows-latest` |

### Creating a Release

Tag and push:

```bash
git tag v1.2.3
git push origin v1.2.3
```

The release workflow builds all targets, packages the archives, and creates a GitHub release with the artifacts.

## Local Release Build

Build a release locally:

```bash
cargo animus-bin-build-release
```

Binaries are placed in `target/release/` (or `target/<triple>/release/` for cross-compilation).
