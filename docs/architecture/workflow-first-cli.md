# Workflow-First CLI Architecture

## Status

AO now treats workflow dispatch as the primary execution surface.

- planning commands dispatch canonical workflow refs such as `ao.vision/draft`
  and `ao.requirement/execute`
- task and requirement domain behavior resolves from bundled first-party packs
  and project/local overrides
- legacy `builtin/*` refs remain as compatibility aliases, not as the preferred
  operator-facing surface

## Current Model

Every AI-invoking command follows the same broad shape:

1. build a `SubjectDispatch`
2. select a `workflow_ref`
3. choose sync or async execution
4. let `workflow-runner` resolve the effective workflow from YAML and packs

The CLI is not the place where domain behavior lives. That behavior now belongs
to:

- bundled kernel workflows such as `ao.vision/*`
- bundled first-party packs such as `ao.task` and `ao.requirement`
- installed packs in `~/.ao/packs/`
- project overrides in `.ao/plugins/`
- project-local YAML in `.ao/workflows.yaml` and `.ao/workflows/*.yaml`

## Why This Matters

This keeps the runtime aligned with the plugin-pack kernel design:

- the daemon stays dumb
- workflow refs stay explicit
- behavior remains inspectable as YAML and pack assets
- Node and Python integrations stay subprocess-based

## Canonical Examples

| Command | Canonical Ref |
|---|---|
| `ao vision draft` | `ao.vision/draft` |
| `ao requirements draft` | `ao.requirement/draft` |
| `ao requirements execute` | `ao.requirement/execute` |
| `ao workflow run --ref ao.task/standard` | `ao.task/standard` |

## Related Docs

- [Plugin Pack Kernel](plugin-pack-kernel.md)
- [How AO Works](../concepts/how-ao-works.md)
- [Workflows](../concepts/workflows.md)
