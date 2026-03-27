# Data Layout

AO splits repository-authored configuration from repo-scoped runtime state.

## Project-Local Layout

These files live in the repository:

```text
.ao/
├── config.json
├── workflows.yaml              # optional single-file workflow source
├── workflows/
│   ├── custom.yaml
│   ├── standard-workflow.yaml
│   ├── hotfix-workflow.yaml
│   └── research-workflow.yaml
└── plugins/
    └── <pack-id>/              # optional project pack override root
```

Key points:

- `.ao/workflows.yaml` and `.ao/workflows/*.yaml` are the authored workflow sources
- `.ao/plugins/<pack-id>/` is the project override root for pack content
- `.ao/config.json` stores repository-local AO config

## Repo-Scoped Runtime Layout

Mutable runtime state lives outside the repo:

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

Key points:

- `workflow.db` stores persisted workflows, tasks, requirements, and checkpoints
- `core-state.json` stores the shared runtime snapshot AO loads at startup
- `config/state-machines.v1.json` stores the effective state-machine document
- `daemon/pm-config.json` stores persisted daemon settings
- `worktrees/` stores managed task worktrees for that repository scope

## Machine-Wide Layout

AO also uses machine-wide directories that are not tied to one repository:

```text
~/.ao/
├── config.json
├── credentials.json
├── daemon-events.jsonl
├── cli-tracker.json
└── packs/
    └── <pack-id>/<version>/
```

## Repository Scope Format

`<repo-scope>` is derived from the canonical project path:

```text
<sanitized-repo-name>-<12-hex-sha256-prefix>
```

This keeps runtime data stable across linked worktrees while avoiding collisions between repositories with the same basename.

## Mutation Policy

Do not hand-edit AO-managed JSON or SQLite state unless you are explicitly working on AO persistence or migrations.

Use AO commands or AO MCP tools instead.

## Resolution-Related Paths

| Path | Purpose |
|---|---|
| `.ao/workflows.yaml` | Single-file project workflow source |
| `.ao/workflows/*.yaml` | Multi-file project workflow sources |
| `.ao/plugins/<pack-id>/` | Project-local pack override root |
| `~/.ao/<repo-scope>/workflow.db` | Persisted workflows, tasks, requirements, checkpoints |
| `~/.ao/<repo-scope>/config/state-machines.v1.json` | Repo-scoped state-machine config |
| `~/.ao/<repo-scope>/state/pack-selection.v1.json` | Repo-scoped pack selection state |
| `~/.ao/packs/<pack-id>/<version>/` | Machine-installed pack root |

See also: [Configuration](configuration.md), [State Management](../concepts/state-management.md), [Project Setup](../getting-started/project-setup.md).
