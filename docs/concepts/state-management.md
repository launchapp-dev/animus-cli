# State Management

AO separates authored repository config from mutable runtime state.

## Project-Local `.ao/`

The repository keeps only the configuration you are expected to author:

```text
.ao/
├── config.json
├── workflows.yaml
├── workflows/
└── plugins/
```

These files define workflow behavior, overrides, and local pack customizations.

## Repo-Scoped Runtime State

Runtime state lives under `~/.ao/<repo-scope>/`, not in the repository:

```text
~/.ao/<repo-scope>/
├── core-state.json
├── resume-config.json
├── workflow.db
├── config/
├── daemon/
├── docs/
├── state/
└── worktrees/
```

Important runtime stores:

- `workflow.db` for workflows, checkpoints, tasks, and requirements
- `state/` for review, history, error, schedule, QA, and pack-selection state
- `worktrees/` for managed task worktrees
- `docs/` for generated planning artifacts such as `product-vision.md`

## Why the Split Exists

Keeping mutable state outside the repository gives AO a few important properties:

- linked worktrees resolve back to one shared repo scope
- runtime files do not pollute source control
- large and frequently updated state can evolve without rewriting repo-local config
- legacy `.ao/`-local state can be migrated forward without changing the authored YAML surface

## Pack and Workflow Resolution

AO still resolves workflows from layered sources:

1. project pack overrides in `.ao/plugins/`
2. project YAML in `.ao/workflows.yaml` and `.ao/workflows/*.yaml`
3. installed packs in `~/.ao/packs/`
4. bundled workflow and pack content

State location and workflow resolution are related but different concerns:

- workflow definitions come from YAML and pack content
- execution state and operational records live under `~/.ao/<repo-scope>/`

## Mutation Policy

Do not hand-edit AO-managed runtime JSON or SQLite state unless you are explicitly working on AO persistence or migrations.

Approved mutation surfaces:

- CLI commands such as `ao task status`
- AO MCP tools such as `ao.task.update`
- pack commands such as `ao pack pin`

## Repository Scope

The repo scope uses the canonical project path to derive a stable identifier:

```text
<sanitized-repo-name>-<12-hex-sha256-prefix>
```

This is what lets AO keep one runtime home for a repository even when you invoke it from linked worktrees.
