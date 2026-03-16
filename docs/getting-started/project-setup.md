# Project Setup

## What `ao setup` Does

`ao setup` initializes the repository-scoped AO workspace. It:

1. resolves the project root
2. creates the `.ao/` directory tree
3. scaffolds project-local workflow YAML
4. prepares AO-managed state files
5. leaves bundled workflows and bundled first-party packs available by default

`ao setup` does not copy bundled task or requirement logic into your repo. That
behavior is resolved from bundled sources and pack overlays unless you override
it locally.

## What Gets Created

Typical project-local files after setup:

```text
.ao/
├── config.json
├── core-state.json
├── resume-config.json
├── workflows/
│   ├── custom.yaml
│   ├── standard-workflow.yaml
│   ├── hotfix-workflow.yaml
│   └── research-workflow.yaml
├── plugins/
├── requirements/
├── tasks/
├── docs/
├── runs/
├── artifacts/
└── state/
    ├── pack-selection.v1.json
    ├── state-machines.v1.json
    ├── reviews.json
    ├── handoffs.json
    ├── history.json
    ├── errors.json
    ├── qa-results.json
    └── qa-review-approvals.json
```

### `workflows/`

These are project-local YAML entry points. The default scaffold wraps bundled
pack workflows such as `ao.task/standard` rather than duplicating task logic in
the repository.

### `plugins/`

This is the project override location for pack assets:

```text
.ao/plugins/<pack-id>/
```

Use it when you want a repository-specific override of an installed or bundled
pack.

### `state/pack-selection.v1.json`

This file records project pack pins and enablement state. Manage it through
pack commands, not by editing it directly:

```bash
ao pack list
ao pack inspect --pack-id ao.task
ao pack pin --pack-id ao.task --version =0.1.0
```

## Bundled vs Installed Packs

AO resolves workflows from multiple layers:

1. project overrides in `.ao/plugins/`
2. project YAML in `.ao/workflows.yaml` and `.ao/workflows/*.yaml`
3. installed packs in `~/.ao/packs/<pack-id>/<version>/`
4. bundled kernel workflows and bundled first-party packs

Bundled first-party packs currently own task, requirement, review, and QA
behavior. Canonical refs include:

- `ao.task/standard`
- `ao.requirement/draft`
- `ao.requirement/execute`

Legacy `builtin/*` refs still resolve, but they are compatibility aliases.

## Machine-Scoped Storage

AO also uses machine-scoped directories outside the repo:

- `~/.ao/packs/` for installed packs
- `~/.ao/<repo-scope>/worktrees/` for task worktrees and repo-scoped runtime data

## Mutation Policy

Do not hand-edit `.ao` state files. Use:

- `ao task ...`
- `ao requirements ...`
- `ao workflow ...`
- `ao pack ...`
- AO MCP tools

## Next Steps

- [Quick Start](quick-start.md)
- [A Typical Day](typical-day.md)
- [Workflows](../concepts/workflows.md)
