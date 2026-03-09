# CI/CD Guide

AO uses GitHub Actions for continuous integration and release automation. This guide covers the CI workflows, build commands, and release process.

## CI Workflows

### Rust Workspace CI (`rust-workspace-ci.yml`)

Runs on every push and pull request. Checks and tests each crate in the workspace independently:

- `cargo check` for each crate: `protocol`, `orchestrator-cli`, `orchestrator-core`, `agent-runner`, `llm-cli-wrapper`, `orchestrator-web-contracts`, `orchestrator-web-api`, `orchestrator-web-server`
- `cargo test --workspace` for the full test suite
- Concurrency grouping cancels superseded runs on the same branch

### Rust-Only Dependency Policy (`rust-only-dependency-policy.yml`)

Enforces the project rule that AO is Rust-only -- no desktop shell frameworks (Tauri, Wry, Tao, GTK, WebKit). This workflow rejects PRs that introduce prohibited dependencies.

### Web UI CI (`web-ui-ci.yml`)

Runs the web dashboard's npm test suite and build from `crates/orchestrator-web-server/web-ui/`.

### Release Rollback Validation (`release-rollback-validation.yml`)

Validates that release artifacts can be produced correctly and that the release process is reversible.

## Build Commands

AO provides cargo aliases for building the workspace binaries:

```bash
cargo ao-bin-check           # Check all runtime binaries compile
cargo ao-bin-build           # Debug build of all runtime binaries
cargo ao-bin-build-release   # Release (optimized) build
```

The workspace produces three binaries:

| Binary | Crate | Purpose |
|--------|-------|---------|
| `ao` | `orchestrator-cli` | Main CLI |
| `agent-runner` | `agent-runner` | Daemon agent runner |
| `llm-cli-wrapper` | `llm-cli-wrapper` | LLM CLI abstraction |

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

1. **Web UI gates** -- Runs npm tests, builds the web UI bundle, and runs smoke E2E tests
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
cargo ao-bin-build-release
```

Binaries are placed in `target/release/` (or `target/<triple>/release/` for cross-compilation).
