# Quick Start

This guide takes you from a fresh repository to autonomous AI workflows. AO is built to run continuously with a background daemon that executes work automatically.

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

## 3. Mark the Task Ready and Start the Daemon

```bash
ao task status --id TASK-001 --status ready
ao daemon start --autonomous
```

The daemon now polls for ready tasks and starts workflows automatically. You can let it run in the background.

## 4. Inspect Progress

```bash
ao task stats
ao workflow list
ao daemon status
ao output tail
ao status
```

## Testing a Workflow Before Daemon

If you want to test a workflow definition before running the daemon, use the `--sync` flag to run it synchronously in your terminal:

```bash
ao workflow run --task-id TASK-001 --sync
```

This is useful for debugging workflow definitions, agent prompts, or MCP tools. Once you're satisfied, follow steps 3–4 above to enable autonomous execution.

## Requirement-First Flow

If you want to start from product requirements instead of a direct task:

```bash
ao requirements create \
  --title "Rate limiting" \
  --priority must \
  --acceptance-criterion "Requests above the threshold are delayed or rejected"

ao requirements execute --id REQ-001
```

This materializes implementation tasks and queues them for the daemon to execute.

## Next Steps

- [Project Setup](project-setup.md)
- [A Typical Day](typical-day.md)
- [Workflows](../concepts/workflows.md)
