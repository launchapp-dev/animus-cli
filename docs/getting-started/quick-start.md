# Quick Start

This guide takes you from a fresh install to autonomous AI agents working on your project.

## 1. Configure Your Project

Navigate to your project repository and run the setup wizard:

```bash
cd /path/to/your/project
ao setup
```

This walks you through configuring your tech stack, workflow definitions, MCP servers, and agent profiles. It creates the `.ao/` directory with all necessary configuration and state files.

## 2. Draft a Vision

Generate a vision document that captures what you are building:

```bash
ao vision draft
```

This dispatches the `builtin/vision-draft` workflow. An AI agent analyzes your project context and produces a vision document covering problem statement, target users, goals, constraints, and a complexity assessment.

The output is saved to `.ao/state/vision.json`.

## 3. Generate Requirements

Turn the vision into concrete requirements with acceptance criteria:

```bash
ao requirements draft --include-codebase-scan
```

This dispatches the `builtin/requirements-draft` workflow. The `--include-codebase-scan` flag tells the agent to analyze your existing codebase when generating requirements, ensuring they account for what already exists.

## 4. Create Tasks

Decompose requirements into implementation tasks:

```bash
ao requirements execute
```

This dispatches the `builtin/requirements-execute` workflow. The agent reads the generated requirements and creates tasks (via the `ao.task.create` MCP tool), each linked to its source requirement with priority, type, and dependency information.

## 5. Start the Daemon

Launch autonomous execution:

```bash
ao daemon start --autonomous
```

The daemon begins its tick loop, dequeuing tasks by priority, spawning workflow runners in isolated git worktrees, and processing work through the full pipeline: triage, research, implementation, code review, testing, and acceptance.

## 6. Monitor Progress

Check how things are going:

```bash
# Task completion summary
ao task stats

# Daemon health and status
ao daemon status

# Watch workflows in real time
ao workflow-monitor

# Stream agent output
ao output tail

# Full project dashboard
ao status
```

## What Happens Next

The daemon continues working through the task backlog. Each completed task produces a pull request. Failed workflows trigger rework loops -- the reviewing agent sends the implementing agent back with feedback. If rework is exhausted, the task is marked blocked for your attention.

When the daemon has processed all queued work:

```bash
# List generated pull requests
gh pr list

# Review and merge
gh pr review <number> --approve
gh pr merge <number>
```

## Next Steps

- [Project Setup](project-setup.md) -- Understand the `.ao/` directory structure and configuration options.
- [A Typical Day](typical-day.md) -- See the full workflow lifecycle in practice.
