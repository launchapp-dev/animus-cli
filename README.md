# Agent Orchestrator

A desktop application for orchestrating AI agents, planning work, and running workflow execution with both UI and CLI interfaces.

## `ao` CLI (orchestrator-cli)

The `ao` binary is the standalone Rust CLI in `crates/orchestrator-cli`.

- Binary name: `ao`
- Package: `orchestrator-cli`
- Workspace: root `Cargo.toml` includes it in workspace members

### Build and run

```bash
# From repository root
cargo run -p orchestrator-cli -- --help

# Common quick starts
cargo build -p orchestrator-cli
./target/debug/ao --help
```

## 1. How the CLI works (internals)

### 1.1 Execution flow

- `clap` parses arguments into `Cli` and a command enum in `cli_types.rs`.
- `main()` runs `run(cli)`:
  1. Builds `RuntimeConfig`.
  2. Resolves project root with this precedence: 
     1) `--project-root`
     2) `PROJECT_ROOT`
     3) persisted last project from registry
     4) current working directory
     
     (implemented in `orchestrator_core::config::resolve_project_root`)
  3. Creates `FileServiceHub` for that root (`orchestrator_core::services::FileServiceHub`).
  4. Dispatches to the top-level command handler (`runtime` or `operations`).

### 1.2 Service architecture

The CLI is only a transport layer; domain logic lives in `orchestrator-core` services.

- `hub.projects()`
- `hub.tasks()`
- `hub.workflows()`
- `hub.planning()`
- `hub.daemon()`

Each command handler builds a typed input, calls one service API, then prints JSON/plain output.

Most domain state is persisted through the file-backed hub:
- `project-root/.ao/core-state.json` (core domain state)
- `.ao/state/*` for CLI-managed artifacts (history, QA, review, model, git metadata, etc.)

### 1.3 Error handling & output contract

Global output modes:
- plain mode (`--json` omitted): prints pretty JSON values for all success responses, and prints errors to stderr as `error: ...`
- JSON mode (`--json`): wraps every success/error in a top-level envelope.

Success envelope:

```json
{
  "schema": "ao.cli.v1",
  "ok": true,
  "data": { ... }
}
```

Error envelope:

```json
{
  "schema": "ao.cli.v1",
  "ok": false,
  "error": {
    "code": "invalid_input|not_found|conflict|unavailable|internal",
    "message": "...",
    "exit_code": 2
  }
}
```

Exit codes:
- `0` success
- `1` internal
- `2` invalid input
- `3` not found
- `4` conflict
- `5` unavailable / dependency failure

### 1.4 Global flags and runtime context

Supported globally on every command:
- `--json` (`true` when present)
- `--project-root <path>`

Runner / daemon context flags can be passed per command:
- `--runner-scope project|global`
- `--start-runner true|false`
- `--project-root` (for project resolution)

Additional runner env overrides (from runtime):
- `AO_RUNNER_CONFIG_DIR` / `AO_CONFIG_DIR` / `AGENT_ORCHESTRATOR_CONFIG_DIR`
- `AO_RUNNER_SCOPE`
- `AO_CONFIG_DIR` also affects protocol global config and daemon metadata files.

## 2. Command model

Top-level commands:

- `daemon`
- `agent`
- `project`
- `task`
- `workflow`
- `vision`
- `requirements`
- `execute`
- `planning`
- `review`
- `qa`
- `history`
- `errors`
- `task-control`
- `git`
- `model`
- `runner`
- `output`
- `web`
- `doctor`

Below is a practical command reference.

### 2.1 Daemon control

Use these for lifecycle + queue/heartbeat control.

```bash
ao daemon start [--max-agents <n>] [--skip-runner] [--autonomous] [--runner-scope project|global]
ao daemon run [--interval-secs <n>] [--once]
 ao daemon run also supports: include-registry, auto-run-ready, startup-cleanup, resume-interrupted, reconcile-stale, max-tasks-per-tick
ao daemon stop
ao daemon status
ao daemon health
ao daemon pause
ao daemon resume
ao daemon events [--limit <n>] [--follow true|false]
ao daemon logs [--limit <n>]
ao daemon clear-logs
ao daemon agents
ao daemon stop
```

Daemon events are written to:
- `<global config dir>/daemon-events.jsonl`
- each event has schema `ao.daemon.event.v1` when needed for machine parsing.

When running `daemon run --json`, records are printed as JSON lines to stdout.

### 2.2 Agent execution control

Agent commands are executed through the runner transport.

```bash
ao agent run [--run-id <id>] [--tool <tool>] [--model <model>] [--prompt <text>] [--cwd <path>] [--timeout-secs <n>] [--context-json '<json>'] [--runtime-contract-json '<json>'] [--detach] [--stream true|false] [--save-jsonl true|false] [--jsonl-dir <dir>] [--start-runner true|false] [--runner-scope project|global]
ao agent control --run-id <id> --action pause|resume|terminate [--start-runner] [--runner-scope]
ao agent status --run-id <id> [--jsonl-dir <dir>] [--start-runner]
ao agent model-status [--model <model> ...] [--start-runner] [--runner-scope]
ao agent runner-status [--start-runner] [--runner-scope]
```

Run output paths:
- events: `<project-root>/.ao/runs/<run-id>/events.jsonl`
- stream output JSON: `<project-root>/.ao/runs/<run-id>/json-output.jsonl`

### 2.3 Projects

```bash
ao project list
ao project active
ao project get --id <id>
ao project create --name <name> --path <path> [--project-type <type>] [--input-json '<json>']
ao project load --id <id>
ao project rename --id <id> --name <name>
ao project archive --id <id>
ao project remove --id <id>
```

### 2.4 Task management

```bash
ao task list [--task-type <type>] [--status <status>] [--priority <priority>] [--assignee-type <type>] [--tag <tag...>] [--linked-requirement <id>] [--search <text>]
ao task prioritized
ao task next
ao task stats
ao task get --id <id>
ao task create --title <title> [--description <text>] [--task-type <type>] [--priority <priority>] [--input-json '<json>']
ao task update --id <id> [--title <title>] [--description <text>] [--priority <priority>] [--status <status>] [--assignee <name>] [--input-json '<json>']
ao task delete --id <id>
ao task assign --id <id> --assignee <name>
ao task assign-agent --id <id> --role <role> [--model <model>] [--updated-by <user>]
ao task assign-human --id <id> --user-id <user> [--updated-by <user>]
ao task checklist-add --id <id> --description <text> [--updated-by <user>]
ao task checklist-update --id <id> --item-id <item_id> --completed <bool> [--updated-by <user>]
ao task dependency-add --id <id> --dependency-id <task_id> --dependency-type blocks-by|blocked-by|related-to [--updated-by <user>]
ao task dependency-remove --id <id> --dependency-id <task_id> [--updated-by <user>]
ao task status --id <id> --status <todo|ready|in-progress|done|blocked|on-hold|cancelled>
```

### 2.5 Workflows and planning pipeline

```bash
ao workflow list
ao workflow get --id <id>
ao workflow decisions --id <id>
ao workflow checkpoints list --id <id>
ao workflow checkpoints get --id <id> --checkpoint <n>
ao workflow run --task-id <task_id> [--pipeline-id <pipeline_id>]
ao workflow run --input-json '<json>'
ao workflow resume --id <id>
ao workflow resume-status --id <id>
ao workflow pause --id <id>
ao workflow cancel --id <id>

ao workflow pipelines
ao workflow config
ao workflow update-pipeline --id <id> --name <name> --phase <p1> --phase <p2> [--description <text>]
```

Default pipelines are stored in `<project-root>/.ao/state/workflow-config.json`.

### 2.6 Vision + requirements + execution

```bash
ao vision draft --problem <text> [--project-name <name>] [--target-user <user> ...] [--goal <goal> ...] [--constraint <constraint> ...] [--value-proposition <text>]
ao vision get

ao requirements draft [--include-codebase-scan true|false] [--append-only true|false] [--max-requirements <n>] [--input-json '<json>']
ao requirements list
ao requirements get --id <id>
ao requirements refine [--id <requirement_id> ...] [--focus <text>] [--input-json '<json>']
ao requirements create --title <title> [--description <text>] [--priority <priority>]
ao requirements update --id <id> [...]
ao requirements delete --id <id>
ao requirements graph get
nao requirements graph save --input-json '<json>'
ao requirements mockups list|create|link|get-file

ao execute plan [--id <requirement_id> ...] [--pipeline-id <pipeline_id>] [--input-json '<json>']
ao execute run [--id <requirement_id> ...] [--pipeline-id <pipeline_id>] [--input-json '<json>']

ao planning vision draft|get
nao planning requirements draft|list|get|refine|execute
```

Planning artifacts are persisted under `.ao/docs`:
- `product-vision.md`

### 2.7 Web GUI (browser UI hosted by CLI)

The new browser-first React UI lives in:

- `apps/ao-web`

CLI commands:

```bash
# Serve API + embedded web UI (default host/port)
ao web serve

# Override host/port, serve external built assets, or API only
ao web serve --host 127.0.0.1 --port 4173 --assets-dir apps/ao-web/dist
ao web serve --api-only

# Open browser to a specific route
ao web open --host 127.0.0.1 --port 4173 --path /tasks
```

Web API base path:

- `/api/v1`

Realtime stream:

- `/api/v1/events` (SSE, event name `daemon-event`)
- `requirements.json`

### 2.7 Review / QA / audit trails

```bash
ao review entity --entity-type <type> --entity-id <id>
ao review record --entity-type <type> --entity-id <id> --reviewer-role <role> --decision <approve|request_changes|reject>
ao review task-status --task-id <id>
ao review requirement-status --id <id>
ao review handoff --run-id <id> --target-role <role> --question <text> [--context-json '<json>']
ao review dual-approve --task-id <id>

ao qa evaluate --workflow-id <workflow_id> --phase-id <phase_id> --task-id <task_id> [--worktree-path <path>] [--gates-json '<json>'] [--metrics-json '<json>'] [--metadata-json '<json>']
ao qa get --workflow-id <workflow_id> --phase-id <phase-id>
ao qa list --workflow-id <workflow_id>
ao qa approval add|list

ao history task --task-id <id> [--limit <n>]
ao history get --id <execution-id>
ao history recent [--limit <n>]
ao history search [--task-id <id>] [--workflow-id <id>] [--status <status>]
ao history cleanup [--days <n>]

ao errors list [--category <cat>] [--severity <sev>] [--task-id <id>] [--limit <n>]
ao errors get <id>
ao errors stats
ao errors retry <id>
ao errors cleanup [--days <n>]

ao task-control pause|resume|cancel|set-priority|set-deadline --task-id <id> ...
```

### 2.8 Git helper integration

The CLI includes a lightweight git orchestration layer that keeps repository metadata in project state and supports confirmations for destructive actions.

```bash
ao git repo list|get|init|clone
ao git branches --repo <name>
ao git status --repo <name>
ao git commit --repo <name> --message <text>
ao git push --repo <name> --remote origin --branch main [--force]
ao git pull --repo <name> --remote origin --branch main

ao git worktree create|list|get|remove|pull|push|sync|sync-status

ao git confirm request --operation-type <type> --repo-name <name> [--context-json '<json>']
ao git confirm respond --request-id <id> --approved true|false
ao git confirm outcome --request-id <id> --success true|false --message <text>
```

Confirmation IDs are generated with `git confirm request` and must be supplied for operations that require approval.

### 2.9 Model + runner health

```bash
ao model availability [--model <id:tool> ...]
ao model status --model <model> --cli-tool <tool>
ao model validate --task-id <id>
ao model roster refresh|get

ao model eval run|report

ao runner health
ao runner orphans detect|cleanup
ao runner restart-stats
```

Model availability checks verify binary presence in `PATH` and required API keys.

### 2.10 Output and artifacts retrieval

```bash
ao output run --run-id <id>
ao output artifacts --execution-id <id>
ao output files --execution-id <id>
ao output download --execution-id <id> --artifact-id <id>
ao output jsonl --run-id <id> [--entries]
ao output monitor --run-id <id> [--task-id <id>] [--phase-id <id>]
ao output cli --run-id <id>
```

### 2.11 Health and diagnostics

```bash
ao doctor
```

`doctor` returns a system report and daemon health snapshot.

## 3. Recommended usage patterns

### Minimal flow

```bash
ao project create --name my-app --path . --project-type web-app

ao task create --title "Implement auth" --task-type feature --priority high

ao task create --title "Add tests" --task-type test --priority medium

ao daemon start --max-agents 2

ao workflow run --task-id <task-id>

ao workflow get --id <workflow-id>
ao output monitor --run-id <run-id>
```

### End-to-end execution flow

```bash
ao requirements draft --input-json '{"include_codebase_scan":true,"append_only":true}'
ao requirements refine --id <id> --focus "first milestone"
ao execute run --id <id> --pipeline-id standard
```

## 4. Persistent data and file layout

### Project-scoped

- `<project-root>/.ao/core-state.json` – canonical state for projects, tasks, workflows.
- `<project-root>/.ao/state/*` – command-level stores (history, git metadata, model status, QA, reviews, requirements docs, etc.).
- `<project-root>/.ao/runs/<run-id>/` – live/archived run streams.
- `<project-root>/.ao/workspace?/docs` – planning docs.
- `<project-root>/.ao/artifacts/<execution_id>/` – artifact files.

### Global (protocol config scope)

- `<global config dir>/daemon-events.jsonl` – daemon event log (consumed by `daemon events` and `errors sync`).
- `<global config dir>/projects.json` – multi-project daemon registry.
- `<global config dir>/cli-tracker.json` – runner process tracking for orphan detection.

Global config dir resolution:
- `AO_CONFIG_DIR`
- else `dirs::config_dir()` + `agent-orchestrator` package name.

## 5. Extensibility and debug tips

- Add commands in `crates/orchestrator-cli/src/cli_types.rs` and handler dispatch in:
  - `crates/orchestrator-cli/src/main.rs`
  - `crates/orchestrator-cli/src/services/runtime.rs`
  - `crates/orchestrator-cli/src/services/operations.rs`
- Add execution semantics in matching domain handler files under:
  - `crates/orchestrator-cli/src/services/runtime/*`
  - `crates/orchestrator-cli/src/services/operations/*`

## 6. Additional references

- `docs/cli/ao-reference.md` – compact command reference.
- `crates/orchestrator-cli/tests/cli_smoke.rs` and `cli_e2e.rs` – usage assertions and command contract examples.
