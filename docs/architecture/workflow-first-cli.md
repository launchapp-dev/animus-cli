# Workflow-First CLI Architecture

## Status

Animus now treats workflow dispatch as the primary execution surface.

- workflow and requirements surfaces dispatch canonical workflow refs such as
  `animus.task/standard` and `animus.requirement/execute`
- task and requirement domain behavior resolves from installable first-party packs
  and project-local overrides
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

- kernel baseline config for shared phase and MCP defaults
- installable first-party packs such as `animus.task` and `animus.requirement`
- installed packs in `~/.animus/packs/`
- project overrides in `.animus/plugins/`
- project-local YAML in `.animus/workflows.yaml` and `.animus/workflows/*.yaml`

## Why This Matters

This keeps the runtime aligned with the plugin-pack kernel design:

- the daemon stays dumb
- workflow refs stay explicit
- behavior remains inspectable as YAML and pack assets
- Node and Python integrations stay subprocess-based

## Canonical Examples

| Command | Canonical Ref |
|---|---|
| `animus workflow run animus.requirement/draft --title "..." --sync` | `animus.requirement/draft` |
| `animus workflow run animus.requirement/execute --subject-id requirement:REQ-001` | `animus.requirement/execute` |
| `animus workflow run animus.task/standard --subject-id TASK-001` | `animus.task/standard` |

## Related Docs

- [Plugin Pack Kernel](plugin-pack-kernel.md)
- [How Animus Works](../concepts/how-ao-works.md)
- [Workflows](../concepts/workflows.md)
