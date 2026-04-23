# Project Setup

## What `animus init` Does

`animus init` is the primary first-run command for a repository. It initializes both the project-local Animus config and the repo-scoped runtime state that Animus uses while the repository is active, then applies a bundled or local project template on top of that bootstrap.

On first run it:

1. resolves the project root
2. creates `.ao/` if it does not exist
3. provisions repo-scoped state under `~/.ao/<repo-scope>/`
4. writes project config and baseline workflow scaffolding
5. copies template workflow wrappers into `.ao/workflows/`
6. creates the default state-machine config if it is missing

`animus setup` still exists as a lower-level bootstrap wizard. Use it when you only want the baseline repo wiring without selecting a template.

## Project-Local Files

These files live in the repository and are the authored configuration surface. The exact workflow set depends on the selected template:

```text
.ao/
├── config.json
└── workflows/
    ├── custom.yaml
    ├── standard-workflow.yaml
    ├── hotfix-workflow.yaml
    ├── research-workflow.yaml
    └── conductor-workflow.yaml   # template-specific example
```

Supported but not created by default:

```text
.ao/workflows.yaml
.ao/plugins/<pack-id>/
```

Use the YAML files in `.ao/workflows/` or `.ao/workflows.yaml` to define repository-specific workflows and defaults. Bundled templates ship curated workflow wrappers, and local templates can add their own starter files the same way.

## Repo-Scoped Runtime State

Animus keeps mutable runtime data outside the repository under:

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

Animus resolves workflows from these layers:

1. project overrides in `.ao/plugins/<pack-id>/`
2. project YAML in `.ao/workflows.yaml` and `.ao/workflows/*.yaml`
3. installed packs in `~/.ao/packs/<pack-id>/<version>/`

## Mutation Policy

Do not hand-edit Animus-managed JSON state. Use:

- `animus task ...`
- `animus requirements ...`
- `animus workflow ...`
- `animus pack ...`
- Animus MCP tools

## Next Steps

- [Quick Start](quick-start.md)
- [A Typical Day](typical-day.md)
- [Data Layout](../reference/data-layout.md)
