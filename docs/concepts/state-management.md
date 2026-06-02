# State Management

Animus separates authored repository config from mutable runtime state.

## Project-Local `.animus/`

The repository keeps only the configuration you are expected to author:

```text
.animus/
├── config.json
├── workflows.yaml
├── workflows/
└── plugins/
```

These files define workflow behavior, overrides, and local pack customizations.

## Repo-Scoped Runtime State

Runtime state lives under `~/.animus/<repo-scope>/`, not in the repository:

```text
~/.animus/<repo-scope>/
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
- `logs/` for redacted structured runtime events and run logs
- `runner/` for repo-scoped runner auth and socket state
- `state/` for review, history, error, schedule, QA, and pack-selection state
- `worktrees/` for managed task worktrees
- `docs/` for generated planning artifacts such as `product-vision.md`

## Why the Split Exists

Keeping mutable state outside the repository gives Animus a few important properties:

- linked worktrees resolve back to one shared repo scope
- runtime files do not pollute source control
- large and frequently updated state can evolve without rewriting repo-local config
- legacy `.animus/`-local state can be migrated forward without changing the authored YAML surface

## Pack and Workflow Resolution

Animus still resolves workflows from layered sources:

1. project pack overrides in `.animus/plugins/`
2. project YAML in `.animus/workflows.yaml` and `.animus/workflows/*.yaml`
3. installed packs in `~/.animus/packs/`
4. bundled workflow content compiled into the CLI

State location and workflow resolution are related but different concerns:

- workflow definitions come from YAML and pack content
- execution state and operational records live under `~/.animus/<repo-scope>/`

## Mutation Policy

Do not hand-edit Animus-managed runtime JSON or SQLite state unless you are explicitly working on Animus persistence or migrations.

Approved mutation surfaces:

- CLI commands such as `animus subject status --kind task`
- Animus MCP tools such as `animus.subject.update`
- pack commands such as `animus pack pin`

## Repository Scope

The repo scope uses the canonical project path to derive a stable identifier:

```text
<sanitized-repo-name>-<12-hex-sha256-prefix>
```

This is what lets Animus keep one runtime home for a repository even when you invoke it from linked worktrees.
