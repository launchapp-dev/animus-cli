# Development Guide

## Prerequisites

- **Rust** -- 2021 edition (install via [rustup](https://rustup.rs/))
- **Cargo** -- Comes with Rust; the workspace uses resolver v2
- **Tokio** -- Async runtime (full features); no additional system install needed
- **Git** -- For version control and the git-ops integration

## Build Commands

The workspace defines custom cargo aliases for building all runtime binaries:

```bash
cargo ao-bin-check            # Type-check all runtime binaries
cargo ao-bin-build            # Debug build of all runtime binaries
cargo ao-bin-build-release    # Release (optimized) build
```

To build and run the main CLI directly:

```bash
cargo run -p orchestrator-cli -- --help
```

To build a specific crate:

```bash
cargo build -p protocol
cargo build -p orchestrator-core
cargo build -p agent-runner
```

## Workspace Structure

The workspace contains 16 crates organized under `crates/`:

```
crates/
├── protocol/                    # Wire types, IPC contracts, config schemas
├── orchestrator-store/          # Atomic JSON persistence primitives
├── orchestrator-config/         # Workflow/runtime config parsing
├── orchestrator-core/           # Domain logic, ServiceHub, state machines
├── orchestrator-providers/      # External integrations (Jira, Linear, GitLab)
├── orchestrator-notifications/  # Webhook notifications
├── orchestrator-git-ops/        # Git branch/merge operations
├── orchestrator-daemon-runtime/ # Daemon tick loop, dispatch queue
├── workflow-runner/             # Phase execution binary
├── agent-runner/                # LLM CLI process manager
├── llm-cli-wrapper/             # CLI tool abstraction layer
├── oai-runner/                  # OpenAI API streaming client
├── orchestrator-cli/            # Main `ao` binary
├── orchestrator-web-api/        # Web API business logic
├── orchestrator-web-contracts/  # Shared web types
└── orchestrator-web-server/     # Axum HTTP server
```

The `default-members` in `Cargo.toml` includes `orchestrator-cli`, `agent-runner`, `llm-cli-wrapper`, and `oai-runner` -- these are the four runtime binaries that get built by default.

See the [Crate Map](../architecture/crate-map.md) for detailed descriptions of each crate.

## Key Dependencies

| Dependency | Usage |
|-----------|-------|
| `anyhow` | Error propagation in CLI/application code |
| `clap` | CLI argument parsing via derive macros |
| `tokio` | Async runtime (full features) |
| `serde` / `serde_json` | Serialization for all state and IPC |
| `serde_yaml` | YAML workflow config parsing |
| `chrono` | Timestamps with serde support |
| `uuid` | Unique identifiers for tasks, workflows, runs |
| `fs2` | File locking for concurrent state access |
| `rmcp` | MCP (Model Context Protocol) server/client |
| `axum` | HTTP server for web UI |
| `async-graphql` | GraphQL API (optional feature) |
| `croner` | Cron expression parsing for schedules |

## Documentation Site

The docs are powered by [VitePress](https://vitepress.dev/) with Mermaid diagram support.

```bash
npm install                  # Install docs dependencies
npm run docs:dev             # Start dev server (http://localhost:5173)
npm run docs:build           # Build static site to docs/.vitepress/dist/
npm run docs:preview         # Preview production build locally
```

All documentation lives in `docs/` as plain Markdown. The VitePress config is at `docs/.vitepress/config.mts`. Mermaid diagrams in fenced code blocks render automatically.

## Project Conventions

- All CLI `--json` output follows the `ao.cli.v1` envelope: `{ schema, ok, data/error }`
- Exit codes: 1=internal, 2=invalid_input, 3=not_found, 4=conflict, 5=unavailable
- No hardcoded absolute paths in committed code
- Always use `--project-root "$(pwd)"` in scripts and automation
- The `.ao/` directory is CLI-managed state -- never edit JSON files by hand
