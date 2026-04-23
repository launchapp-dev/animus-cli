<div align="center">

![header](https://capsule-render.vercel.app/api?type=waving&color=0:0d1117,50:161b22,100:1f6feb&height=200&section=header&text=Animus&fontSize=90&fontColor=f0f6fc&animation=fadeIn&fontAlignY=35&desc=Autonomous%20Agent%20Orchestrator&descAlignY=55&descSize=22&descColor=8b949e)

<br/>

[![Typing SVG](https://readme-typing-svg.demolab.com?font=JetBrains+Mono&weight=500&size=20&duration=3000&pause=1500&color=58A6FF&center=true&vCenter=true&multiline=true&repeat=true&random=false&width=700&height=80&lines=Define+your+engineering+team+as+YAML.;Dispatch+tasks+to+AI+agents+across+isolated+worktrees.;Review%2C+merge%2C+and+ship+%E2%80%94+while+you+sleep.)](https://github.com/samishukri/animus)

<br/>
<br/>
<br/>


<a href="https://github.com/launchapp-dev/ao/releases/latest"><img src="https://img.shields.io/github/v/release/launchapp-dev/ao?style=for-the-badge&color=1f6feb&labelColor=0d1117&logo=github&logoColor=f0f6fc" alt="Release" /></a>
&nbsp;
<img src="https://img.shields.io/badge/rust-100%25-f0f6fc?style=for-the-badge&labelColor=0d1117&logo=rust&logoColor=f0f6fc" alt="Rust" />
&nbsp;
<img src="https://img.shields.io/badge/macOS%20%7C%20Linux%20%7C%20Windows-f0f6fc?style=for-the-badge&labelColor=0d1117&logo=apple&logoColor=f0f6fc" alt="Platforms" />
&nbsp;
<a href="https://github.com/launchapp-dev/awesome-ai-coding-tools"><img src="https://awesome.re/mentioned-badge-flat.svg" alt="Mentioned in Awesome AI Coding Tools" /></a>

</div>

<p align="center">
<sub>AI agent orchestrator | autonomous coding agents | multi-model AI dev team | Claude + Gemini + GPT workflow automation | MCP integration | YAML-driven CI for AI | Rust CLI</sub>
</p>

<br/>

## Install

```bash
curl -fsSL https://raw.githubusercontent.com/launchapp-dev/ao/main/install.sh | bash
```

The upstream installer currently targets macOS. On Linux and Windows, use a release archive or build from source.

<details>
<summary><kbd>options</kbd></summary>

```bash
# Specific version
AO_VERSION=v0.3.0 curl -fsSL https://raw.githubusercontent.com/launchapp-dev/ao/main/install.sh | bash

# Custom directory
AO_INSTALL_DIR=/usr/local/bin curl -fsSL https://raw.githubusercontent.com/launchapp-dev/ao/main/install.sh | bash
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

Animus turns a single YAML file into an autonomous software delivery pipeline.

You define agents, wire them into phases, compose phases into workflows, schedule everything with cron — and Animus's daemon handles the rest: dispatching tasks to AI agents in isolated git worktrees, managing quality gates, and merging the results.

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

## Quick Start

```bash
cd your-project                          # any git repo
animus doctor                            # check prerequisites and auto-remediate
animus init --template task-queue --non-interactive  # initialize .ao/ with a queue-first workflow template

# Option 1: Run workflows on demand
animus task create --title "Add rate limiting" --task-type feature --priority high
animus workflow run --task-id TASK-001

# Option 2: Go fully autonomous (v0.3.0+)
animus daemon start --autonomous         # continuous execution with event triggers
```

### v0.3.0: Autonomous Mode is Production-Ready

The daemon now runs as your primary software delivery engine. Enable event triggers, cloud sync, and automatic quality gates:

```bash
animus config set daemon.autonomous true
animus config set cloud.sync enabled
animus daemon start                      # runs forever, responds to webhooks and cron
```

---

## Cloud Integration (v0.3.0+)

Sync your Animus state to a cloud backend for team visibility, distributed execution, and webhook-driven automation.

```bash
animus cloud login                       # authenticate with your workspace
animus cloud sync --status               # check sync status
animus config set cloud.webhook-url "https://your-domain.com/webhook"
```

Features:
- **Team Sync**: All team members see the same task queue, execution logs, and run history
- **Webhook Triggers**: GitHub, Linear, Slack, and custom webhooks trigger workflows automatically
- **Distributed Daemon**: Run multiple daemon instances across regions with automatic failover
- **Execution Timeline**: Inspect runs, decisions, and agent reasoning in the cloud dashboard

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

### Schedules & Event Triggers (v0.3.0+)

Cron-based autonomous execution **and** event-driven triggers. The daemon responds to webhooks, git events, and external integrations.

```yaml
schedules:
  - id: work-planner
    cron: "*/5 * * * *"
    workflow_ref: work-planner
    enabled: true

triggers:
  - id: pr-opened
    event: "github.pull_request.opened"
    workflow_ref: pr-reviewer
  
  - id: task-updated
    event: "linear.issue.updated"
    workflow_ref: work-planner
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

## Claude Code Integration

Install [**Animus Skills**](https://github.com/samishukri/animus-skills) for deep Animus integration inside Claude Code:

```bash
git clone https://github.com/samishukri/animus-skills.git ~/animus-skills
claude --plugin-dir ~/animus-skills
```

<table>
<tr>
<td width="50%">

**Slash Commands**

| Command | What it does |
|:---|:---|
| `/setup-animus` | Initialize Animus in your project |
| `/getting-started` | Install, concepts, first task |
| `/workflow-authoring` | Write custom YAML workflows |
| `/pack-authoring` | Build workflow packs |
| `/mcp-setup` | Connect AI tools via MCP |
| `/troubleshooting` | Debug common issues |

</td>
<td width="50%">

**Auto-Loaded References**

| Skill | Coverage |
|:---|:---|
| `configuration` | Config files, state layout, model routing |
| `task-management` | Full task lifecycle via CLI and MCP |
| `daemon-operations` | Daemon monitoring and troubleshooting |
| `workflow-patterns` | Patterns from 150+ autonomous PRs |
| `agent-personas` | PO, architect, auditor agents |
| `mcp-tools` | Complete `animus.*` tool reference |

</td>
</tr>
</table>

---

## CLI

```
animus task          Create, list, update, prioritize tasks
animus workflow      Run and manage multi-phase workflows
animus daemon        Start/stop the autonomous scheduler (v0.3.0: event-driven)
animus queue         Inspect and manage the dispatch queue
animus agent         Control agent runner processes
animus output        Stream and inspect agent output
animus doctor        Health checks, auto-remediation, and troubleshooting (v0.3.0+)
animus cloud         Sync state, manage webhooks, and access cloud dashboard (v0.3.0+)
animus init          Initialize a project from a bundled or local template
animus setup         Lower-level bootstrap and configuration wizard
animus requirements  Manage product requirements
animus mcp           Start Animus as an MCP server
animus web           Launch the embedded web dashboard
animus status        Project overview at a glance
```

---

## Architecture

Animus is a Rust-only workspace with 17 crates. The major crates are:

- `orchestrator-cli` - CLI commands and dispatch
- `orchestrator-core` - services, state, and workflow lifecycle
- `orchestrator-config` - workflow YAML scaffolding, loading, and compilation
- `workflow-runner-v2` - workflow execution runtime
- `agent-runner` - LLM CLI process management
- `llm-cli-wrapper` - CLI tool abstraction layer
- `orchestrator-daemon-runtime` - daemon scheduler, cron, event triggers (v0.3.0+)
- `orchestrator-providers` - provider integrations (cloud sync, webhooks, auth)
- `orchestrator-notifications` - event streaming and cloud syncing (v0.3.0+)
- `orchestrator-logging` - shared logging utilities
- `orchestrator-web-server` - embedded React dashboard
- `orchestrator-web-api` - web API business logic
- `orchestrator-store` - persistence primitives
- `protocol` - shared types and routing

```mermaid
graph LR
    A[CLI] --> B[Core Services]
    A --> C[Daemon Runtime]
    B --> D[Workflow Runner]
    D --> E[Agent Runner]
    E --> F[LLM CLI Wrapper]
    F --> G[claude / codex / gemini]
    B --> H[Config]
    H --> I[YAML Compiler]
    A --> J[Web Server]
    J --> K[Web API]
    K --> B
    C --> D
    style A fill:#1f6feb,stroke:#1f6feb,color:#fff
    style C fill:#1f6feb,stroke:#1f6feb,color:#fff
    style J fill:#1f6feb,stroke:#1f6feb,color:#fff
```

---

## Platforms

| Platform | Architecture | |
|:---|:---|:---|
| macOS | Apple Silicon (M1+) | `aarch64-apple-darwin` |
| macOS | Intel | `x86_64-apple-darwin` |
| Linux | x86_64 | `x86_64-unknown-linux-gnu` |
| Windows | x86_64 | `x86_64-pc-windows-msvc` |

---

## License

This project is licensed under the [Elastic License 2.0 (ELv2)](LICENSE). You may use, modify, and distribute the software, but you may not provide it to third parties as a hosted or managed service.

---

<div align="center">

**Update**

```bash
curl -fsSL https://raw.githubusercontent.com/launchapp-dev/ao/main/install.sh | bash
```

**Uninstall**

```bash
rm -f ~/.local/bin/animus \
  ~/.local/bin/agent-runner \
  ~/.local/bin/llm-cli-wrapper \
  ~/.local/bin/animus-oai-runner \
  ~/.local/bin/animus-workflow-runner
```

<br/>

<sub>Built with Rust. Powered by AI. Ships code autonomously.</sub>

</div>

![footer](https://capsule-render.vercel.app/api?type=waving&color=0:0d1117,50:161b22,100:1f6feb&height=100&section=footer)
