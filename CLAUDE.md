# AO CLI — Project Instructions

## Project Overview

AO (`ao`) is a Rust-only agent orchestrator CLI. 9-crate workspace providing CLI, daemon, agent runner, LLM wrappers, MCP server, and web UI for orchestrating AI agent workflows.

## Workspace Layout

```
crates/
├── orchestrator-cli/        # Main `ao` binary (clap-based CLI)
├── orchestrator-core/       # Domain logic, state management, FileServiceHub
├── orchestrator-web-api/    # Web API business logic (WebApiService)
├── orchestrator-web-server/ # Axum web server + embedded static assets
├── orchestrator-web-contracts/ # Shared web types
├── protocol/                # Wire protocol types shared across all crates
├── agent-runner/            # Standalone daemon managing LLM CLI processes via IPC
├── llm-cli-wrapper/         # Abstraction over AI CLI tools (claude, codex, gemini, etc.)
└── oai-runner/              # OpenAI-compatible streaming API client (MCP server is embedded in orchestrator-cli via rmcp)
```

## Build & Run

```bash
cargo ao-bin-check                    # Check all runtime binaries
cargo ao-bin-build                    # Build all runtime binaries
cargo ao-bin-build-release            # Release build
cargo run -p orchestrator-cli -- --help  # Run ao CLI
cargo test --workspace                # Run all tests
```

## Key Architecture Patterns

- **ServiceHub trait** (`orchestrator-core/src/lib.rs`): dependency injection — `FileServiceHub` for production, `InMemoryServiceHub` for tests
- **JSON envelope** (`ao.cli.v1`): all `--json` output uses `{ schema, ok, data/error }` contract
- **Exit codes**: 1=internal, 2=invalid_input, 3=not_found, 4=conflict, 5=unavailable
- **Atomic writes**: state persisted via temp file + rename in `write_json_atomic`
- **Scoped directories**: runtime state at `~/.ao/<repo-scope>/worktrees/`

## Key Entry Points

- CLI dispatch: `crates/orchestrator-cli/src/main.rs`
- CLI type definitions: `crates/orchestrator-cli/src/cli_types/` (modularized into per-domain files)
- Error classification: `crates/orchestrator-cli/src/shared/output.rs`
- Runner IPC: `crates/orchestrator-cli/src/shared/runner.rs`
- Core state + persistence: `crates/orchestrator-core/src/services.rs`
- Protocol types: `crates/protocol/src/lib.rs`

## Coding Conventions

- Rust 2021 edition, resolver v2
- `anyhow` for error propagation in CLI/application code
- `clap` derive macros for CLI argument parsing
- `tokio` async runtime (full features)
- `serde` + `serde_json` for all serialization
- `chrono` for timestamps (with serde feature)

## Strict Rules

- **Rust-only**: no desktop shell frameworks (tauri, wry, tao, gtk, webkit)
- **`.ao/` is CLI-managed state**: never hand-edit `.ao/*.json` files — use `ao` commands
- **Repo-scoped**: always pass `--project-root "$(pwd)"` in scripts/automation
- **No hardcoded absolute paths** in committed code
- **Security-sensitive patterns**: validate paths stay within project root, sanitize run IDs, prevent path traversal

## Testing

- Unit tests: `#[cfg(test)]` modules throughout crates
- Integration tests: `crates/orchestrator-cli/tests/` (e2e smoke, JSON contracts, workflow state machines, dependency policy)
- CI workflows: `rust-workspace-ci.yml`, `rust-only-dependency-policy.yml`, `web-ui-ci.yml`, `release.yml`
- Run specific crate tests: `cargo test -p <crate-name>`

## Self-Hosting Workflow

AO is built using AO. Task/requirement tracking is done through `ao` commands:

```bash
ao requirements list          # View requirements backlog
ao task prioritized           # View prioritized tasks
ao task next                  # Get next task to work on
ao task status --id TASK-XXX --status in-progress  # Start work
ao task status --id TASK-XXX --status done          # Complete work
```

## CLI Command Surface

Full command tree reference: `docs/cli-command-surface.md`

24 top-level commands, ~130+ subcommands. Key groups:
- **Core workflow**: `task`, `workflow`, `daemon`, `agent`
- **Planning**: `vision`, `requirements`, `execute`, `architecture`
- **Operations**: `runner`, `output`, `errors`, `history`
- **Infrastructure**: `git`, `model`, `skill`, `mcp`, `web`
- **UX**: `status`, `setup`, `doctor`, `tui`, `workflow-monitor`
- **Review/QA**: `review`, `qa`

## Known Issues to Be Aware Of

- `classify_error` uses string matching on error messages (fragile)
