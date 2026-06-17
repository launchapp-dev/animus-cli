# Getting Started

This section walks you from zero to a running Animus project with autonomous AI agents executing workflows continuously.

Animus is designed for **autonomous execution**: you define work and the daemon runs it automatically, 24/7. Single synchronous runs (`--sync`) are useful for debugging, not the primary use case.

The happy-path onboarding sequence, from install to watching workflows run:

```mermaid
flowchart LR
    INSTALL["Install animus"]
    --> INIT["animus init (install + activate packs/plugins)"]
    --> SUBJECT["Create a subject (animus subject create)"]
    --> READY["Mark it ready"]
    --> DAEMON["animus daemon start (runs preflight)"]
    --> RUN["Daemon runs the workflow"]
    --> OBSERVE["Observe (animus status)"]
```

## Pages

- **[Installation](installation.md)** -- Install the CLI, verify `animus`, and confirm your local AI coding tools are available.
- **[Quick Start](quick-start.md)** -- Initialize a repo, create a task, start the daemon, and watch workflows run autonomously.
- **[Project Setup](project-setup.md)** -- Understand the split between project-local `.animus/` config and repo-scoped runtime state.
- **[A Typical Day](typical-day.md)** -- End-to-end walkthrough of creating requirements, materializing tasks, and letting the daemon execute automatically.
