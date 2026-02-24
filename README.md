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
# Build all workspace crates
cargo check

# Main CLI
cargo run -p orchestrator-cli -- --help

# Wrapper CLI
cargo run -p llm-cli-wrapper -- --help

# Runner + MCP server binaries
cargo build -p agent-runner
cargo build -p llm-mcp-server --release
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

## Notes

- Desktop-wrapper GUI code is intentionally excluded from this workspace.
- Web UI planning and implementation notes for the web server live in `crates/orchestrator-web-server/docs/` (current artifact: `task-011-react-shell-requirements.md`).
- CLI destructive safety planning artifacts for TASK-009 live in `crates/orchestrator-cli/docs/`:
  - `task-009-destructive-command-requirements.md`
  - `task-009-destructive-command-implementation-notes.md`
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

Examples:

```bash
# Preview build artifacts only (no GitHub Release)
git checkout -b version/0.2.0
git push origin version/0.2.0

# Full GitHub Release publish
git tag v0.2.0
git push origin v0.2.0
```
