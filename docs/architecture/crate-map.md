# Crate Map

The Animus workspace is a Cargo workspace of 15 crates organized by runtime
responsibility. `Cargo.toml` is the source of truth for membership.

## Foundation

| Crate | Responsibility |
|---|---|
| `protocol` | Shared protocol, config, repository-scope, and CLI JSON envelope types |
| `orchestrator-store` | Atomic persistence helpers and repo-scoped state directory support |
| `orchestrator-logging` | Shared tracing, log path, and runtime log plumbing |

## Runtime

| Crate | Responsibility |
|---|---|
| `orchestrator-daemon-runtime` | Daemon queue, scheduling, subject dispatch, trigger handling, and runtime supervision |
| `animus-runtime-shared` | Shared workflow execution helpers, runtime contracts, agent memory wiring, and runner IPC utilities consumed by daemon code and external `workflow_runner` plugins |
| `agent-runner` | Runner process that launches and supervises provider sessions |

The OpenAI-compatible runner binary moved out-of-tree to
`launchapp-dev/animus-provider-oai-agent` v0.1.3 in the v0.5.2
surface-shrink. The daemon's runtime contract resolver locates the
binary inside the installed plugin (see `crates/orchestrator-core/src/runtime_contract.rs::resolve_oai_runner_binary`).

## CLI and Services

| Crate | Responsibility |
|---|---|
| `orchestrator-cli` | Main `animus` binary, clap surface, MCP server, output formatting, and operations |
| `orchestrator-core` | Domain services, bootstrap, state mutation APIs, plugin registry, and preflight |
| `orchestrator-config` | Workflow YAML loading, pack loading, scaffolding, and phase plan resolution |
| `orchestrator-notifications` | Notification/runtime integration support |
| `orchestrator-providers` | Provider-facing adapter glue and compatibility helpers |

## Plugin Runtime

| Crate | Responsibility |
|---|---|
| `orchestrator-plugin-host` | Plugin discovery, install lockfiles, manifest probes, stdio host, router, signature verification, and the `session` provider-plugin bridge |
| `animus-plugin-protocol` | In-tree copy of the stdio plugin protocol types |
| `animus-plugin-runtime` | Runtime helper crate for plugin implementations |

The workspace also depends on external `launchapp-dev/animus-protocol` crates
for provider/session contracts plus queue/workflow/subject plugin routing,
currently through `animus-provider-protocol`, `animus-session-backend`,
`animus-queue-protocol`, `animus-workflow-runner-protocol`, and
`animus-subject-protocol` in crate-local `Cargo.toml` pins. The root
workspace file currently pins `animus-provider-protocol`,
`animus-session-backend`, and `animus-subject-protocol`.

## Repo-Local Directories Outside The Workspace

| Crate | Responsibility |
|---|---|
| `crates/orchestrator-web-server/` | Legacy in-repo web server directory retained outside the current Cargo workspace |

## Web

The active web stack is not part of the current Cargo workspace. `animus web`
discovers and spawns external plugins, normally installed through:

```bash
animus plugin install-defaults --include-transports
```

The curated transport set is currently `animus-transport-http`,
`animus-transport-graphql`, and `animus-web-ui`. The exact tags live in
`orchestrator-core::plugin_registry`.
