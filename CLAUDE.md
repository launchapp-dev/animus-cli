# AO CLI - Coding Agent Guide

This file is the current working brief for AI coding agents operating in this repo.
If a statement here conflicts with source, update the docs and follow source.

## Verify Before Repeating

This repo has had stale prose before. Verify against these files before restating
architecture, command counts, routes, or state paths:

- `Cargo.toml`
- `docs/reference/cli/index.md`
- `docs/reference/mcp-tools.md`
- `crates/orchestrator-cli/src/cli_types/root_types.rs`
- `crates/orchestrator-core/src/config.rs`
- `crates/orchestrator-core/src/services.rs`
- `crates/protocol/src/config.rs`
- `crates/protocol/src/repository_scope.rs`
- `crates/orchestrator-web-server/web-ui/package.json`
- `crates/orchestrator-web-server/web-ui/src/app/router.tsx`

## Current Baseline

AO is a Rust-only agent orchestrator with:

- a 16-crate workspace
- a visible CLI surface that includes `project` and `queue`
- hidden `review` and `qa` command trees
- scoped runtime state under `~/.ao/<repo-scope>/`
- project-local workflow YAML overlays under `.ao/workflows.yaml` or `.ao/workflows/*.yaml`
- a React 18 web UI in `crates/orchestrator-web-server/web-ui`

Do not reintroduce stale claims such as:

- 9-crate or 10-crate workspace summaries
- `PROJECT_ROOT` or "last-project-root registry" resolution rules
- removed crates like `llm-mcp-server`
- outdated CLI groups such as a top-level `planning` facade
- React 19, `urql`, or other old web UI stack descriptions

## Workspace Map

Core orchestration:

- `crates/orchestrator-cli`
- `crates/orchestrator-core`
- `crates/orchestrator-config`
- `crates/orchestrator-store`
- `crates/protocol`

Runtime and provider layer:

- `crates/agent-runner`
- `crates/llm-cli-wrapper`
- `crates/oai-runner`
- `crates/workflow-runner-v2`
- `crates/orchestrator-daemon-runtime`
- `crates/orchestrator-providers`
- `crates/orchestrator-git-ops`
- `crates/orchestrator-notifications`

Web surface:

- `crates/orchestrator-web-contracts`
- `crates/orchestrator-web-api`
- `crates/orchestrator-web-server`

## Root Resolution And State

Project root resolution is currently:

1. `--project-root`
2. git common root for the current cwd or linked worktree
3. current working directory

Do not document environment-variable fallbacks unless you add them in code.

State layout is split:

- Project-local `.ao/` stores repo config and workflow YAML overlays.
- Scoped runtime state lives in `~/.ao/<repo-scope>/`.
- Global config lives in `protocol::Config::global_config_dir()` and can be overridden with `AO_CONFIG_DIR`.

Important current paths:

- Project-local config: `.ao/config.json`
- Project-local daemon settings: `.ao/pm-config.json`
- Workflow YAML overlays: `.ao/workflows.yaml`, `.ao/workflows/*.yaml`
- Scoped runtime root: `~/.ao/<repo-scope>/`
- Compiled workflow config: `~/.ao/<repo-scope>/state/workflow-config.v2.json`
- Agent runtime config: `~/.ao/<repo-scope>/state/agent-runtime-config.v2.json`
- State machines: `~/.ao/<repo-scope>/state/state-machines.v1.json`
- Runs: `~/.ao/<repo-scope>/runs/`
- Artifacts: `~/.ao/<repo-scope>/artifacts/`

Legacy readers still probe older repo-local run/artifact paths. Preserve compatibility when needed,
but write new features against the scoped runtime root.

## Working Rules

- Keep the repo Rust-only. Do not add `tauri`, `wry`, `tao`, `gtk`, `webkit*`, `webview*`, or similar desktop-shell dependencies.
- Treat AO JSON state as tool-managed. Use CLI commands instead of hand-editing `.ao/*.json` or scoped state JSON.
- Supported hand-edit exception: workflow YAML overlays in `.ao/workflows.yaml` and `.ao/workflows/*.yaml`.
- In scripts, CI snippets, and automation, pass `--project-root "$(pwd)"`.
- If you change CLI behavior, update `docs/reference/cli/index.md`.
- If you change MCP tools, update `docs/reference/mcp-tools.md` and `docs/guides/agents.md`.
- Prefer narrow verification over full-workspace rebuilds while iterating.

## Implementation Landmarks

CLI and dispatch:

- `crates/orchestrator-cli/src/main.rs`
- `crates/orchestrator-cli/src/cli_types/root_types.rs`
- `crates/orchestrator-cli/src/cli_types/`
- `crates/orchestrator-cli/src/shared/output.rs`

Core services and state:

- `crates/orchestrator-core/src/config.rs`
- `crates/orchestrator-core/src/services.rs`
- `crates/orchestrator-core/src/services/`
- `crates/orchestrator-core/src/workflow/`

Workflow and runtime config:

- `crates/orchestrator-config/src/workflow_config/`
- `crates/orchestrator-config/src/agent_runtime_config.rs`
- `crates/workflow-runner-v2/src/`

Web UI:

- `crates/orchestrator-web-server/web-ui/src/app/router.tsx`
- `crates/orchestrator-web-server/web-ui/src/app/`
- `crates/orchestrator-web-server/web-ui/src/lib/graphql/`

## CLI Reality Check

Visible top-level command groups currently include:

- `daemon`, `agent`, `project`, `queue`, `task`, `workflow`
- `vision`, `requirements`, `architecture`
- `history`, `errors`, `git`, `skill`, `model`, `runner`
- `status`, `output`, `mcp`, `web`, `setup`, `tui`, `doctor`

Hidden but implemented:

- `review`
- `qa`

Use `cargo run -p orchestrator-cli -- --help` or `docs/reference/cli/index.md`
when changing or documenting the command tree.

## Service Model

The main production hub is `FileServiceHub`. Tests commonly use `InMemoryServiceHub`.
If you touch orchestration behavior, look for both implementations and update tests accordingly.

Keep these patterns intact:

- CLI output uses the `ao.cli.v1` envelope for `--json`
- state mutations flow through service APIs, not ad hoc file writes
- workflow YAML overlays compile into generated runtime config under scoped state
- git/worktree behavior is repo-scope aware

## Build And Test Commands

Rust:

```bash
cargo ao-fmt
cargo ao-lint
cargo ao-bin-check
cargo test -p orchestrator-cli
cargo test --workspace
```

Web UI:

```bash
cd crates/orchestrator-web-server/web-ui
npm test
npm run typecheck
npm run build
```

Prefer targeted crate or package tests while iterating. Use workspace-wide checks when the change
crosses crate boundaries or touches shared contracts.

## Web UI Notes

The embedded UI currently uses:

- React 18
- React Router 7
- `@tanstack/react-query`
- `graphql-request`
- Tailwind CSS 4
- `next-themes`
- Base UI and local UI components under `src/components/ui/`

If GraphQL contracts change, verify the Rust schema export path and regenerate client types.

## AO-Managed Workflow

AO is meant to self-host its planning and execution state.

Common flow:

```bash
ao task next
ao task status --id TASK-XXX --status in-progress
ao workflow run --task-id TASK-XXX
ao queue list
ao daemon health
```

If a task is specifically about persistence or migrations, it can justify direct state-file work.
Otherwise, treat AO state as a command surface, not a manual editing target.
