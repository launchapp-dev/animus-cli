# Quick Start

This guide takes you from a fresh repository to autonomous AI workflows.

## 0. Five-Minute Walkthrough

From your project root:

```bash
cd /path/to/your/project
animus init --walkthrough
```

The walkthrough can install default plugins, install the recommended workflow
packs (`animus.core-skills`, `animus.task`, `animus.requirement`,
`animus.review` — offered with default yes, so the documented workflows are
runnable right after init), copy a starter workflow into `.animus/workflows/`,
and optionally start the daemon. If it detects API keys in environment
variables, it suggests `animus secret set <KEY>` to move them into the OS
keychain (see [secrets](../reference/secrets.md)) and offers to store them now
(default no — the keychain is never touched silently).

In scripted runs, opt in to the pack install explicitly:

```bash
animus init --walkthrough --non-interactive --install-packs
```

## 1. Install Plugins

```bash
animus plugin install-defaults
animus daemon preflight
```

One command is enough: `install-defaults` reads the default flavor manifest
(`flavors/default.toml`, bundled in the binary) and installs everything it
marks `required` — provider, subject backends, transport, workflow runner,
and queue — which covers every daemon-preflight role. Add
`--include-recommended` for the full curated bundle (extra providers, the web
UI, the GraphQL transport, more subject backends).

## 2. Prepare the Repository

```bash
cd /path/to/your/project
animus doctor
animus init --template task-queue --non-interactive --install-packs
```

This bootstraps `.animus/` in the repo and repo-scoped runtime state under
`~/.animus/<repo-scope>/`. The `--install-packs` flag also installs and
activates the recommended workflow packs so `animus workflow run
animus.task/standard ...` works immediately; drop it if you manage packs
yourself.

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
animus daemon start
```

If your environment is incomplete and you want the daemon to fix missing
plugins on startup, including the required `workflow_runner` / `queue` roles:

```bash
animus daemon start --auto-install
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
