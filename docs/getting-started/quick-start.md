# Quick Start

This guide takes you from a fresh repository to a running AO workflow using the current CLI surface.

## 1. Prepare the Repository

```bash
cd /path/to/your/project
ao doctor
ao setup
```

`ao setup` creates the project-local `.ao/` config and scaffolds the default workflow YAML files. AO also provisions repo-scoped runtime state under `~/.ao/<repo-scope>/`.

## 2. Create Your First Task

```bash
ao task create \
  --title "Add rate limiting" \
  --description "Throttle API requests before they hit the upstream provider" \
  --task-type feature \
  --priority high
```

The first task in a repository is typically `TASK-001`.

## 3. Run a Workflow Once

```bash
ao workflow run --task-id TASK-001 --sync
```

Use `--sync` when you want the workflow to execute in the current terminal instead of enqueueing it for the daemon.

## 4. Move to Autonomous Execution

If you want the daemon to pick up work automatically, mark tasks ready and start it:

```bash
ao task status --id TASK-001 --status ready
ao daemon start --autonomous
```

## 5. Inspect Progress

```bash
ao task stats
ao workflow list
ao daemon status
ao output monitor
ao status
```

## Requirement-First Flow

If you want to start from product requirements instead of a direct task:

```bash
ao requirements create \
  --title "Rate limiting" \
  --priority must \
  --acceptance-criterion "Requests above the threshold are delayed or rejected"

ao requirements execute --id REQ-001
```

That materializes implementation work and can optionally start follow-on workflows.

## Next Steps

- [Project Setup](project-setup.md)
- [A Typical Day](typical-day.md)
- [Workflows](../concepts/workflows.md)
