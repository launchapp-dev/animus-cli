# Quick Start

This guide takes you from a fresh repository to autonomous AI workflows.

## 0. Five-Minute Walkthrough

From your project root:

```bash
cd /path/to/your/project
animus init --walkthrough
```

The walkthrough can install default plugins, copy a starter workflow into
`.animus/workflows/`, and optionally start the daemon.

## 1. Install Plugins

```bash
animus plugin install-defaults --include-subjects --include-transports
animus daemon preflight
```

## 2. Prepare the Repository

```bash
cd /path/to/your/project
animus doctor
animus init --template task-queue --non-interactive
```

This bootstraps `.animus/` in the repo and repo-scoped runtime state under
`~/.animus/<repo-scope>/`.

## 3. Create Your First Task

```bash
animus subject create --kind task \
  --title "Add rate limiting" \
  --body "Throttle API requests before they hit the upstream provider" \
  --priority p1
```

## 4. Mark It Ready and Start the Daemon

```bash
animus subject status --kind task --id task:TASK-001 --status ready
animus daemon start --autonomous
```

If your environment is incomplete and you want the daemon to fix missing
plugins on startup, including the required `workflow_runner` / `queue` roles:

```bash
animus daemon start --autonomous --auto-install
```

## 5. Inspect Progress

```bash
animus subject list --kind task
animus workflow list
animus daemon status
animus status
```

## Test a Workflow Before Running the Daemon

```bash
animus workflow run --task-id TASK-001 --sync
```

## Requirement-First Flow

```bash
animus subject create --kind requirement \
  --title "Rate limiting rollout" \
  --body "Define constraints, acceptance signals, and migration steps." \
  --priority p1
```

## Next Steps

- [Project Setup](project-setup.md)
- [A Typical Day](typical-day.md)
- [Upgrading Animus](../guides/upgrading.md)
- [Workflows](../concepts/workflows.md)
