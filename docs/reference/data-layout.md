# Data Layout

AO stores project state under `.ao/` and machine-scoped pack/runtime data under
`~/.ao/`.

## Project Layout

```text
.ao/
├── config.json
├── core-state.json
├── resume-config.json
├── docs/
├── requirements/
├── tasks/
├── plugins/
├── workflows.yaml
├── workflows/
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

Key points:

- `.ao/workflows.yaml` and `.ao/workflows/*.yaml` are project-authored workflow
  sources
- `.ao/plugins/` contains project-local pack overrides
- `.ao/state/pack-selection.v1.json` tracks active pack pins and enablement
- `.ao/runs/` and `.ao/artifacts/` contain execution records and artifacts

## Machine Layout

### Installed packs

```text
~/.ao/packs/<pack-id>/<version>/
```

### Repo-scoped runtime state

```text
~/.ao/<repo-scope>/worktrees/
```

These stores serve different purposes:

- `~/.ao/packs/` is the machine pack registry
- `~/.ao/<repo-scope>/...` is repo-scoped runtime state such as managed worktrees

## Mutation Policy

Do not hand-edit AO state files. Use AO commands or AO MCP tools unless you are
explicitly working on AO persistence as part of a migration.

## Resolution-Related Paths

| Path | Purpose |
|---|---|
| `.ao/plugins/<pack-id>/` | Project-local pack override root |
| `.ao/workflows.yaml` | Single-file project workflow source |
| `.ao/workflows/*.yaml` | Multi-file project workflow sources |
| `.ao/state/pack-selection.v1.json` | Project pack pin/enablement state |
| `~/.ao/packs/<pack-id>/<version>/` | Machine-installed pack root |

See also: [Configuration](configuration.md), [Workflows](../concepts/workflows.md).
