# AGENTS.md

Operator and contributor guide for AO (`ao` CLI).

## Mission

Use AO to build AO. Requirements, tasks, workflows, queue entries, and review state
in this repo are AO-managed data, and this workspace remains Rust-only.

## Source Of Truth

When prose and code disagree, trust the code and generated references:

- `Cargo.toml` for current workspace members
- `docs/reference/cli/index.md` for the current CLI tree
- `docs/reference/mcp-tools.md` and `docs/guides/agents.md` for MCP surface docs
- `crates/orchestrator-cli/src/cli_types/root_types.rs` for visible and hidden top-level commands
- `crates/orchestrator-core/src/config.rs` for project-root resolution
- `crates/orchestrator-core/src/services.rs` for bootstrap and state persistence
- `crates/orchestrator-web-server/web-ui/package.json` and `web-ui/src/app/router.tsx` for web UI stack and routes

Do not keep outdated counts or removed crates alive in docs.

## Workspace

17-crate Rust workspace. Main binary: `ao` (`crates/orchestrator-cli`).

```text
crates/
├── agent-runner/                # Runner process that launches and supervises AI CLIs
├── orchestrator-logging/        # Shared tracing and log file utilities
├── llm-cli-wrapper/             # Claude/Codex/Gemini CLI integration layer
├── oai-runner/                  # OpenAI-compatible runner implementation
├── orchestrator-cli/            # Main `ao` binary
├── orchestrator-config/         # Workflow and agent-runtime config loading/compilation
├── orchestrator-core/           # Domain services, bootstrap, state mutation APIs
├── orchestrator-daemon-runtime/ # Daemon queue and scheduling runtime
├── orchestrator-git-ops/        # Git/worktree automation helpers
├── orchestrator-notifications/  # Notification/runtime integration support
├── orchestrator-providers/      # Built-in provider integrations and routing
├── orchestrator-store/          # Shared state/storage path helpers
├── orchestrator-web-api/        # Web API business logic
├── orchestrator-web-contracts/  # Shared web/API contract types
├── orchestrator-web-server/     # Axum server + embedded web UI
├── protocol/                    # Shared protocol/config/runtime types
└── workflow-runner-v2/          # Workflow execution runtime and phase output persistence
```

Runtime-critical binaries and supporting crates must stay healthy:

- `orchestrator-cli`
- `agent-runner`
- `llm-cli-wrapper`
- `oai-runner`
- `orchestrator-logging`
- `workflow-runner-v2`
- `orchestrator-daemon-runtime`

Do not add desktop shell frameworks or their transitive equivalents.

## Runtime Model

Startup flow:

1. Parse global flags and the selected top-level command.
2. Resolve project root with this precedence:
   1. `--project-root`
   2. Git common root for the current working directory or linked worktree
   3. Current working directory
3. Bootstrap project-local `.ao/` files and scoped runtime state under `~/.ao/<repo-scope>/`.
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

AO now splits project-local config from scoped runtime state.

Project-local config in `<project>/.ao/`:

- `config.json`
- `workflows.yaml`
- `workflows/*.yaml`

Scoped runtime state in `~/.ao/<repo-scope>/`:

- `core-state.json`
- `resume-config.json`
- `workflow.db`
- `config/`
- `daemon/`
- `docs/`
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
- managed task worktrees live under `~/.ao/<repo-scope>/worktrees/`

Legacy fallback readers still check some repo-local run/artifact paths. Do not depend on
those fallback locations for new features unless you are intentionally preserving compatibility.

## Command Surface

Visible top-level commands:

- `version`
- `daemon`
- `agent`
- `project`
- `queue`
- `task`
- `workflow`
- `requirements`
- `history`
- `errors`
- `git`
- `skill`
- `model`
- `pack`
- `runner`
- `status`
- `output`
- `mcp`
- `web`
- `setup`
- `sync`
- `doctor`

Hidden/internal top-level commands:

- `review`
- `qa`

Use these reference docs instead of hand-maintained summaries:

- `docs/reference/cli/index.md`
- `docs/reference/mcp-tools.md`
- `docs/guides/agents.md`

## Mutation Policy

- Treat `~/.ao/<repo-scope>/` and `.ao/*.json` as AO-managed state.
- Use `ao project`, `ao queue`, `ao task`, `ao workflow`, `ao requirements`, `ao review`, and `ao qa` commands for mutations.
- Do not hand-edit generated state JSON unless the task is explicitly about persistence or migrations.
- Supported exception: project-local workflow YAML overlays in `.ao/workflows.yaml` and `.ao/workflows/*.yaml`.
- In scripts and automation, always pass `--project-root "$(pwd)"`.

## Contributor Rules

- Keep the workspace Rust-only. Do not introduce `tauri`, `wry`, `tao`, `gtk`, `webkit*`, `webview*`, or similar desktop shell dependencies.
- If you change CLI surface, update `docs/reference/cli/index.md` in the same change.
- If you change MCP tools, update both `docs/reference/mcp-tools.md` and `docs/guides/agents.md`.
- If you change workflow config loading, verify both project-local YAML overlays and compiled scoped state behavior.
- Prefer source files over prose for command counts, crate counts, routes, or runtime paths.

## Useful Commands

```bash
cargo ao-fmt
cargo ao-lint
cargo ao-bin-check
cargo test -p orchestrator-cli
cargo test --workspace

ao status
ao project list
ao task prioritized
ao task next
ao queue list
ao daemon health
ao workflow list
```

## Self-Hosting Flow

```bash
ao task next
ao task status --id TASK-XXX --status in-progress
ao workflow run --task-id TASK-XXX
ao output monitor --run-id <run-id>
ao task status --id TASK-XXX --status done
```
