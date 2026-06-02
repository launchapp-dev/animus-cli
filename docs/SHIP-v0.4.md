# Animus v0.4.x — Plugin Ecosystem Shipped

**Historical release log:** v0.4.x plugin rollout

For the current source-backed runtime and plugin architecture, see
[`docs/architecture/runtime-architecture.md`](architecture/runtime-architecture.md)
and [`docs/architecture/plugin-system.md`](architecture/plugin-system.md).

Animus is a Rust-based autonomous agent orchestrator. The v0.4.x series
makes the entire stack pluggable — anyone can ship a subject backend,
an LLM provider, a trigger source, or a log sink as a stdio plugin
under their own GitHub repo and the daemon will dispatch through it.

This document covers the full v0.4.x shipped state: four patch releases
landed in three weeks, turning the plugin ecosystem from "in-flight"
to "the daemon literally cannot run autonomous work without one."

## Release timeline

| Tag | Date | Headline |
|---|---|---|
| [v0.4.0](#v040) | 2026-05-08 | Stdio plugin host foundation; CLI/MCP/daemon dispatch through plugins |
| [v0.4.1](#v041) | 2026-05-15 | `animus plugin install <owner>/<repo>` and `animus plugin new` scaffolding |
| [v0.4.2](#v042) | 2026-05-17 | Full provider ecosystem live; cosign signature verification; `${VAR}` interpolation |
| [v0.4.3](#v043) | 2026-05-21 | Controller-as-plugin migration: daemon Unix-socket control protocol; CLI/MCP/WebAPI try-control-then-local |
| [v0.4.4](#v044) | 2026-05-21 | Legacy command surfaces deleted (`task`, `requirements`, `cloud`, `setup`, `now`, `errors`) |

See [CHANGELOG.md](../CHANGELOG.md) for the complete per-release breakdown.

## Quickstart (60 seconds)

```bash
# Install the CLI (v0.4.4)
curl -fsSL https://raw.githubusercontent.com/launchapp-dev/animus-cli/main/scripts/install.sh | bash
# or download from https://github.com/launchapp-dev/animus-cli/releases/tag/v0.4.4

# Initialize a project
mkdir my-animus && cd my-animus && git init
animus init --template task-queue

# Install a provider plugin from a public repo
animus plugin install launchapp-dev/animus-provider-claude

# Start the daemon (auto-discovers installed plugins + exposes control socket)
animus daemon start --autonomous

# Create your first subject and let the daemon pick it up
animus subject create --kind task --title "test"
animus subject next --kind task

# Tail what the daemon is doing
animus daemon stream --pretty
```

## What's pluggable

Four plugin kinds dispatch through the daemon's stdio plugin host:

| Kind | Wraps | Examples shipped |
|---|---|---|
| Subject backend | A system-of-record (Linear, SQLite, markdown files, requirement trees) | `animus-subject-{linear,sqlite,markdown,requirements}` |
| LLM provider | A CLI tool (Claude, Codex, Gemini, OpenCode) or HTTP API (OpenAI-compatible) | `animus-provider-{claude,codex,gemini,opencode,oai}` |
| Trigger backend | Event sources (Slack, webhooks) | `animus-trigger-{webhook,slack}` |
| Log storage | Persist `events.jsonl` to file, S3, ELK, etc. | `animus-log-storage-file` |

When the daemon starts, it discovers installed plugins, registers their declared
kinds with the subject and provider routing layers, and rejects duplicate
subject-kind claims during router setup. Current daemon preflight requires
installed provider and subject plugins; the former in-tree task and requirement
fallbacks are no longer used for new daemon runs.

## Plugin Ecosystem

The launchapp-dev plugin ecosystem is installed through `animus plugin install`
or the curated default sets in `animus plugin install-defaults`. Keep version
pins in `orchestrator-core::plugin_registry` and the marketplace registry as the
source of truth rather than hard-coding live counts in this historical release
note.

### Protocol + tooling

| Repo | Role |
|---|---|
| [`animus-protocol`](https://github.com/launchapp-dev/animus-protocol) | Protocol crates for stdio plugins, subject backends, provider sessions, and runtime helpers |
| [`animus-plugin-template`](https://github.com/launchapp-dev/animus-plugin-template) | Subject + provider scaffolds consumed by `animus plugin new` |
| [`animus-plugin-registry`](https://github.com/launchapp-dev/animus-plugin-registry) | Marketplace index (browse + search) |

### Subject backends

| Repo | Install |
|---|---|
| [`animus-subject-linear`](https://github.com/launchapp-dev/animus-subject-linear) | `animus plugin install launchapp-dev/animus-subject-linear` |
| [`animus-subject-sqlite`](https://github.com/launchapp-dev/animus-subject-sqlite) | `animus plugin install launchapp-dev/animus-subject-sqlite` |
| [`animus-subject-markdown`](https://github.com/launchapp-dev/animus-subject-markdown) | `animus plugin install launchapp-dev/animus-subject-markdown` |
| [`animus-subject-requirements`](https://github.com/launchapp-dev/animus-subject-requirements) | `animus plugin install launchapp-dev/animus-subject-requirements` |

### Providers

| Repo | Install |
|---|---|
| [`animus-provider-claude`](https://github.com/launchapp-dev/animus-provider-claude) | `animus plugin install launchapp-dev/animus-provider-claude` |
| [`animus-provider-codex`](https://github.com/launchapp-dev/animus-provider-codex) | `animus plugin install launchapp-dev/animus-provider-codex` |
| [`animus-provider-gemini`](https://github.com/launchapp-dev/animus-provider-gemini) | `animus plugin install launchapp-dev/animus-provider-gemini` |
| [`animus-provider-opencode`](https://github.com/launchapp-dev/animus-provider-opencode) | `animus plugin install launchapp-dev/animus-provider-opencode` |
| [`animus-provider-oai`](https://github.com/launchapp-dev/animus-provider-oai) | `animus plugin install launchapp-dev/animus-provider-oai` |

### Triggers

| Repo | Install |
|---|---|
| [`animus-trigger-webhook`](https://github.com/launchapp-dev/animus-trigger-webhook) | `animus plugin install launchapp-dev/animus-trigger-webhook` |
| [`animus-trigger-slack`](https://github.com/launchapp-dev/animus-trigger-slack) | `animus plugin install launchapp-dev/animus-trigger-slack` |

### Log storage

| Repo | Install |
|---|---|
| [`animus-log-storage-file`](https://github.com/launchapp-dev/animus-log-storage-file) | `animus plugin install launchapp-dev/animus-log-storage-file` |

## Per-release detail

### v0.4.0

Stdio plugin host foundation. The daemon learned to spawn plugin
subprocesses, speak JSON-RPC over stdin/stdout (canonical NDJSON framing), and dispatch
phase-execution through them. Subject backends and providers became
the first plugin kinds. Eight repos went live in the
`launchapp-dev/animus-*` namespace.

### v0.4.1

Public-repo plugin install. `animus plugin install <owner>/<repo>`
downloads, verifies, and registers a plugin in one command. `animus
plugin new` scaffolds a fresh plugin repo from the template.

### v0.4.2

Provider ecosystem reached parity with the daemon's needs:
`claude`, `codex`, `gemini`, `opencode`, `oai` all shipped v0.1.0.
Subject backends grew to four (linear, sqlite, markdown,
requirements). Cosign sigstore verification on install (verify-if-
present by default; `--require-signature` to enforce). `${VAR}`,
`${VAR:-default}`, `${VAR:?error}` interpolation in workflow YAML.

### v0.4.3

Controller-as-plugin migration. The daemon now exposes a Unix-socket
control protocol at `~/.animus/<repo-scope>/control.sock` speaking
JSON-RPC 2.0 per [`animus-control-protocol` v0.1.3](https://github.com/launchapp-dev/animus-protocol/tree/v0.1.3/animus-control-protocol).
47 method constants across 7 groups (subject, plugin, daemon,
workflow, agent, queue, project). The CLI, MCP server, and Web API
all try the control socket first and fall back to direct in-process
calls when the daemon isn't running. Six new `animus.subject.*` MCP
tools mirror the unified CLI. `animus logs tail` replaces ad-hoc
`events.jsonl` tailing.

### v0.4.4

Cleanup. With `animus subject --kind <kind>` proven across all
three call paths (CLI, MCP, WebAPI), the legacy command trees were
deleted in a single commit:

- `animus task` → `animus subject --kind task`
- `animus requirements` → `animus subject --kind requirement`
- `animus cloud` → retired
- `animus setup` → `animus init` (the supported onboarding entry point)
- `animus now` → folded into `animus status`
- `animus errors` → folded into `animus history`

The underlying `orchestrator-core` services (`task*`, `requirements*`)
are preserved — both the new SubjectBackend adapters and the daemon's
workflow runtime continue to depend on them. The matching MCP tool
families (`ao_task_*`, `ao_requirements_*`) are deleted in favor of
`animus.subject.*` with `kind=task` or `kind=requirement`.

## Building your own plugin

```bash
animus plugin new --kind subject --name my-jira
cd animus-subject-my-jira
# implement src/backend.rs against the SubjectBackend trait
cargo build --release
animus plugin install --path target/release/animus-subject-my-jira
```

The protocol crates ship from `launchapp-dev/animus-protocol`:

- `animus-plugin-protocol` — wire types (latest: v0.1.3)
- `animus-subject-protocol` — `SubjectBackend` trait
- `animus-provider-protocol` — `ProviderBackend` trait
- `animus-plugin-runtime` — the stdio JSON-RPC loop
- `animus-session-backend` — CLI subprocess management
- `animus-control-protocol` — daemon Unix-socket method constants

Plugin authors write `subject_backend_main(info, backend).await` from
`main()` and the runtime handles the wire protocol. Sign your
releases with the GitHub Actions OIDC keyless cosign workflow and
your plugin installs with `signature_status: verified`.

## What's coming in v0.5

- Cross-language plugins: Python + TypeScript SDKs against the protocol
- In-process `sigstore-rs` (drop the `cosign` CLI shell-out)
- TTL eviction for the session-keyed plugin host cache
- AgentPool integration (currently `allow(dead_code)`)
- `workflows_list`, `queue_reorder` web-api migration to control protocol

## Links

- Animus CLI: <https://github.com/launchapp-dev/animus-cli>
- Plugin protocol: <https://github.com/launchapp-dev/animus-protocol>
- Plugin template: <https://github.com/launchapp-dev/animus-plugin-template>
- Plugin registry: <https://github.com/launchapp-dev/animus-plugin-registry>
- Plugin signing: [docs/architecture/plugin-signing.md](architecture/plugin-signing.md)
- Naming contract: [docs/architecture/naming-contract.md](architecture/naming-contract.md)
- Subject-backend plugins: [docs/architecture/subject-backend-plugins.md](architecture/subject-backend-plugins.md)

## For agents helping a user set up Animus

If a user asks you to set up Animus for them, follow this exact sequence:

1. Verify the user is in a git repo (`git rev-parse --show-toplevel`). If not, ask whether to `git init` here or elsewhere.
2. Install the animus CLI if missing. Check `which animus` and download the v0.4.4 release for the user's platform if it's not on PATH.
3. Run `animus init --template task-queue` to scaffold the project. (Do not use `animus setup` — it was deleted in v0.4.4.)
4. Ask the user which LLM provider they want. Common choices:
   - Claude → `animus plugin install launchapp-dev/animus-provider-claude`
   - Codex → `animus plugin install launchapp-dev/animus-provider-codex`
   - OpenAI HTTP → `animus plugin install launchapp-dev/animus-provider-oai` (set `OPENAI_API_KEY`)
   - Gemini → `animus plugin install launchapp-dev/animus-provider-gemini`
   - OpenCode → `animus plugin install launchapp-dev/animus-provider-opencode`
5. Verify the install: `animus plugin list` — confirm the plugin appears with its `SIG` column.
6. Set up secrets the provider needs (env vars or `.env` file — DON'T commit secrets).
7. Start the daemon: `animus daemon start --autonomous`.
8. Show the user how to create a subject and let the daemon pick it up:
   ```bash
   animus subject create --kind task --title "my-first-task"
   animus subject next --kind task
   animus daemon stream --pretty
   ```

Do not use `animus task ...` or `animus requirements ...` — both were
deleted in v0.4.4. Use `animus subject --kind <kind>` instead. Do not
silently install multiple plugins — ask the user first.
