# Guides

Practical walkthroughs for day-to-day Animus operations.

## Planning and Requirements

- **[Requirements Workflow](requirements-workflow.md)** -- From vision drafting through requirement decomposition into actionable tasks.

## Task and Execution

- **[Task Management](task-management.md)** -- Creating, prioritizing, assigning, and tracking tasks through their full lifecycle.
- **[Writing Custom Workflows](writing-workflows.md)** -- YAML workflow authoring: agents, phases, pipelines, MCP servers, and post-success hooks.
- **[Self-Hosting Workflow](self-hosting.md)** -- How Animus tracks its own development through `animus` commands.

## Operations

- **[Daemon Operations](daemon-operations.md)** -- Starting, stopping, pausing, configuring, and monitoring the autonomous daemon.
- **[Operator Runbook](operator-runbook.md)** -- Production healthchecks, common failure scenarios with remediations, debug procedures, backup/restore, and disaster recovery.
- **[Model Routing](model-routing.md)** -- How Animus selects models and tools per phase, and how to override defaults.
- **[Web Dashboard](web-dashboard.md)** -- Launching the web UI, navigating boards, and using the REST API.

## MCP & Agent Integration

- **[Working with Animus via MCP Tools](agents.md)** -- Complete guide to all 81 built-in MCP tools: JSON examples, common workflows, sequencing tips, pagination, and batch operations.

## Extending Animus

- **[Plugin Author Guide](plugin-author-guide.md)** -- Write providers, subject backends, trigger backends, and transports. Stdio JSON-RPC contract, scaffolding, testing with `animus-plugin-testkit`, cosign signing, and publishing.
- **[Full System Architecture](../architecture/full-system-architecture.md)** -- End-to-end architecture across crates, processes, state, daemon, runners, plugins, control surfaces, and verification.
- **[Plugin System Architecture](../architecture/plugin-system.md)** -- Discovery, install state, wire protocol, hosting, security, and runtime plugin paths.

## Infrastructure

- **[CI/CD](ci-cd.md)** -- CI workflows, release pipelines, build commands, and test targets.
- **[Upgrading Animus](upgrading.md)** -- General upgrade procedure, plugin updates, and rollback. Links to per-version migration guides.
- **[Troubleshooting](troubleshooting.md)** -- Common issues, diagnostics, and fixes.
