# AGENTS.md

Operator and contributor guide for AO (`ao` CLI).

## Mission

Use AO to build AO. Requirements and tasks in this repo are the planning source
of truth, and this workspace stays Rust-only without desktop-wrapper dependencies.

## Workspace Guardrails

- Main binary: `ao` (`crates/orchestrator-cli`).
- Supporting runtime crates must remain healthy:
  `crates/agent-runner`, `crates/llm-cli-wrapper`, `crates/llm-mcp-server`.
- All first-class crates live under `crates/`.
- Do not add or depend on desktop shell frameworks.

## How `ao` Actually Works

`ao` startup flow:

1. Parse CLI args (`--json`, `--project-root`, subcommand).
2. Resolve project root with precedence:
   1. `--project-root`
   2. `PROJECT_ROOT` env var
   3. registry fallback file `~/.config/agent-orchestrator/last-project-root`
   4. current working directory
3. Construct `FileServiceHub` for that root.
4. Dispatch subcommands into:
   - runtime handlers: `daemon`, `agent`, `project`, `task`
   - operations handlers: workflow/planning/review/qa/history/errors/git/model/runner/output/mcp/web/doctor

`FileServiceHub` bootstrap side effects:

- Ensures the project root exists.
- Initializes a git repo if missing.
- Creates an empty initial commit if there is no `HEAD`.
- Creates `.ao/` base structure and core state/config files.

## Data Layout

Repo-local AO state (authoritative for this project):

- `.ao/core-state.json`
- `.ao/config.json`
- `.ao/resume-config.json`
- `.ao/docs/{vision.json,requirements.json,tasks.json}`
- `.ao/requirements/index.json` and `.ao/requirements/generated/*.json`
- `.ao/tasks/index.json` and `.ao/tasks/TASK-*.json`
- `.ao/state/workflow-config.v2.json`
- `.ao/state/agent-runtime-config.v2.json`
- `.ao/state/state-machines.v1.json`
- `.ao/state/{reviews,handoffs,history,errors,qa-results,qa-review-approvals}.json`
- `.ao/runs/<run_id>/events.jsonl` (+ `json-output.jsonl` when available)
- `.ao/artifacts/<execution_id>/...`

Global/shared files (outside repo):

- `protocol::Config::global_config_dir()` (overridable by `AO_CONFIG_DIR`):
  - `projects.json` (daemon registry)
  - `daemon-events.jsonl`
  - runner/global config when scope is global

## Worktree Model

Daemon-managed task worktrees are created under:

- `~/.ao/<repo-scope>/worktrees/`

Where `<repo-scope>` is:

- `<sanitized-repo-name>-<12 hex chars of SHA256(canonical project root)>`

For each repo scope directory:

- `.project-root` contains canonical root path.
- On unix, `project-root` symlink points to canonical project root.

Task defaults:

- worktree name: `task-<sanitized-task-id>`
- branch name: `ao/<sanitized-task-id>`

Example task `TASK-001`:

- worktree: `task-task-001`
- branch: `ao/task-001`

Important distinction:

- `ao git worktree create` uses explicit `--worktree-path`.
- daemon auto-provisioning enforces the managed `~/.ao/<repo-scope>/worktrees` root.

## Runner and CLI Wrapper Path

Agent execution path:

1. `ao agent run` builds runtime context (tool/model/prompt/cwd/runtime_contract).
2. CWD is canonicalized and must remain inside project root.
3. CLI connects to `agent-runner` via unix socket (or TCP on non-unix).
4. Runner executes CLI launch from runtime contract.
5. `llm-cli-wrapper` enforces machine-readable JSON flags for supported AI CLIs.
6. Run events stream back and can be persisted in `.ao/runs/<run_id>/events.jsonl`.

Runner config directory precedence:

1. `AO_RUNNER_CONFIG_DIR`
2. `AO_CONFIG_DIR`
3. `AGENT_ORCHESTRATOR_CONFIG_DIR`
4. scope-based default:
   - `AO_RUNNER_SCOPE=project` (default): `<project>/.ao/runner`
   - `AO_RUNNER_SCOPE=global`: global config dir

## Output and Error Contract

With `--json`, AO uses envelope schema `ao.cli.v1`:

- success: `{ "schema": "ao.cli.v1", "ok": true, "data": ... }`
- error: `{ "schema": "ao.cli.v1", "ok": false, "error": { "code", "message", "exit_code" } }`

Exit code mapping:

- `2` invalid input/parse
- `3` not found
- `4` conflict/already exists
- `5` unavailable/connection/timeout
- `1` internal

## Command Surface (Top Level)

- `daemon`, `agent`, `project`, `task`
- `workflow`, `vision`, `requirements`, `execute`, `planning`
- `review`, `qa`, `history`, `errors`, `task-control`
- `git`, `model`, `runner`, `output`, `mcp`, `web`, `doctor`

## Comprehensive CLI Capability Review

`ao` is a full operations/control-plane CLI for AO and not just a thin wrapper.

1. Core dispatch and output contract

- Entry point is `/Users/samishukri/ao-cli/crates/orchestrator-cli/src/main.rs`.
- Subcommands are defined in `/Users/samishukri/ao-cli/crates/orchestrator-cli/src/cli_types.rs`.
- Exit semantics are centralized in `/Users/samishukri/ao-cli/crates/orchestrator-cli/src/shared/output.rs`:
  - structured success/error envelope when `--json` is enabled
  - explicit exit-code mapping for invalid/not-found/conflict/unavailable/internal paths
- Every call resolves `/Users/samishukri/ao-cli/crates/orchestrator-core/src/lib.rs` service access through `FileServiceHub`.

2. Command-by-command capabilities

- Daemon operations
  - `daemon start`, `daemon run`, `daemon stop`, `daemon status`, `daemon health`
  - `daemon pause`, `daemon resume`, `daemon agents`, `daemon logs`, `daemon clear-logs`
  - `daemon events` includes follow mode and event stream tailing
  - implementation modules include:
    - `/Users/samishukri/ao-cli/crates/orchestrator-cli/src/services/runtime/runtime_daemon.rs`
    - `/Users/samishukri/ao-cli/crates/orchestrator-cli/src/services/runtime/daemon_run.rs`
    - `/Users/samishukri/ao-cli/crates/orchestrator-cli/src/services/runtime/daemon_events.rs`

- Agent lifecycle and control
  - `agent run`, `agent status`, `agent control`, `agent model-status`, `agent runner-status`
  - supports attach/detach run modes, run-id continuity, stream or JSON event output, and optional persisted event/json output logs
  - runtime implementation:
    - `/Users/samishukri/ao-cli/crates/orchestrator-cli/src/services/runtime/runtime_agent.rs`
    - `/Users/samishukri/ao-cli/crates/orchestrator-cli/src/services/runtime/agent/run.rs`
    - `/Users/samishukri/ao-cli/crates/orchestrator-cli/src/services/runtime/agent/status.rs`

- Project commands
  - `project list`, `project active`, `project get`, `project create`, `project load`
  - `project rename`, `project archive`, `project remove`
  - implementation:
    - `/Users/samishukri/ao-cli/crates/orchestrator-cli/src/services/runtime/runtime_project_task/project.rs`

- Task management
  - `task list`, `task prioritized`, `task next`, `task stats`, `task get`
  - `task create`, `task update`, `task delete`
  - `task assign-owner`, `task assign-reviewer`, `task add-checklist-item`, `task remove-checklist-item`
  - `task add-dependency`, `task remove-dependency`, `task set-priority`, `task update` with status/deadline/type/description/title/rules
  - direct control variants via:
    - `/Users/samishukri/ao-cli/crates/orchestrator-cli/src/services/runtime/runtime_project_task/task.rs`
    - `/Users/samishukri/ao-cli/crates/orchestrator-cli/src/services/runtime/runtime_task_control.rs`

- Workflow system
  - `workflow list`, `workflow get`, `workflow decisions`, `workflow checkpoints`
  - `workflow run`, `workflow resume`, `workflow resume-status`, `workflow pause`, `workflow cancel`
  - `workflow phase approve`, `workflow phases`, `workflow pipelines`, `workflow config`
  - `workflow state-machine`, `workflow agent-runtime`, `workflow update-pipeline`
  - migration helper and validation pathways are in:
    - `/Users/samishukri/ao-cli/crates/orchestrator-cli/src/services/operations/ops_workflow.rs`

- Vision and requirements planning
  - `vision draft`, `vision refine`, `vision get`
  - `requirements draft`, `requirements refine`, `requirements list`, `requirements get`
  - `requirements create`, `requirements update`, `requirements delete`
  - `requirements graph`, `requirements mockups`, `requirements recommendations`
  - handlers:
    - `/Users/samishukri/ao-cli/crates/orchestrator-cli/src/services/operations/ops_requirements.rs`
    - `/Users/samishukri/ao-cli/crates/orchestrator-cli/src/services/operations/ops_planning/mod.rs`

- Execute & planning facade
  - `planning vision*` and `planning requirements*` mirror direct command groups for consistency
  - `planning execute`, `execute plan`, `execute run`
  - can drive task generation/execution and records planning history metadata

- Reviews and QA
  - Review: `review entity`, `review record`, `review task-status`, `review requirement-status`, `review handoff`, `review dual-approve`
  - QA: `qa evaluate`, `qa get`, `qa list`, `qa approval add`, `qa approval list`
  - implementations:
    - `/Users/samishukri/ao-cli/crates/orchestrator-cli/src/services/operations/ops_review/handlers.rs`
    - `/Users/samishukri/ao-cli/crates/orchestrator-cli/src/services/operations/ops_qa.rs`

- Audit and observability
  - `history task`, `history get`, `history recent`, `history search`, `history cleanup`
  - `errors list`, `errors get`, `errors stats`, `errors retry`, `errors cleanup`
  - `output run`, `output artifacts`, `output download`, `output files`, `output jsonl`, `output monitor`, `output cli`
  - handlers:
    - `/Users/samishukri/ao-cli/crates/orchestrator-cli/src/services/operations/ops_history.rs`
    - `/Users/samishukri/ao-cli/crates/orchestrator-cli/src/services/operations/ops_errors.rs`
    - `/Users/samishukri/ao-cli/crates/orchestrator-cli/src/services/operations/ops_output.rs`

- Task control and hard state ops
  - `task-control pause`, `task-control resume`, `task-control cancel`
  - `task-control set-priority`, `task-control set-deadline`

- Git and workspace mechanics
  - `git repo`, `git worktree`, `git commit`, `git push`, `git pull`
  - includes explicit confirmation/response flows for destructive operations
  - implementation:
    - `/Users/samishukri/ao-cli/crates/orchestrator-cli/src/services/operations/ops_git`

- Model/tool ecosystem
  - `model availability`, `model status`, `model validate`, `model roster`, `model eval`
  - handler:
    - `/Users/samishukri/ao-cli/crates/orchestrator-cli/src/services/operations/ops_model/handlers.rs`

- Runner health and recovery
  - `runner health`, `runner orphans`, `runner orphans detect`, `runner orphans cleanup`, `runner restart-stats`
  - implementation:
    - `/Users/samishukri/ao-cli/crates/orchestrator-cli/src/services/operations/ops_runner.rs`

- Service-facing entry points
  - `mcp serve` exposes AO tools for external automation
  - `web serve`, `web open`
  - `doctor` for top-level diagnostics
  - handlers:
    - `/Users/samishukri/ao-cli/crates/orchestrator-cli/src/services/operations/ops_mcp.rs`
    - `/Users/samishukri/ao-cli/crates/orchestrator-cli/src/services/operations/ops_web.rs`
    - `/Users/samishukri/ao-cli/crates/orchestrator-cli/src/services/runtime/runtime_doctor.rs`

3. Supporting binaries in the AO ecosystem

- `crates/agent-runner`: long-lived execution engine behind `agent run` and runner status/control.
- `crates/llm-cli-wrapper`: CLI provider discovery and validation.
- `crates/llm-mcp-server`: separate MCP server for external agent registry/service bridging.
- This means `ao` command behavior depends on shared runtime/process surface, not only local file reads/writes.

4. Shared runtime and protocol infrastructure

- Runner connect/protocol and IPC framing helpers:
  - `/Users/samishukri/ao-cli/crates/orchestrator-cli/src/shared/runner.rs`
- JSON parsing and strict enum validation:
  - `/Users/samishukri/ao-cli/crates/orchestrator-cli/src/shared/parsing.rs`
- Structured output, event logging, and stateful task-generation operations are reused across multiple top-level flows.

5. What this gives an operator

- End-to-end flow from vision/requirements -> planning -> workflow/agent execution -> run artifacts -> review/QA -> audit/history.
- Deterministic local project root state with optional daemon/runner orchestration.
- Machine-consumable and human outputs behind the same command surface.
- Explicit recovery tools (restart, orphan cleanup, error retry, output replay, history lookup).
- Extensibility via MCP and web server entry points for external tools/automation.

## Accepted Value Sets

Task status:

- `backlog|todo`, `ready`, `in-progress|in_progress`, `blocked`, `on-hold|on_hold`, `done`, `cancelled`

Task type:

- `feature`, `bugfix`, `hotfix`, `refactor`, `docs`, `test`, `chore`, `experiment`

Priority:

- `critical`, `high`, `medium`, `low`

Requirement priority:

- `must`, `should`, `could`, `wont|won't`

Requirement status:

- `draft`, `refined`, `planned`, `in-progress|in_progress`, `done`

## Repo-Scoped Operating Policy

To keep AO focused on the current repo only:

- Always pass explicit root in scripts/automation:
  - `cargo run -p orchestrator-cli -- --project-root "$(pwd)" ...`
- Run daemon with registry disabled:
- `ao daemon run --include-registry false ...`
  - `ao daemon start --include-registry false ...`
- Treat this repo as the only managed scope for work in this session.

## CLI-Only `.ao` Mutation Policy

`/.ao` is repository state and must be changed through `ao` commands only.

- Required: use `ao vision ...`, `ao requirements ...`, `ao task ...`, `ao workflow ...`, and related `ao` subcommands for all state changes.
- Prohibited: direct manual edits to `/.ao/*.json` (including `core-state`, requirements/task entity files, and state files).
- Exception: explicit migration tooling or code-level persistence changes in AO itself, where file-shape changes are the subject of the task.
- When automating, always pass explicit root:
  - `cargo run -p orchestrator-cli -- --project-root \"$(pwd)\" ...`

## Agent Task Logging and Parallel Work Execution Policy

All agents must treat task records as the source of truth for work performed.

1. Log before work starts

- Create or claim the task in AO before running implementation work.
- Each task must be linked to an existing requirement (`requirements update`/linking flow), or a new requirement must be created first and linked to the task before execution begins.
- Put intent in the task description and acceptance criteria in checklist items.
- Capture ownership and reviewer with `task assign-owner` and `task assign-reviewer`.
- If execution depends on new subtasks, model them explicitly in the task graph (`task add-dependency`).

2. Log during execution

- Advance status transitions through `task status` updates at each meaningful state change (e.g. `in-progress`, `blocked`, `done`).
- Update deadlines and priorities if scope changes (`task set-priority`, `task set-deadline` style task-control commands where used).
- Record blockers, decisions, and important notes in task metadata whenever they affect outcome.
- For each major milestone, add checklist entries so the task carries an auditable completion trail.

3. Log completion and handoff

- On completion, run status update to `done` and leave the final state self-consistent with task/commit artifacts.
- If quality gates are required, use `review` and `qa` flows and reference resulting artifacts in task notes.
- Keep command outputs and evidence discoverable through `output` commands and link relevant artifact paths in task context.
- If a task failed or was paused, log the failure reason and recovery decision explicitly before reactivation.

4. Prefer workflows for parallel multi-agent builds

- Do not run competing manual agent tasks directly against the same mutable workspace path without orchestration.
- Define build/development work as workflow phases so multiple agents can run in parallel:
  - one phase per component/domain (backend, infra, docs, tests, validation, etc.),
  - explicit dependencies between phases,
  - isolated outputs/artifacts per phase.
- Use `workflow run` and `workflow pause/resume/cancel` to control fan-out/fan-in and recovery.
- Use `workflow phase approve` for manual-gated checkpoints between parallel branches when needed.
- Favor `workflow checkpoints` and history/error telemetry to prove each branch outcome before merging/advancing.

5. Why this policy exists

- It prevents silent work and untracked side effects.
- It makes replay and recovery deterministic through task status, workflow state, run artifacts, and history.
- It allows parallel speed-up while preserving correctness, especially on large refactors or multi-package AO changes.

Reference implementation points:

- Task commands and semantics: `/Users/samishukri/ao-cli/crates/orchestrator-cli/src/cli_types.rs`, `/Users/samishukri/ao-cli/crates/orchestrator-cli/src/services/runtime/runtime_project_task/task.rs`, `/Users/samishukri/ao-cli/crates/orchestrator-cli/src/services/runtime/runtime_task_control.rs`
- Workflow execution controls: `/Users/samishukri/ao-cli/crates/orchestrator-cli/src/services/operations/ops_workflow.rs`
- Output/history/audit support: `/Users/samishukri/ao-cli/crates/orchestrator-cli/src/services/operations/ops_output.rs`, `/Users/samishukri/ao-cli/crates/orchestrator-cli/src/services/operations/ops_history.rs`, `/Users/samishukri/ao-cli/crates/orchestrator-cli/src/services/operations/ops_errors.rs`

## Self-Hosting Workflow (Use AO to Build AO)

1. Inspect queue:
   - `ao requirements list`
   - `ao task prioritized`
   - `ao task next`
2. Move selected requirement/task to active:
   - `ao requirements update --id REQ-XXX --status in-progress`
   - `ao task status --id TASK-XXX --status in-progress`
3. Implement and validate (`cargo check`/tests for touched crates).
4. Close loop:
   - `ao task status --id TASK-XXX --status done`
   - `ao requirements update --id REQ-XXX --status done`
5. Keep docs updated whenever command behavior, flags, or state files change.
