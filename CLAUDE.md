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

Full command tree reference: `docs/reference/cli/index.md`

24 top-level commands, ~130+ subcommands. Key groups:
- **Core workflow**: `task`, `workflow`, `daemon`, `agent`
- **Planning**: `vision`, `requirements`, `execute`, `architecture`
- **Operations**: `runner`, `output`, `errors`, `history`
- **Infrastructure**: `git`, `model`, `skill`, `mcp`, `web`
- **UX**: `status`, `setup`, `doctor`, `tui`
- **Review/QA**: `review`, `qa`

## MCP Tools Quick Reference

AO exposes ~68 MCP tools via `ao mcp serve`. Every tool maps 1:1 to a CLI command (`ao task list` → `ao.task.list`). All tools accept an optional `project_root` parameter.

Full reference: `docs/reference/mcp-tools.md` | Usage guide: `docs/guides/agents.md`

### Tool Groups

| Group | Count | Purpose |
|-------|-------|---------|
| `ao.task.*` | 20 | Create, query, update, assign, pause/resume/cancel tasks; checklists; bulk ops |
| `ao.workflow.*` | 14 | Run/execute/pause/cancel workflows; phases; decisions; checkpoints; config |
| `ao.daemon.*` | 11 | Start/stop/pause/resume daemon; health; config; logs; events; agents |
| `ao.requirements.*` | 6 | Create/get/list/update/delete/refine requirements |
| `ao.queue.*` | 6 | List/stats/enqueue/hold/release/reorder dispatch queue |
| `ao.output.*` | 5 | Run output/tail/monitor/jsonl/artifacts from agent executions |
| `ao.agent.*` | 3 | Run/status/control AI agents |
| `ao.runner.*` | 3 | Health/orphan-detect/restart-stats for runner processes |

### Common MCP Workflows

**Start work on next task:**
1. `ao.task.next` → get highest priority ready task
2. `ao.task.status` → set `in-progress`
3. `ao.workflow.run` → dispatch workflow for the task
4. `ao.output.tail` → monitor agent progress

**Create and execute a task:**
1. `ao.task.create` → create with title, description, priority
2. `ao.workflow.execute` → run synchronously (blocking, no daemon needed)

**Monitor daemon operations:**
1. `ao.daemon.status` → check if running
2. `ao.daemon.health` → detailed metrics
3. `ao.daemon.agents` → active agents
4. `ao.daemon.logs` → search logs for issues

**Batch operations:**
- `ao.task.bulk-status` → update status for many tasks at once
- `ao.task.bulk-update` → update fields for many tasks at once
- `ao.workflow.run-multiple` → dispatch workflows for many tasks

### List Tool Pagination

All list tools support: `limit` (default 25, max 200), `offset`, `max_tokens` (default 3000, max 12000).

### Batch Tool Error Handling

Batch tools accept `on_error`: `"stop"` (default, halt on first failure) or `"continue"` (process all). Max 100 items per call.

## Known Issues to Be Aware Of

- `classify_error` uses string matching on error messages (fragile)
