# Development Guide

## Prerequisites

- **Rust** -- install via [rustup](https://rustup.rs/)
- **Cargo** -- comes with Rust; the workspace uses resolver v2
- **Git** -- required for repo root resolution and worktree operations

## Build Commands

```bash
cargo animus-bin-check
cargo animus-bin-build
cargo animus-bin-build-release
```

Run the CLI directly:

```bash
cargo run -p orchestrator-cli -- --help
```

Build a specific crate:

```bash
cargo build -p protocol
cargo build -p orchestrator-core
cargo build -p orchestrator-plugin-host
```

## Workspace Structure

The workspace is a Cargo workspace of 10 crates. The current workspace members are:

```text
crates/
├── animus-plugin-protocol/
├── animus-plugin-runtime/
├── animus-runtime-shared/
├── orchestrator-cli/
├── orchestrator-config/
├── orchestrator-core/
├── orchestrator-daemon-runtime/
├── orchestrator-logging/
├── orchestrator-plugin-host/
└── protocol/
```

Repo-local but not current workspace members:

- `crates/orchestrator-web-server/`

`default-members` in `Cargo.toml` include:

- `orchestrator-cli`

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
| `webbrowser` | Browser-launch helper for `animus web open` |
| `croner` | Schedule parsing |

## Documentation Site

The docs are powered by [VitePress](https://vitepress.dev/).

The web dashboard itself is no longer an in-tree web server. `animus web`
delegates to installed `transport_backend` and `web_ui` plugins.

```bash
npm install
npm run docs:check-sync
cargo test -p orchestrator-cli cli_types::tests::cli_reference_top_level_tree_matches_live_clap_commands -- --exact
cargo test -p orchestrator-cli services::operations::ops_mcp::tests::mcp_reference_table_matches_live_builtin_tools -- --exact
cargo test -p orchestrator-cli services::operations::ops_mcp::tests::mcp_docs_publish_the_live_builtin_tool_count -- --exact
npm run docs:dev
npm run docs:build
npm run docs:preview
npm run docs:deploy
```

Run `npm run docs:check-sync` whenever the CLI command tree or MCP surface
changes. It compares `crates/orchestrator-cli/src/cli_types/root_types.rs` and
`crates/orchestrator-cli/src/services/operations/ops_mcp/` against the
reference docs and fails on drift.

For a full drift check, also run the `orchestrator-cli` tests that assert the
published CLI tree and MCP tool table match the live clap/MCP registries before
deploying the docs site to Vercel.

`npm run docs:deploy` wraps the required preflight in order: sync check,
production site build, then `npx vercel --yes --prod`.

Protocol schema exports live at the repo root:

```bash
cargo run -p animus-plugin-protocol --bin animus-plugin-protocol-export-schema
```

For `animus-subject-protocol` schema exports, work in the upstream
`launchapp-dev/animus-protocol` repository — the in-tree mirror was removed
in v0.5 in favor of the canonical git-pinned crate.

These commands write to `/schemas/animus-plugin-protocol/` under the
workspace root. Do not commit accidental crate-local output such as
`crates/orchestrator-cli/schemas/`.

## Project Conventions

- All CLI `--json` output follows the `animus.cli.v1` envelope
- Always use `--project-root "$(pwd)"` in scripts and automation
- Treat `.animus/` project config and `~/.animus/<repo-scope>/` runtime state as Animus-managed data
- Prefer source files over prose when documenting command counts, crate counts, and runtime paths
