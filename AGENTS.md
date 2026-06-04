# AGENTS.md

Operator and contributor guide for Animus (`animus` / `ao` CLI).

## Mission

Use Animus to build Animus. Requirements, tasks, workflows, queue entries, and review state
in this repo are Animus-managed data, and this workspace remains Rust-only.

## Source Of Truth

When prose and code disagree, trust the code and generated references:

- `Cargo.toml` for current workspace members
- `docs/reference/cli/index.md` for the current CLI tree
- `docs/reference/mcp-tools.md` and `docs/guides/agents.md` for MCP surface docs
- `crates/orchestrator-cli/src/cli_types/root_types.rs` for the current top-level commands
- `crates/orchestrator-core/src/config.rs` for project-root resolution
- `crates/orchestrator-core/src/services.rs` for bootstrap and state persistence
- `crates/orchestrator-cli/src/services/operations/ops_web.rs` for how `animus web` resolves the external transport plugins

Do not keep outdated counts or removed crates alive in docs.

## Workspace

Rust Cargo workspace with the current members listed below. Main binary: `animus` (`crates/orchestrator-cli`). The
web stack (transport + UI) lives in external plugins under
[`launchapp-dev`](https://github.com/launchapp-dev). The workspace also depends
on external protocol crates from that repo. The authoritative pins live in
`Cargo.toml` and `crates/orchestrator-cli/Cargo.toml`; current builds mix the
legacy `v0.1.13` wire crates with newer `v0.5.x`
queue/workflow/subject protocol crates.

Current workspace members from `Cargo.toml`:

```text
crates/
├── agent-runner/                # Runner process that launches and supervises AI CLIs
├── animus-runtime-shared/       # Shared workflow execution/runtime-contract/IPC helpers used by daemon and workflow_runner plugins
├── animus-plugin-protocol/      # In-tree stdio plugin protocol types
├── animus-plugin-runtime/       # Runtime helpers for plugin implementations
├── orchestrator-cli/            # Main `animus` binary
├── orchestrator-config/         # Workflow, pack, and template config loading
├── orchestrator-core/           # Domain services, subject_adapter, store, bootstrap, state mutation APIs
├── orchestrator-daemon-runtime/ # Daemon queue and scheduling runtime
├── orchestrator-logging/        # Shared tracing and log file utilities
├── orchestrator-plugin-host/    # Plugin discovery, install, stdio host, session backend bridge (folded in v0.5.3)
└── protocol/                    # Shared protocol/config/runtime types
```

The OpenAI-compatible runner (formerly `crates/oai-runner/`) moved
out-of-tree to `launchapp-dev/animus-provider-oai-agent` v0.1.3 in the
v0.5.2 surface-shrink. Install via
`animus plugin install launchapp-dev/animus-provider-oai-agent`.

Repo-local but not current workspace members:

- `crates/orchestrator-web-server/`

Runtime-critical binaries and supporting crates must stay healthy:

- `orchestrator-cli`
- `agent-runner`
- `orchestrator-logging`
- `animus-runtime-shared`
- `orchestrator-daemon-runtime`

Do not add desktop shell frameworks or their transitive equivalents.

## Runtime Model

Startup flow:

1. Parse global flags and the selected top-level command.
2. Resolve project root with this precedence:
   1. `--project-root`
   2. Git common root for the current working directory or linked worktree
   3. Current working directory
3. Bootstrap project-local `.animus/` files and scoped runtime state under `~/.animus/<repo-scope>/`.
4. Construct `FileServiceHub`.
5. Dispatch into CLI operations, daemon runtime, agent runtime, or web services.

Key implementation files:

- `crates/orchestrator-cli/src/main.rs`
- `crates/orchestrator-cli/src/cli_types/root_types.rs`
- `crates/orchestrator-cli/src/shared/output.rs`
- `crates/orchestrator-core/src/config.rs`
- `crates/orchestrator-core/src/services.rs`
- `crates/orchestrator-config/src/workflow_config/`
- `crates/protocol/src/config.rs`
- `crates/protocol/src/repository_scope.rs`

## State Layout

Animus now splits project-local config from scoped runtime state.

Project-local config in `<project>/.animus/`:

- `config.json`
- `workflows.yaml`
- `workflows/*.yaml`
- `plugins.lock` (created by plugin install/update flows when project-local lockfile resolution is active)

Scoped runtime state in `~/.animus/<repo-scope>/`:

- `core-state.json`
- `resume-config.json`
- `workflow.db`
- `config/`
- `daemon/`
- `docs/`
- `logs/`
- `runner/`
- `state/`
- `worktrees/`

Global config in `protocol::Config::global_config_dir()`:

- `config.json`
- `credentials.json`
- `daemon-events.jsonl`
- `cli-tracker.json`
- runner socket and runner config files

Repository scope format:

- `<repo-scope>` = `<sanitized-repo-name>-<12 hex sha256(canonical-root)>`
- managed task worktrees live under `~/.animus/<repo-scope>/worktrees/`

Legacy fallback readers still check some repo-local run/artifact paths. Do not depend on
those fallback locations for new features unless you are intentionally preserving compatibility.

## Command Surface

Visible top-level commands:

- `version`
- `daemon`
- `agent`
- `project`
- `queue`
- `workflow`
- `history`
- `git`
- `skill`
- `model`
- `pack`
- `plugin`
- `runner`
- `status`
- `output`
- `mcp`
- `web`
- `init`
- `doctor`
- `trigger`
- `logs`
- `subject`
- `flavor`

Use these reference docs instead of hand-maintained summaries:

- `docs/reference/cli/index.md`
- `docs/reference/mcp-tools.md`
- `docs/guides/agents.md`

## Mutation Policy

- Treat `~/.animus/<repo-scope>/` and `.animus/*.json` as Animus-managed state.
- Use `animus project`, `animus queue`, `animus subject`, `animus workflow`, `animus plugin`, and `animus pack` commands for mutations.
- Do not hand-edit generated state JSON unless the task is explicitly about persistence or migrations.
- Supported exception: project-local workflow YAML overlays in `.animus/workflows.yaml` and `.animus/workflows/*.yaml`.
- In scripts and automation, always pass `--project-root "$(pwd)"`.

## Contributor Rules

- Keep the workspace Rust-only. Do not introduce `tauri`, `wry`, `tao`, `gtk`, `webkit*`, `webview*`, or similar desktop shell dependencies.
- If you change CLI surface, update `docs/reference/cli/index.md` in the same change.
- If you change MCP tools, update both `docs/reference/mcp-tools.md` and `docs/guides/agents.md`.
- If you change workflow config loading, verify both project-local YAML overlays and compiled scoped state behavior.
- Prefer source files over prose for command counts, crate counts, routes, or runtime paths.

## Useful Commands

```bash
cargo animus-fmt
cargo animus-lint
cargo animus-bin-check
cargo test -p orchestrator-cli
cargo test --workspace

animus status
animus project list
animus subject list --kind task
animus subject next --kind task
animus queue list
animus daemon health
animus workflow list
```

## Self-Hosting Flow

```bash
animus subject next --kind task
animus subject status --kind task --id task:TASK-XXX --status in_progress
animus workflow run --task-id TASK-XXX
animus output monitor --run-id <run-id>
animus subject status --kind task --id task:TASK-XXX --status done
```
