# Quick Start

This guide takes you from a fresh repository to AO-managed workflows running in
that project.

## 1. Initialize the Project

```bash
cd /path/to/your/project
ao setup
ao pack list
```

`ao setup` creates the project-local `.ao/` scaffold. `ao pack list` shows the
bundled and active pack inventory available to the project.

## 2. Draft a Vision

```bash
ao vision draft
```

This resolves the canonical workflow ref `ao.vision/draft` and saves the vision
artifact through AO-managed state.

## 3. Generate Requirements

```bash
ao requirements draft --include-codebase-scan
```

This resolves `ao.requirement/draft`. Requirement planning is now described as
a workflow surface, with compatibility aliases for the older `builtin/*` refs.

## 4. Materialize Tasks

```bash
ao requirements execute
```

This resolves `ao.requirement/execute`. The workflow creates or updates tasks
through AO mutation surfaces such as `ao.task.create`, not through daemon-owned
business logic.

## 5. Start the Daemon

```bash
ao daemon start --autonomous
```

The daemon handles queueing, capacity, and subprocess supervision only. Task and
requirement behavior comes from workflows, packs, MCP, and subject adapters.

## 6. Monitor Progress

```bash
ao task stats
ao daemon status
ao workflow list
ao output tail
ao status
```

## What Happens Next

Project-local workflows such as `standard-workflow` typically wrap bundled pack
refs like `ao.task/standard`. As work completes, execution facts are projected
back onto AO task and requirement state.

## Next Steps

- [Project Setup](project-setup.md)
- [A Typical Day](typical-day.md)
- [Workflows](../concepts/workflows.md)
