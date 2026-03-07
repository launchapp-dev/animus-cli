# AO — Agent Orchestrator CLI

Rust-only CLI for orchestrating AI agent workflows. Manages tasks, requirements, multi-phase workflows, and concurrent agent execution through a unified command-line interface.

## Quick Start

```bash
# Install (build from source)
cargo ao-bin-build-release

# Or run directly
cargo run -p orchestrator-cli -- --help

# Core commands
ao status                    # Project dashboard
ao task next                 # Get next task to work on
ao daemon start              # Start background agent daemon
ao workflow run --task-id TASK-001  # Run workflow for a task
```

## Workspace Layout

10-crate Rust workspace:

```
crates/
├── orchestrator-cli/        # Main `ao` binary (clap CLI, 24 top-level commands)
├── orchestrator-core/       # Domain logic, state management, ServiceHub DI
├── orchestrator-web-api/    # Web API business logic
├── orchestrator-web-server/ # Axum web server + embedded static assets
├── orchestrator-web-contracts/ # Shared web types
├── protocol/                # Wire protocol types shared across all crates
├── agent-runner/            # Standalone daemon managing LLM CLI processes via IPC
├── llm-cli-wrapper/         # Abstraction over AI CLI tools (claude, codex, gemini, etc.)
└── oai-runner/              # OpenAI-compatible streaming API client
```

## Build & Test

```bash
cargo ao-bin-check           # Type-check all runtime binaries
cargo ao-bin-build           # Build all runtime binaries (debug)
cargo ao-bin-build-release   # Build all runtime binaries (release)
cargo test --workspace       # Run all tests
cargo test -p <crate-name>   # Run tests for a specific crate
```

## Build Footprint

Prefer narrow commands while iterating:

```bash
cargo ao-bin-check
cargo test -p orchestrator-cli --test cli_smoke
```

Workspace-wide debug and integration-test builds accumulate large `target/debug`
artifacts. The repo now uses leaner dev/test debuginfo settings to keep future
builds smaller, and includes a cleanup helper:

```bash
scripts/cleanup-build-targets.sh --report
scripts/cleanup-build-targets.sh
scripts/cleanup-build-targets.sh --debug
scripts/cleanup-build-targets.sh --worktrees --days 7
```

`ao git worktree prune` remains the right command for pruning managed git
worktree metadata; the cleanup script above only removes Rust build artifacts.

## Command Overview

See `docs/cli-command-surface.md` for the full command tree with all flags.

| Group | Commands | Purpose |
|---|---|---|
| **Core** | `task`, `workflow`, `daemon`, `agent` | Task management, workflow execution, daemon lifecycle, agent runs |
| **Planning** | `vision`, `requirements`, `execute`, `architecture` | Project vision, requirements drafting, execution planning |
| **Operations** | `runner`, `output`, `errors`, `history` | Runner health, run output, error tracking, execution history |
| **Infrastructure** | `git`, `model`, `skill`, `mcp`, `web` | Git ops, model routing, skill packages, MCP server, web UI |
| **UX** | `status`, `setup`, `doctor`, `tui` | Dashboard, onboarding, diagnostics, terminal UI |
| **Review/QA** | `review`, `qa` | Review decisions, QA gate evaluation |

Global flags: `--json` (machine-readable output), `--project-root <PATH>` (override project root).

## Self-Hosting Workflow

AO is built using AO. Task and requirement tracking is done through `ao` commands:

```bash
ao requirements list                              # View requirements backlog
ao task prioritized                               # View prioritized tasks
ao task next                                      # Get next task to work on
ao task status --id TASK-XXX --status in-progress # Start work
ao task status --id TASK-XXX --status done        # Complete work
```

## Architecture

- **ServiceHub trait** — dependency injection (`FileServiceHub` for production, `InMemoryServiceHub` for tests)
- **JSON envelope** (`ao.cli.v1`) — all `--json` output uses `{ schema, ok, data/error }` contract
- **Exit codes** — 1=internal, 2=invalid_input, 3=not_found, 4=conflict, 5=unavailable
- **Atomic writes** — state persisted via temp file + rename (`write_json_atomic`)
- **Scoped directories** — runtime state at `~/.ao/<repo-scope>/`

## Dependency Policy

Rust-only. Prohibited: `tauri`, `wry`, `tao`, `gtk`, `webkit2gtk`, `webview2` and related desktop shell frameworks.

Enforced by CI: `.github/workflows/rust-only-dependency-policy.yml`

## Release

CI/CD via `.github/workflows/release.yml`:
- **Tag push** (`v*`) — builds + publishes GitHub Release
- **Branch push** (`version/**`) — builds preview artifacts only

Targets: `x86_64-linux`, `x86_64-macos`, `aarch64-macos`, `x86_64-windows`

Archives: `ao-<version>-<target>.tar.gz` (`.zip` on Windows), with `SHA256SUMS.txt`.
