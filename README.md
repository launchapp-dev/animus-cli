# AO CLI Workspace (CLI-only)

Rust-only workspace for the AO orchestrator stack.

- Main CLI crate: `crates/orchestrator-cli`
- Main CLI binary: `ao`
- Supporting runtime crates: `agent-runner`, `llm-cli-wrapper`, `llm-mcp-server`

## Workspace Layout

```text
.
├── Cargo.toml
├── Cargo.lock
├── .cargo/
└── crates/
    ├── orchestrator-cli/
    ├── orchestrator-core/
    ├── orchestrator-web-api/
    ├── orchestrator-web-server/
    ├── orchestrator-web-contracts/
    ├── protocol/
    ├── agent-runner/
    ├── llm-cli-wrapper/
    └── llm-mcp-server/
```

## Quick Start

```bash
# Build/check the AO runtime binary set used in local runs and release artifacts
cargo ao-bin-check
cargo ao-bin-build

# Main CLI
cargo run -p orchestrator-cli -- --help

# Wrapper CLI
cargo run -p llm-cli-wrapper -- --help

# Release build for all runtime binaries
cargo ao-bin-build-release
```

## Self-Hosting Workflow (Using `ao` to Build `ao`)

This repo is configured to use `ao` itself for requirements and task tracking.

### Inspect project and backlog

```bash
cargo run -p orchestrator-cli -- project active
cargo run -p orchestrator-cli -- requirements list
cargo run -p orchestrator-cli -- task prioritized
cargo run -p orchestrator-cli -- task next
```

### Create new requirement and linked task

```bash
cargo run -p orchestrator-cli -- requirements create \
  --title "<requirement title>" \
  --description "<problem and expected outcome>" \
  --priority should

cargo run -p orchestrator-cli -- task create \
  --title "<task title>" \
  --input-json '{
    "title":"<task title>",
    "description":"<implementation scope>",
    "task_type":"feature",
    "priority":"medium",
    "linked_requirements":["REQ-XYZ"]
  }'

cargo run -p orchestrator-cli -- requirements update \
  --id REQ-XYZ \
  --status planned \
  --linked-task-id TASK-XYZ
```

### Model architecture and link tasks

```bash
# Inspect the architecture graph
cargo run -p orchestrator-cli -- architecture get

# Add architecture entities + edges
cargo run -p orchestrator-cli -- architecture entity create \
  --id arch-cli-core \
  --name "CLI Core" \
  --kind crate \
  --code-path crates/orchestrator-cli/src

cargo run -p orchestrator-cli -- architecture entity create \
  --id arch-core-state \
  --name "Core State" \
  --kind crate \
  --code-path crates/orchestrator-core/src/services

cargo run -p orchestrator-cli -- architecture edge create \
  --from arch-cli-core \
  --to arch-core-state \
  --relation reads-writes

# Link a task to architecture entities and query by that linkage
cargo run -p orchestrator-cli -- task update \
  --id TASK-XYZ \
  --replace-linked-architecture-entities \
  --linked-architecture-entity arch-cli-core

cargo run -p orchestrator-cli -- task list \
  --linked-architecture-entity arch-cli-core

# Resolve recommended code paths for a task from linked architecture entities
cargo run -p orchestrator-cli -- architecture suggest --task-id TASK-XYZ
```

### Typical execution loop

Required baseline:

- Every task must be linked to at least one requirement before implementation starts.
- If no requirement exists, create it first and link `TASK-XYZ` in `requirements update --linked-task-id`.

```bash
# Start work
cargo run -p orchestrator-cli -- task status --id TASK-XYZ --status in-progress
cargo run -p orchestrator-cli -- requirements update --id REQ-XYZ --status in-progress

# Finish work
cargo run -p orchestrator-cli -- task status --id TASK-XYZ --status done
cargo run -p orchestrator-cli -- requirements update --id REQ-XYZ --status done
```

## Seeded Backlog

Initial requirements/tasks were created to bootstrap self-hosted development:

- `REQ-001` <-> `TASK-001`: documentation and operating conventions
- `REQ-002` <-> `TASK-002`: CLI UX/help/error quality
- `REQ-003` <-> `TASK-003`: runner lifecycle reliability
- `REQ-004` <-> `TASK-004`: wrapper/MCP integration hardening
- `REQ-005` <-> `TASK-005`: CLI-focused CI coverage
- `REQ-006` <-> `TASK-006`: multi-binary release packaging

## Rust-Only Dependency Policy

- Scope: workspace Rust crate manifests (`Cargo.toml` for members in `[workspace].members`).
- Allowed dependency classes: Rust crates needed for CLI/runtime/server behavior, including test and observability crates that do not embed desktop wrappers.
- Prohibited dependency classes:
  - `tauri`
  - `tauri-build`
  - `tauri-plugin-*`
  - `wry`
  - `tao`
  - `gtk`
  - `gtk4`
  - `webkit2gtk`
  - `webview2`
  - `webview2-com`
- Enforcement: `.github/workflows/rust-only-dependency-policy.yml` runs `cargo test -p orchestrator-cli --test rust_only_dependency_policy` and fails on violations.

## Notes

- Desktop-wrapper GUI code is intentionally excluded from this workspace.
- Web UI planning and implementation notes for the web server live in `crates/orchestrator-web-server/docs/` (current artifacts include `task-011-react-shell-requirements.md`, `task-013-planning-workspace-{requirements,implementation-notes}.md`, `task-014-task-workflow-control-center-{requirements,implementation-notes}.md`, and `task-018-web-gui-ci-e2e-release-gates-*.md`).
- CLI help/error-message planning artifacts for TASK-002 live in `crates/orchestrator-cli/docs/`:
  - `task-002-cli-help-error-requirements.md`
  - `task-002-cli-help-error-implementation-notes.md`
- Rust workspace CI coverage planning artifacts for TASK-005 live in `crates/orchestrator-cli/docs/`:
  - `task-005-rust-workspace-ci-requirements.md`
  - `task-005-rust-workspace-ci-implementation-notes.md`
- Multi-binary release pipeline artifacts for TASK-006 live in `crates/orchestrator-cli/docs/`:
  - `task-006-multi-binary-release-pipeline-requirements.md`
  - `task-006-multi-binary-release-pipeline-implementation-notes.md`
- Rust-only dependency guardrail artifacts for TASK-007 live in `crates/orchestrator-cli/docs/`:
  - `task-007-rust-only-dependency-policy-requirements.md`
  - `task-007-rust-only-dependency-policy-implementation-notes.md`
- CLI destructive safety planning artifacts for TASK-009 live in `crates/orchestrator-cli/docs/`:
  - `task-009-destructive-command-requirements.md`
  - `task-009-destructive-command-implementation-notes.md`
- CLI JSON envelope contract planning artifacts for TASK-008 live in `crates/orchestrator-cli/docs/`:
  - `task-008-json-envelope-contract-requirements.md`
  - `task-008-json-envelope-contract-implementation-notes.md`
- Runner lifecycle regression planning artifacts for TASK-003 live in `crates/orchestrator-core/docs/`:
  - `task-003-runner-lifecycle-regression-requirements.md`
  - `task-003-runner-lifecycle-regression-implementation-notes.md`
- Runner status propagation planning artifacts for TASK-030 live in `crates/orchestrator-cli/docs/`:
  - `task-030-runner-status-requirements.md`
  - `task-030-runner-status-implementation-notes.md`
- Daemon notification connector planning artifacts for TASK-026 live in `crates/orchestrator-cli/docs/`:
  - `task-026-event-notification-connector-framework-requirements.md`
  - `task-026-event-notification-connector-framework-implementation-notes.md`
  - `task-026-notification-operator-guide.md`
- Guided onboarding/config wizard planning artifacts for TASK-022 live in `crates/orchestrator-cli/docs/`:
  - `task-022-guided-onboarding-config-wizard-requirements.md`
  - `task-022-guided-onboarding-config-wizard-implementation-notes.md`
- Runtime tracking files live under `.ao/`.
- Do not hand-edit `.ao/state/*`; use `ao` commands for changes.
- Daemon-managed git worktrees are created under:
  - `~/.ao/<repo-scope>/worktrees/`
  - each repo scope includes a `.project-root` marker and `project-root` symlink (unix).

## GitHub-Only Release Flow

Release CI/CD is configured via `.github/workflows/release.yml`.

- Triggers:
  - tag push matching `v*` (for example `v0.1.0`)
  - branch push matching `version/**` (for preview build artifacts)
- Behavior:
  - always builds release archives for `ao`, `agent-runner`, `llm-cli-wrapper`, `llm-mcp-server`
  - publishes a GitHub Release only for `v*` tags
  - uploads workflow artifacts for both tags and `version/**` branches

Artifact matrix:

| Runner | Target | Archive |
| --- | --- | --- |
| `ubuntu-latest` | `x86_64-unknown-linux-gnu` | `.tar.gz` |
| `macos-15-intel` | `x86_64-apple-darwin` | `.tar.gz` |
| `macos-14` | `aarch64-apple-darwin` | `.tar.gz` |
| `windows-latest` | `x86_64-pc-windows-msvc` | `.zip` |

Artifact naming and traceability contract:
- archive base name: `ao-<version>-<target>`
- tag builds: `<version> = <tag name>` (for example `v0.2.0`)
- preview builds: `<version> = <sanitized-branch>-<sha7>`
- release publish job emits `dist/release-assets/SHA256SUMS.txt` for all archives

Checksum verification example after downloading release assets:

```bash
sha256sum -c SHA256SUMS.txt
```

Examples:

```bash
# Preview build artifacts only (no GitHub Release)
git checkout -b version/0.2.0
git push origin version/0.2.0

# Full GitHub Release publish
git tag v0.2.0
git push origin v0.2.0
```
