# Quick Start

This guide takes you from a fresh repository to autonomous AI workflows. Animus is built to run continuously with a background daemon that executes work automatically.

## 1. Prepare the Repository

```bash
cd /path/to/your/project
animus doctor
animus init --template task-queue --non-interactive
```

`animus init` is the primary first-run flow. It bootstraps the project-local `.ao/` config, copies the selected template workflow wrappers into the repo, and provisions repo-scoped runtime state under `~/.ao/<repo-scope>/`.

If you are running in a real terminal and want the guided picker instead of an explicit template id, run `animus init`. The bundled first-party templates are:

- `task-queue` for queue-driven delivery with aggressive daemon defaults
- `conductor` for planning-heavy requirement intake and queue execution
- `direct-workflow` for human-driven workflow runs with conservative automation

## 2. Create Your First Task

```bash
animus task create \
  --title "Add rate limiting" \
  --description "Throttle API requests before they hit the upstream provider" \
  --task-type feature \
  --priority high
```

The first task in a repository is typically `TASK-001`.

## 3. Mark the Task Ready and Start the Daemon

```bash
animus task status --id TASK-001 --status ready
animus daemon start --autonomous
```

The daemon now polls for ready tasks and starts workflows automatically. You can let it run in the background.

## 4. Inspect Progress

```bash
animus task stats
animus workflow list
animus daemon status
animus output tail
animus status
```

## Testing a Workflow Before Daemon

If you want to test a workflow definition before running the daemon, use the `--sync` flag to run it synchronously in your terminal:

```bash
animus workflow run --task-id TASK-001 --sync
```

This is useful for debugging workflow definitions, agent prompts, or MCP tools. Once you're satisfied, follow steps 3–4 above to enable autonomous execution.

## Requirement-First Flow

If you want to start from product requirements instead of a direct task:

```bash
animus requirements create \
  --title "Rate limiting" \
  --priority must \
  --acceptance-criterion "Requests above the threshold are delayed or rejected"

animus requirements execute --id REQ-001
```

This materializes implementation tasks and queues them for the daemon to execute.

## Next Steps

- [Project Setup](project-setup.md)
- [A Typical Day](typical-day.md)
- [Workflows](../concepts/workflows.md)
