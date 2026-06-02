# Project Setup

## What `animus init` Does

`animus init` is the supported first-run command for a repository.

On first run it:

1. resolves the project root
2. creates `.animus/` if it does not exist
3. provisions repo-scoped runtime state under `~/.animus/<repo-scope>/`
4. writes project config and baseline workflow scaffolding
5. copies template workflow wrappers into `.animus/workflows/`

## Project-Local Files

```text
.animus/
├── config.json
├── plugins/                    # optional project-local plugins and pack overrides
├── workflows.yaml
├── plugins.lock                # appears after project-scoped plugin install/update activity
└── workflows/
    ├── custom.yaml
    ├── standard-workflow.yaml
    ├── hotfix-workflow.yaml
    └── research-workflow.yaml
```

`plugins.lock` is not part of the initial scaffold; plugin commands create it
when the project resolves a local lockfile. `.animus/plugins/` is optional and
serves two roles: project-local plugin discovery and pack override content for
workflow resolution. Templates may add more workflow wrappers or companion
files.

## Repo-Scoped Runtime State

```text
~/.animus/<repo-scope>/
├── core-state.json
├── resume-config.json
├── workflow.db
├── config/
├── daemon/
├── docs/
├── logs/
├── runner/
├── state/
└── worktrees/
```

Some runtime files appear lazily when their subsystem first runs.

## Workflow Sources

Animus resolves workflows from:

1. project overrides in `.animus/plugins/<pack-id>/`
2. project YAML in `.animus/workflows.yaml` and `.animus/workflows/*.yaml`
3. installed packs in `~/.animus/packs/<pack-id>/<version>/`

## Mutation Policy

Do not hand-edit Animus-managed JSON or SQLite state. Use:

- `animus subject ...`
- `animus workflow ...`
- `animus queue ...`
- `animus daemon ...`
- `animus pack ...`
- Animus MCP tools

## Next Steps

- [Quick Start](quick-start.md)
- [A Typical Day](typical-day.md)
- [Data Layout](../reference/data-layout.md)
