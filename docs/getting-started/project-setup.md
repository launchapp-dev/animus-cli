# Project Setup

## What `ao setup` Does

`ao setup` initializes both the project-local AO config and the repo-scoped runtime state that AO uses while the repository is active.

On first run it:

1. resolves the project root
2. creates `.ao/` if it does not exist
3. provisions repo-scoped state under `~/.ao/<repo-scope>/`
4. writes project config and default workflow YAML scaffolding
5. creates the default state-machine config if it is missing

## Project-Local Files

These files live in the repository and are the authored configuration surface:

```text
.ao/
├── config.json
└── workflows/
    ├── custom.yaml
    ├── standard-workflow.yaml
    ├── hotfix-workflow.yaml
    └── research-workflow.yaml
```

Supported but not created by default:

```text
.ao/workflows.yaml
.ao/plugins/<pack-id>/
```

Use the YAML files in `.ao/workflows/` or `.ao/workflows.yaml` to add repository-specific workflows, override metadata, or wrap bundled pack refs such as `ao.task/standard`.

## Repo-Scoped Runtime State

AO keeps mutable runtime data outside the repository under:

```text
~/.ao/<repo-scope>/
├── core-state.json
├── resume-config.json
├── workflow.db
├── config/
│   └── state-machines.v1.json
├── daemon/
│   └── pm-config.json
├── docs/
│   ├── architecture.json
│   ├── vision.json
│   └── product-vision.md
├── state/
│   ├── pack-selection.v1.json
│   ├── schedule-state.json
│   ├── reviews.json
│   ├── handoffs.json
│   ├── history.json
│   ├── errors.json
│   ├── qa-results.json
│   └── qa-review-approvals.json
└── worktrees/
```

Some of these files appear lazily, only after the corresponding subsystem runs.

## What Lives Where

`workflow.db`
: Stores workflow state plus the persisted task and requirement records.

`core-state.json`
: Stores the shared in-memory snapshot AO loads at startup.

`config/state-machines.v1.json`
: Stores the effective workflow and requirement lifecycle state machines.

`daemon/pm-config.json`
: Stores persisted daemon configuration such as auto-merge and scheduling overrides.

`worktrees/`
: Stores managed task worktrees under the repository scope.

## Workflow Sources

AO resolves workflows from these layers:

1. project overrides in `.ao/plugins/<pack-id>/`
2. project YAML in `.ao/workflows.yaml` and `.ao/workflows/*.yaml`
3. installed packs in `~/.ao/packs/<pack-id>/<version>/`
4. bundled workflow and pack content embedded in AO

## Mutation Policy

Do not hand-edit AO-managed JSON state. Use:

- `ao task ...`
- `ao requirements ...`
- `ao workflow ...`
- `ao pack ...`
- AO MCP tools

## Next Steps

- [Quick Start](quick-start.md)
- [A Typical Day](typical-day.md)
- [Data Layout](../reference/data-layout.md)
