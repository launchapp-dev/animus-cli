# Requirements Workflow Guide

Requirements now use the same unified subject surface as tasks:
`animus subject ... --kind requirement`.

## Create a Requirement

```bash
animus subject create --kind requirement \
  --title "Add rate limiting" \
  --body "Protect the API from burst traffic and define rollout constraints." \
  --priority p1
```

## Inspect and Update

```bash
animus subject list --kind requirement
animus subject get --kind requirement --id requirement:REQ-001
animus subject update --kind requirement --id requirement:REQ-001 --priority p0
animus subject status --kind requirement --id requirement:REQ-001 --status ready
```

## Execute a Workflow for a Requirement

Use the workflow surface when the requirement itself should drive execution.
A requirement is an ordinary subject kind, so name the workflow explicitly
(there is no requirement-specific default workflow — without a workflow ref the
run uses the project default):

```bash
animus workflow run animus.requirement/plan --subject-id requirement:REQ-001
```

`animus workflow run --subject-id ...` uses the same fallback pattern as
task execution: Animus checks the in-tree requirement store first, then retries
through the active `subject_backend` resolver when the requirement is owned by
a plugin.

Then inspect the resulting workflow state:

```bash
animus workflow list --workflow-ref standard-workflow
animus workflow get --id <workflow-id>
```

## Notes

- The legacy `animus requirements ...` command tree was removed.
- Requirement ids are backend-qualified on the subject surface; use
  `animus subject list --kind requirement --json` to inspect returned ids.
- The exact requirement lifecycle and field set depends on the active
  `subject_backend` plugin for `kind=requirement`.

See also: [Task Management](task-management.md),
[Writing Workflows](writing-workflows.md), and
[MCP Tools Reference](../reference/mcp-tools.md).
