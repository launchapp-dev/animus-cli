# Development Guide

## Prerequisites

- **Rust** -- install via [rustup](https://rustup.rs/)
- **Cargo** -- comes with Rust; the workspace uses resolver v2
- **Git** -- required for repo root resolution and worktree operations

## Build Commands

```bash
cargo ao-bin-check
cargo ao-bin-build
cargo ao-bin-build-release
```

Run the CLI directly:

```bash
cargo run -p orchestrator-cli -- --help
```

Build a specific crate:

```bash
cargo build -p protocol
cargo build -p orchestrator-core
cargo build -p agent-runner
```

## Workspace Structure

The workspace currently contains 17 crates:

```text
crates/
├── agent-runner/
├── llm-cli-wrapper/
├── oai-runner/
├── orchestrator-cli/
├── orchestrator-config/
├── orchestrator-core/
├── orchestrator-daemon-runtime/
├── orchestrator-git-ops/
├── orchestrator-logging/
├── orchestrator-notifications/
├── orchestrator-providers/
├── orchestrator-store/
├── orchestrator-web-api/
├── orchestrator-web-contracts/
├── orchestrator-web-server/
├── protocol/
└── workflow-runner-v2/
```

`default-members` in `Cargo.toml` include:

- `orchestrator-cli`
- `agent-runner`
- `llm-cli-wrapper`
- `oai-runner`

## Key Dependencies

| Dependency | Usage |
|-----------|-------|
| `anyhow` | Error propagation |
| `clap` | CLI argument parsing |
| `tokio` | Async runtime |
| `serde` / `serde_json` | State and IPC serialization |
| `serde_yaml` | Workflow config parsing |
| `uuid` | IDs for tasks, workflows, and runs |
| `fs2` | File locking for concurrent state access |
| `rusqlite` | Repo-scoped workflow/task/requirement persistence |
| `rmcp` | MCP server and client support |
| `axum` | Web server |
| `croner` | Schedule parsing |

## Documentation Site

The docs are powered by [VitePress](https://vitepress.dev/).

```bash
npm install
npm run docs:dev
npm run docs:build
npm run docs:preview
```

## Project Conventions

- All CLI `--json` output follows the `ao.cli.v1` envelope
- Always use `--project-root "$(pwd)"` in scripts and automation
- Treat `.ao/` project config and `~/.ao/<repo-scope>/` runtime state as AO-managed data
- Prefer source files over prose when documenting command counts, crate counts, and runtime paths
