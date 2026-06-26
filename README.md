<div align="center">

![header](https://capsule-render.vercel.app/api?type=waving&color=0:0d1117,50:161b22,100:1f6feb&height=200&section=header&text=Animus&fontSize=90&fontColor=f0f6fc&animation=fadeIn&fontAlignY=35&desc=Ship%20every%20idea%20you%20have&descAlignY=55&descSize=22&descColor=8b949e)

<br/>

### One founder. Sixteen projects. Eighty agents in parallel.

<sub>Animus runs the AI engineering team behind a portfolio of products.</sub>

<br/>
<br/>

<a href="https://github.com/launchapp-dev/animus-cli/releases/latest"><img src="https://img.shields.io/github/v/release/launchapp-dev/animus-cli?style=for-the-badge&color=1f6feb&labelColor=0d1117&logo=github&logoColor=f0f6fc" alt="Latest release" /></a>
&nbsp;
<img src="https://img.shields.io/badge/rust-100%25-f0f6fc?style=for-the-badge&labelColor=0d1117&logo=rust&logoColor=f0f6fc" alt="Rust" />
&nbsp;
<img src="https://img.shields.io/badge/macOS%20%7C%20Linux%20%7C%20Windows-f0f6fc?style=for-the-badge&labelColor=0d1117&logo=apple&logoColor=f0f6fc" alt="Platforms" />
&nbsp;
<a href="https://github.com/launchapp-dev/awesome-ai-coding-tools"><img src="https://awesome.re/mentioned-badge-flat.svg" alt="Mentioned in Awesome AI Coding Tools" /></a>

</div>

<!-- TODO: add hero screenshot of the daemon queue at peak (80 agents across 16 projects) -->
<!-- ![hero](docs/images/hero-queue-80-agents.png) -->

> "I was spending so many hours chasing ideas — I wanted to build SaaS templates,
> an auth platform, an ecommerce CRM, every direction I could think of. Animus
> is what made that possible. At one point I had 16 projects with 80 agents
> running in parallel doing real work. This is what I always wished existed."
>
> — Sami, building [LaunchApp](https://launchapp.dev)

<br/>

## Install — 30 seconds

### One paste, any agent

Open a fresh **Claude Code** (or **Codex** / **OpenCode** / **Cursor**) session and paste this. The agent installs the Animus CLI, clones `animus-skills`, runs the setup script, and adds the project section to `CLAUDE.md` / `AGENTS.md`. You'll be running workflows in about a minute.

> Install Animus + Animus Skills: run **`curl -fsSL https://raw.githubusercontent.com/launchapp-dev/animus-cli/main/scripts/install.sh | bash`** to install the `animus` CLI (currently `v0.6.15` in this repo), then **`animus plugin install-defaults`** to pull in the provider + subject + workflow_runner + queue plugins the daemon needs (one-time setup, idempotent; add `--include-recommended` for the web UI and extra providers). Then **`git clone --single-branch --depth 1 https://github.com/launchapp-dev/animus-skills.git ~/.claude/skills/animus-skills && cd ~/.claude/skills/animus-skills && ./setup`** to link the skills and write `.mcp.json`. Add an "Animus" section to CLAUDE.md (or AGENTS.md for Codex) listing the slash commands: `/animus-setup`, `/animus-getting-started`, `/animus-mcp-setup`, `/animus-workflow-authoring`, `/animus-pack-authoring`, `/animus-skill-authoring`, `/animus-troubleshooting`. Restart the agent so the new `animus` MCP server is picked up. From a project root, run `animus init --walkthrough` to scaffold `.animus/`, install packs, and optionally start the daemon.

For Codex CLI, swap the clone path to `~/.codex/skills/animus-skills` and edit `AGENTS.md` instead of `CLAUDE.md`.

### Manual install (no agent)

```bash
curl -fsSL https://raw.githubusercontent.com/launchapp-dev/animus-cli/main/scripts/install.sh | bash
animus plugin install-defaults
```

The upstream installer currently targets macOS. On Linux and Windows, use a release archive or build from source.

The second command is **required in v0.4.12 and later** (and expanded in **v0.5** to include `workflow_runner` and `queue` plugins) — the daemon no longer ships with bundled providers, subject backends, workflow runners, or queue implementations. It will refuse to start until at least one of each required role is installed. The command is idempotent and skips anything already installed. Pass `--include-recommended` to also get the web UI, GraphQL transport, and additional providers.

### Install via avm (version manager, recommended for multi-project machines)

[`avm`](https://github.com/launchapp-dev/avm) pins each project to a specific `animus` kernel version (à la `nvm`/`rustup`) and dispatches transparently: a project's `.animus-version` selects the kernel, with a global default fallback. Running `animus` in any project then uses the right version automatically.

```bash
curl -fsSL https://raw.githubusercontent.com/launchapp-dev/avm/main/install.sh | sh
avm install v0.6.15            # download a kernel version
avm use --global v0.6.15       # machine default; per-project: `avm use v0.6.15` writes .animus-version
```

### Project setup: `animus.toml` (v0.6.15+)

Animus uses an npm/cargo-style manifest. A committed `animus.toml` declares the project's kernel, plugins, and packs; `animus install` resolves it into `.animus/plugins.lock` and installs the set, so onboarding a checked-out project is two commands:

```bash
git clone <repo> && cd <repo>
cp .env.example .env && $EDITOR .env   # fill in the project's declared secrets
animus install                          # plugins + packs from animus.lock, then load .env into the secret store
# CI / containers: reproduce the lock exactly
animus install --locked
```

`animus init` scaffolds `animus.toml`, `.env.example`, and a merge-safe `.gitignore`; `animus add <spec>` / `animus remove <name>` manage dependencies. See [`docs/reference/cli/index.md`](docs/reference/cli/index.md#project-manifest-animustoml-install--add--remove).

<details>
<summary><kbd>options</kbd></summary>

```bash
# Specific version
ANIMUS_VERSION=v0.6.15 curl -fsSL https://raw.githubusercontent.com/launchapp-dev/animus-cli/main/scripts/install.sh | bash

# Custom directory
ANIMUS_INSTALL_DIR=/usr/local/bin curl -fsSL https://raw.githubusercontent.com/launchapp-dev/animus-cli/main/scripts/install.sh | bash

# Run install-defaults automatically as the last step
ANIMUS_INSTALL_PLUGINS=1 curl -fsSL https://raw.githubusercontent.com/launchapp-dev/animus-cli/main/scripts/install.sh | bash

# Skip the post-install plugin step (CI / Docker)
ANIMUS_SKIP_PLUGIN_INSTALL=1 curl -fsSL https://raw.githubusercontent.com/launchapp-dev/animus-cli/main/scripts/install.sh | bash
```

</details>

<details>
<summary><kbd>upgrading from v0.4.11 or earlier</kbd></summary>

Stop the running daemon first, then upgrade. See [`docs/migration/v0.4.11-to-v0.4.12.md`](docs/migration/v0.4.11-to-v0.4.12.md) for the full rationale and rollback procedure.

```bash
animus daemon stop
curl -fsSL https://raw.githubusercontent.com/launchapp-dev/animus-cli/main/scripts/install.sh | bash
animus plugin install-defaults --include-subjects --include-transports
animus daemon preflight                 # verify all required plugins present
animus daemon start
```

</details>

<details>
<summary><kbd>prerequisites</kbd></summary>

You need at least one AI coding CLI:

```bash
npm install -g @anthropic-ai/claude-code    # Claude (recommended)
npm install -g @openai/codex                # Codex
npm install -g @google/gemini-cli           # Gemini
```

</details>

---

## What is Animus?

**Animus is how one founder ships a portfolio of products.**

Define your agent team in YAML. Animus runs them in parallel git worktrees,
gates their work through quality checks, and opens reviewed PRs — while you
focus on the next idea.

Use Claude, Codex, Gemini, OpenCode, Ollama, or any coding agent you already use.
Animus orchestrates — it doesn't replace your tools.

- **Local-first.** Your code never leaves your machine.
- **Plugin-first.** Works with Linear, GitHub Issues, Asana, Jira, or whatever tracker you already use.

The core daemon is the orchestration runtime; providers, subject backends, triggers, transports, web UI, and log storage ship as independent `animus-*` plugins under [launchapp-dev](https://github.com/launchapp-dev). `animus plugin install <owner/repo>` pulls them in with optional cosign signature verification. The daemon discovers installed plugins at startup, exposes a Unix-socket control protocol, and the CLI, MCP server, and web transports route through that control surface.

```
                ┌──────────────────────────────────────────────────┐
                │            Animus Daemon (Rust)                  │
                │                                                  │
  ┌────────┐    │    ┌───────────┐    ┌───────────┐    ┌────────┐ │    ┌────────┐
  │ Tasks  │───▶│───▶│  Dispatch │───▶│  Agents   │───▶│ Phases │─│──▶│  PRs   │
  │        │    │    │  Queue    │    │           │    │        │ │    │        │
  │ TASK-1 │    │    │ priority  │    │ Claude    │    │ impl   │ │    │ PR #42 │
  │ TASK-2 │    │    │ routing   │    │ Codex     │    │ review │ │    │ PR #43 │
  │ TASK-3 │    │    │ capacity  │    │ Gemini    │    │ test   │ │    │ PR #44 │
  └────────┘    │    └───────────┘    └───────────┘    └────────┘ │    └────────┘
                │                                                  │
                │    Schedules: work-planner (5m), pr-reviewer     │
                │    (5m), reconciler (5m), PO scans (2-8h)        │
                └──────────────────────────────────────────────────┘
```

---

## Who is this for?

**Today: portfolio builders.**
- **Solo founders** running 3+ projects in parallel
- **Indie hackers** shipping every idea they have
- **Two-founder studios** trying to ship like a 20-person team

**Increasingly: teams drowning in code maintenance.**
- Codebases with stale deps, missing tests, and doc drift
- Microservices nobody has time to maintain
- Legacy systems waiting for "the modernization project"

**Not for:**
- Anyone looking for AI to "replace" their engineers
- Enterprise looking for a managed coding agent (the Devin / Codespaces use case)
- Teams already happy doing everything inside Cursor or Claude Code

The same capabilities — parallel worktrees, supervised agents, quality gates, automated PRs — serve both audiences. We started with portfolio builders because that's the user we know best: we are one.

---

## Quick Start

```bash
cd your-project                                  # any git repo
animus init --walkthrough                        # guided setup: plugins, packs, starter workflow, daemon
```

The walkthrough installs required plugins, installs recommended workflow packs,
copies a starter workflow into `.animus/workflows/`, and optionally starts the
daemon. It is the fastest path from zero to running workflows. Scripted / CI
alternative:

```bash
animus doctor                                    # check prerequisites and auto-remediate
animus plugin install-defaults                   # one command: provider + subjects + workflow_runner + queue
animus init --template task-queue --non-interactive --install-packs
animus daemon preflight                          # verify all required plugins are present

# Option 1: run a workflow on demand
animus subject create --kind task --title "Add rate limiting" --priority p1
animus workflow run --task-id TASK-001

# Option 2: go fully autonomous
animus daemon start                              # daemon executes ready subjects continuously
animus daemon health                             # verify it's up
animus logs tail --limit 100                     # inspect recent daemon events
animus daemon stream                             # live structured event stream

# Scaffold a brand-new subject backend (Jira, Notion, anything with an API):
animus plugin new --kind subject --name jira
```

> **v0.5 note:** the daemon will refuse to start unless plugins for all
> required roles are installed — provider, subject backend, `workflow_runner`,
> and `queue`. Run `animus daemon preflight` for the exact remediation
> command if startup fails. See
> [docs/migration/v0.4.11-to-v0.4.12.md](docs/migration/v0.4.11-to-v0.4.12.md)
> for the upgrade story from v0.4.11; v0.5 follows the same install-defaults
> remediation pattern.

Bundled `init` templates: **`task-queue`**, **`conductor`**, **`direct-workflow`**.

> **v0.4.4 note:** `animus task ...` and `animus requirements ...` were removed
> in favor of `animus subject --kind <kind>`. Install the task and requirement
> subject plugins, then route through `subject --kind task` or
> `subject --kind requirement`.

---

## Everything in One YAML

<table>
<tr>
<td width="50%">

### Agents

Bind models, tools, MCP servers, and system prompts to named profiles. Route by task complexity.

```yaml
agents:
  default:
    model: claude-sonnet-4-6
    tool: claude
    mcp_servers: ["animus", "context7"]

  work-planner:
    system_prompt: |
      Scan tasks, check dependencies,
      enqueue ready work for the daemon.
    model: claude-sonnet-4-6
    tool: claude
```

</td>
<td width="50%">

### Phases

Reusable execution units. Three modes: **agent** (AI with decision contracts), **command** (shell), **manual** (human gate).

```yaml
phases:
  implementation:
    mode: agent
    agent: default
    directive: "Implement production code."
    decision_contract:
      min_confidence: 0.7
      max_risk: medium

  push-branch:
    mode: command
    command:
      program: git
      args: ["push", "-u", "origin", "HEAD"]
```

</td>
</tr>
<tr>
<td width="50%">

### Workflows

Compose phases into pipelines with skip conditions and post-success hooks.

```yaml
workflows:
  - id: standard
    phases:
      - requirements
      - implementation
      - push-branch
      - create-pr
    post_success:
      merge:
        strategy: squash
        auto_merge: true
        cleanup_worktree: true
```

</td>
<td width="50%">

### Schedules & Triggers

Cron-based autonomous execution and event-driven triggers. Trigger types: `file_watcher`, `webhook` (generic HTTP), `github_webhook` (with event filtering).

```yaml
schedules:
  - id: work-planner
    cron: "*/5 * * * *"
    workflow_ref: work-planner
    enabled: true

triggers:
  - id: pr-opened
    type: github_webhook
    workflow_ref: pr-reviewer
    enabled: true
    config:
      events: ["pull_request"]
```

</td>
</tr>
</table>

---

## The Full Agent Team

Animus doesn't run one agent. It runs an **entire product organization**:

```
  ┌─────────────────────────────────────────────────────────────────┐
  │                                                                 │
  │   Planners               Builders              Reviewers        │
  │   ╭──────────────╮       ╭──────────────╮       ╭──────────────╮│
  │   │ Work Planner │       │ Claude Eng   │       │ PR Reviewer  ││
  │   │ Reconciler   │       │ Codex Eng    │       │ PO Reviewer  ││
  │   │ Triager      │       │ Gemini Eng   │       │ Code Review  ││
  │   │ Req Refiner  │       │ GLM Eng      │       │              ││
  │   ╰──────────────╯       ╰──────────────╯       ╰──────────────╯│
  │                                                                 │
  │   Product Owners         Architects             Operations      │
  │   ╭──────────────╮       ╭──────────────╮       ╭──────────────╮│
  │   │ PO: Web      │       │ Rust Arch    │       │ Sys Monitor  ││
  │   │ PO: MCP      │       │ Infra Arch   │       │ Release Mgr  ││
  │   │ PO: Workflow │       │              │       │ Branch Sync  ││
  │   │ PO: CLI      │       │              │       │ Doc Drift    ││
  │   │ PO: Runner   │       │              │       │ Wf Optimizer ││
  │   ╰──────────────╯       ╰──────────────╯       ╰──────────────╯│
  │                                                                 │
  └─────────────────────────────────────────────────────────────────┘
```

## Key Concepts

<table>
<tr>
<td width="33%">

**Decision Contracts**

Every agent phase returns a typed verdict: `advance`, `rework`, `skip`, or `fail`. Rework loops pass the reviewer's feedback back to the implementer. Configurable `max_rework_attempts` prevents infinite loops.

</td>
<td width="33%">

**Model Routing**

Route tasks to different models by type and complexity. Low-priority bugfixes go to cheap models. Critical architecture tasks go to Opus. The work-planner agent manages this automatically.

</td>
<td width="33%">

**Worktree Isolation**

Every task gets its own git worktree. Agents work in parallel on separate branches without conflicts. Post-success hooks handle merge, cleanup, and PR creation.

</td>
</tr>
</table>

| Complexity | Type | Model | Why |
|:---|:---|:---|:---|
| `low` | bugfix/chore | GLM-5-Turbo | Cheapest option |
| `medium` | feature | Claude Sonnet | Reliable, fast |
| `medium` | UI/UX | Gemini 3.1 Pro | Vision + design expertise |
| `high` | refactor | Codex GPT-5.3 | Strong code understanding |
| `high` | architecture | Claude Opus | Maximum quality |
| `critical` | any | Claude Opus | No compromises |

---

## Plugin Ecosystem

The plugin ecosystem lives in standalone GitHub repositories under
[launchapp-dev](https://github.com/launchapp-dev). The exact `(repo, tag)` set
installed by `animus plugin install-defaults` lives in a single source of truth at
[`crates/orchestrator-core/src/plugin_registry.rs`](crates/orchestrator-core/src/plugin_registry.rs)
so the CLI installer and the daemon preflight always agree on which tag is
"the default":

| Kind | Repos |
|---|---|
| **Protocol + tooling** | [`animus-protocol`](https://github.com/launchapp-dev/animus-protocol), [`animus-plugin-template`](https://github.com/launchapp-dev/animus-plugin-template), [`animus-plugin-registry`](https://github.com/launchapp-dev/animus-plugin-registry) |
| **Subject backends** | `animus-subject-default`, [`animus-subject-requirements`](https://github.com/launchapp-dev/animus-subject-requirements), [`animus-subject-linear`](https://github.com/launchapp-dev/animus-subject-linear), [`animus-subject-sqlite`](https://github.com/launchapp-dev/animus-subject-sqlite), [`animus-subject-markdown`](https://github.com/launchapp-dev/animus-subject-markdown) |
| **Providers** | [`animus-provider-claude`](https://github.com/launchapp-dev/animus-provider-claude), [`animus-provider-codex`](https://github.com/launchapp-dev/animus-provider-codex), [`animus-provider-gemini`](https://github.com/launchapp-dev/animus-provider-gemini), [`animus-provider-opencode`](https://github.com/launchapp-dev/animus-provider-opencode), [`animus-provider-oai`](https://github.com/launchapp-dev/animus-provider-oai) |
| **Triggers** | [`animus-trigger-webhook`](https://github.com/launchapp-dev/animus-trigger-webhook), [`animus-trigger-slack`](https://github.com/launchapp-dev/animus-trigger-slack) |
| **Transports + web UI** | `animus-transport-http`, `animus-transport-graphql`, `animus-web-ui` |
| **Log storage** | [`animus-log-storage-file`](https://github.com/launchapp-dev/animus-log-storage-file) |

```bash
animus plugin install launchapp-dev/animus-provider-claude
animus plugin list                     # see what's installed with SIG column
animus plugin install-defaults --include-subjects --include-transports
animus plugin new my-thing --kind subject   # scaffold from the template
```

Installs verify a sigstore cosign signature when one is published. Use
`--require-signature` to enforce. See
[docs/architecture/plugin-signing.md](docs/architecture/plugin-signing.md).

---

## Claude Code Integration

[**Animus Skills**](https://github.com/launchapp-dev/animus-skills) is the companion skill bundle. Install with the one-paste prompt above, or directly:

```bash
git clone https://github.com/launchapp-dev/animus-skills.git ~/animus-skills
cd ~/animus-skills && ./setup           # auto-detects installed agent hosts
```

The `./setup` script supports `--host claude|codex|opencode|cursor|slate|kiro|all`, `--no-cli` (skip animus install), and `--no-mcp` (skip writing project `.mcp.json`).

<table>
<tr>
<td width="50%">

**Slash Commands**

| Command | What it does |
|:---|:---|
| `/animus-setup` | Initialize Animus in your project |
| `/animus-getting-started` | Install, concepts, first task |
| `/animus-mcp-setup` | Connect AI tools via MCP |
| `/animus-workflow-authoring` | Write custom YAML workflows |
| `/animus-pack-authoring` | Build workflow packs |
| `/animus-skill-authoring` | Author Animus skills |
| `/animus-troubleshooting` | Debug common issues |

</td>
<td width="50%">

**Auto-Loaded References**

| Skill | Coverage |
|:---|:---|
| `animus-configuration` | Config files, state layout, model routing |
| `animus-task-management` | Full task lifecycle via CLI and MCP |
| `animus-daemon-operations` | Daemon monitoring and troubleshooting |
| `animus-queue-management` | Dispatch queue operations |
| `animus-workflow-patterns` | Patterns from 150+ autonomous PRs |
| `animus-agent-personas` | PO, architect, auditor agents |
| `animus-mcp-tools` | Complete `animus.*` tool reference |
| `animus-mcp-servers-for-agents` | Context7, GitHub, memory MCP wiring |

</td>
</tr>
</table>

---

## CLI

```
animus subject       Unified subject surface: list/get/create/update/next/status --kind <kind>
                     (kind=task and kind=requirement are served by installed subject_backend plugins)
animus workflow      Run and manage multi-phase workflows
animus daemon        Start/stop the autonomous scheduler (start, stop, health, stream)
animus queue         Inspect and manage the dispatch queue
animus agent         Control agent runner processes
animus output        Stream and inspect agent output
animus logs          Tail daemon events.jsonl or whichever log-storage plugin is active
animus trigger       Manage event triggers (file_watcher, webhook, github_webhook, slack)
animus pack          Install, list, and update workflow packs
animus plugin        Install, list, inspect, and scaffold stdio plugins
animus skill         Install and inspect Animus skills
animus git           Worktree and branch helpers
animus history       Inspect run history (includes phase + runtime error reports)
animus init          Initialize a project from a template registry or local template
animus mcp           Start Animus as an MCP server
animus web           Launch installed web dashboard/transport plugins
animus status        Project overview at a glance
animus approval      Manage approvals for destructive operations
animus auth          Inspect identity and permissions
animus events        Stream workflow lifecycle events
animus state         Export and import scoped runtime state
animus secret        Manage project-scoped secrets in the OS keychain
animus doctor        Health checks, auto-remediation, and troubleshooting
```

Run `animus --help` for the full surface.

**Removed in v0.4.4:** `animus task` (→ `animus subject --kind task`),
`animus requirements` (→ `animus subject --kind requirement`),
`animus setup` (→ `animus init`), `animus now` (→ `animus status`),
`animus errors` (→ `animus history`).

---

## Architecture

Animus v0.5 is a **kernel + flavors** architecture: a Rust workspace daemon
kernel plus a curated bundle of out-of-tree plugins for providers, subject
backends, workflow execution, queues, transports, and web UI. The current
workspace members from `Cargo.toml` are:

- `animus-plugin-protocol` — in-tree stdio plugin protocol types
- `animus-plugin-runtime` — runtime helpers for plugin implementations
- `orchestrator-daemon-runtime` — daemon queue, scheduling, subject dispatch, and runtime supervision
- `orchestrator-logging` — shared tracing and log-file utilities
- `orchestrator-plugin-host` — plugin discovery, install state, stdio host, and provider session bridge
- `orchestrator-config` — workflow YAML loading, pack loading, scaffolding, and phase plan resolution
- `orchestrator-core` — domain services, bootstrap, plugin registry, and state mutation APIs
- `orchestrator-cli` — main `animus` binary, clap surface, MCP server, and CLI operations
- `animus-runtime-shared` — shared workflow execution and runtime-contract helpers used by the daemon and external `workflow_runner` plugins
- `animus-mcp-oauth` — OAuth authorization-code + PKCE helpers and the `animus-mcp-proxy` bridge for protected MCP servers

Shared `protocol::*`, config-source, provider/session, and subject wire types
now come from the external `launchapp-dev/animus-protocol` dependencies pinned
in the workspace `Cargo.toml` files rather than an in-tree crate.

Provider execution and the web stack are no longer in-tree crates. The
OpenAI-compatible runner ships as the external
[`launchapp-dev/animus-provider-oai-agent`](https://github.com/launchapp-dev/animus-provider-oai-agent)
plugin, and `animus web` resolves external transport/UI plugins rather than an
embedded web server.

**v0.5 reference plugins** (install via `animus plugin install-defaults`):

- [`animus-workflow-runner-default`](https://github.com/launchapp-dev/animus-workflow-runner-default) `v0.4.1` — Rust workflow_runner plugin
- [`animus-queue-default`](https://github.com/launchapp-dev/animus-queue-default) `v0.2.0` — Rust queue plugin with atomic `queue/lease` + `queue/release_pending`
- [`animus-step-durable-dbos`](https://github.com/launchapp-dev/animus-step-durable-dbos) `v0.2.0` — Postgres + DBOS-backed durable_store
- [`animus-memory-zep`](https://github.com/launchapp-dev/animus-memory-zep) `v0.1.0` — Zep Cloud memory_store

See [`docs/architecture/full-system-architecture.md`](docs/architecture/full-system-architecture.md),
[`docs/architecture/runtime-architecture.md`](docs/architecture/runtime-architecture.md),
and [`docs/architecture/plugin-system.md`](docs/architecture/plugin-system.md)
for the current source-backed architecture docs.

The web dashboard is no longer bundled in-tree. Install it as plugins via
`animus plugin install-defaults --include-transports`
(`animus-transport-http` + `animus-transport-graphql` + `animus-web-ui`).

```mermaid
graph LR
    A[CLI] --> B[Core Services]
    A --> C[Daemon Runtime]
    B --> D[Workflow Runner]
    D --> E[Agent Runner]
    E --> F[Session Host]
    F --> G[Provider Plugins]
    B --> H[Config Compiler]
    A --> I[Plugin Host]
    C --> I
    I --> J[Subject / Trigger / Transport Plugins]
    C --> D
    style A fill:#1f6feb,stroke:#1f6feb,color:#fff
    style C fill:#1f6feb,stroke:#1f6feb,color:#fff
    style I fill:#1f6feb,stroke:#1f6feb,color:#fff
```

---

## Platforms

| Platform | Architecture | |
|:---|:---|:---|
| macOS | Apple Silicon (M1+) | `aarch64-apple-darwin` |
| macOS | Intel | `x86_64-apple-darwin` |
| Linux | x86_64 | `x86_64-unknown-linux-gnu` |
| Linux | arm64 | `aarch64-unknown-linux-gnu` |
| Windows | x86_64 | `x86_64-pc-windows-msvc` |

---

## License

This project is licensed under the [Elastic License 2.0 (ELv2)](LICENSE). You may use, modify, and distribute the software, but you may not provide it to third parties as a hosted or managed service.

---

<div align="center">

**Update**

```bash
curl -fsSL https://raw.githubusercontent.com/launchapp-dev/animus-cli/main/scripts/install.sh | bash
```

**Uninstall**

```bash
rm -f ~/.local/bin/animus \
  ~/.local/bin/ao
```

<br/>

<sub>Open source. Local-first. Built by founders running too many projects at once.</sub>

</div>

![footer](https://capsule-render.vercel.app/api?type=waving&color=0:0d1117,50:161b22,100:1f6feb&height=100&section=footer)
