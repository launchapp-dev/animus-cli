# AGENTS.md

Operator and contributor guide for AO (`ao` CLI).

## Mission

Use AO to build AO. Requirements and tasks in this repo are the planning source
of truth, and this workspace stays Rust-only without desktop-wrapper dependencies.

## Workspace

10-crate Rust workspace. Main binary: `ao` (`crates/orchestrator-cli`).

```
crates/
├── orchestrator-cli/        # Main `ao` binary (clap CLI, 24 commands, ~130+ subcommands)
├── orchestrator-core/       # Domain logic, state management, FileServiceHub DI
├── orchestrator-web-api/    # Web API business logic (WebApiService)
├── orchestrator-web-server/ # Axum web server + embedded static assets
├── orchestrator-web-contracts/ # Shared web types
├── protocol/                # Wire protocol types shared across all crates
├── agent-runner/            # Standalone daemon managing LLM CLI processes via IPC
├── llm-cli-wrapper/         # Abstraction over AI CLI tools (claude, codex, gemini, etc.)
├── oai-runner/              # OpenAI-compatible streaming API client
└── llm-mcp-server/          # MCP server for external agent bridging
```

Supporting runtime crates must remain healthy:
`agent-runner`, `llm-cli-wrapper`, `llm-mcp-server`, `oai-runner`.

Do not add or depend on desktop shell frameworks.

## How `ao` Works

Startup flow:

1. Parse CLI args (`--json`, `--project-root`, subcommand).
2. Resolve project root with precedence:
   1. `--project-root`
   2. `PROJECT_ROOT` env var
   3. Registry fallback `~/.config/agent-orchestrator/last-project-root`
   4. Current working directory
3. Construct `FileServiceHub` for that root.
4. Dispatch subcommands into runtime and operations handlers.

`FileServiceHub` bootstrap: ensures project root exists, initializes git repo if missing, creates `.ao/` base structure and core state/config files.

## Data Layout

Repo-local state (`.ao/`):

- `core-state.json`, `config.json`, `resume-config.json`
- `docs/{vision,requirements,tasks,architecture}.json`
- `requirements/index.json` and `requirements/generated/*.json`
- `tasks/index.json` and `tasks/TASK-*.json`
- `state/{workflow-config.v2,agent-runtime-config.v2,state-machines.v1}.json`
- `state/{reviews,handoffs,history,errors,qa-results,qa-review-approvals}.json`
- `runs/<run_id>/events.jsonl`
- `artifacts/<execution_id>/...`

Global files (`protocol::Config::global_config_dir()`, overridable by `AO_CONFIG_DIR`):

- `projects.json`, `daemon-events.jsonl`, runner/global config

## Worktree Model

Daemon-managed task worktrees: `~/.ao/<repo-scope>/worktrees/`

- `<repo-scope>` = `<sanitized-repo-name>-<12 hex SHA256(canonical root)>`
- Task defaults: worktree `task-<sanitized-task-id>`, branch `ao/<sanitized-task-id>`

## Runner and Agent Execution

1. `ao agent run` builds runtime context (tool/model/prompt/cwd/runtime_contract)
2. CWD canonicalized, must stay inside project root
3. CLI connects to `agent-runner` via unix socket (TCP on non-unix)
4. Runner executes CLI launch from runtime contract
5. `llm-cli-wrapper` enforces machine-readable JSON flags per AI CLI
6. Events stream back, optionally persisted in `.ao/runs/<run_id>/events.jsonl`

Runner config precedence: `AO_RUNNER_CONFIG_DIR` > `AO_CONFIG_DIR` > `AGENT_ORCHESTRATOR_CONFIG_DIR` > scope-based default.

## Output and Error Contract

With `--json`, envelope schema `ao.cli.v1`:

- Success: `{ "schema": "ao.cli.v1", "ok": true, "data": ... }`
- Error: `{ "schema": "ao.cli.v1", "ok": false, "error": { "code", "message", "exit_code" } }`

Exit codes: 1=internal, 2=invalid_input, 3=not_found, 4=conflict, 5=unavailable

## CLI Command Surface

Full reference: `docs/cli-command-surface.md`

| Group | Commands | Purpose |
|---|---|---|
| **Core** | `task`, `workflow`, `daemon`, `agent` | Task CRUD, workflow execution, daemon lifecycle, agent runs |
| **Planning** | `vision`, `requirements`, `execute`, `architecture` | Vision drafting, requirements, execution planning, architecture graph |
| **Operations** | `runner`, `output`, `errors`, `history` | Runner health, run output, error tracking, history |
| **Infrastructure** | `git`, `model`, `skill`, `mcp`, `web` | Git ops, model routing, skill packages, MCP server, web UI |
| **UX** | `status`, `setup`, `doctor`, `tui`, `workflow-monitor` | Dashboard, onboarding, diagnostics, TUI |
| **Review/QA** | `review`, `qa` | Review decisions, QA gates |
| **Facade** | `planning` | Mirrors vision + requirements (legacy alias) |

## Key Implementation Files

- CLI dispatch: `crates/orchestrator-cli/src/main.rs`
- CLI type definitions: `crates/orchestrator-cli/src/cli_types/` (modularized per domain)
- Error classification: `crates/orchestrator-cli/src/shared/output.rs`
- Runner IPC: `crates/orchestrator-cli/src/shared/runner.rs`
- Core state: `crates/orchestrator-core/src/services.rs`
- Protocol types: `crates/protocol/src/lib.rs`
- Model routing: `crates/protocol/src/model_routing.rs`

## Accepted Value Sets

| Field | Values |
|---|---|
| Task status | `backlog`, `todo`, `ready`, `in-progress`, `blocked`, `on-hold`, `done`, `cancelled` |
| Task type | `feature`, `bugfix`, `hotfix`, `refactor`, `docs`, `test`, `chore`, `experiment` |
| Task priority | `critical`, `high`, `medium`, `low` |
| Requirement priority | `must`, `should`, `could`, `wont` |
| Requirement status | `draft`, `refined`, `planned`, `in-progress`, `done` |

## `.ao/` Mutation Policy

`.ao/` is repository state managed exclusively through `ao` commands.

- Use `ao vision/requirements/task/workflow ...` for all state changes
- Never hand-edit `.ao/*.json` files
- Exception: migration tooling or persistence changes that are the subject of the task
- In scripts: always pass `--project-root "$(pwd)"`

## Agent Task Policy

1. **Before work**: create/claim task, link to requirement, set status `in-progress`
2. **During work**: update status at state changes, record blockers in metadata
3. **After work**: set status `done`, run review/QA if gated
4. **Parallel work**: use workflow phases, not competing manual agents on same workspace

## Self-Hosting Workflow

```bash
ao requirements list                              # View backlog
ao task prioritized                               # View prioritized tasks
ao task next                                      # Get next task
ao task status --id TASK-XXX --status in-progress # Start
ao task status --id TASK-XXX --status done        # Complete
```

## Daemon Operations

```bash
ao daemon start --pool-size 5                     # Start with 5 agent slots
ao daemon health                                  # Check capacity
ao daemon events --limit 50                       # Recent events
ao runner health                                  # Runner process health
ao workflow list                                  # Active workflows
```

Diagnostics: `ao daemon health --json`, `ao runner health --json`, `ao daemon events --limit 50 --follow false --json`
