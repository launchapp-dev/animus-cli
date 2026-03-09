# Project Setup

## What `ao setup` Does

The `ao setup` command is an interactive wizard that initializes AO for your project. It:

1. Detects your project root (git repository root)
2. Prompts for tech stack, language, and framework details
3. Configures workflow definitions (builtin + task workflows)
4. Sets up MCP server references
5. Configures agent profiles and model preferences
6. Creates the `.ao/` directory tree with all necessary files

## The `.ao/` Directory

After setup, your project root contains a `.ao/` directory with this structure:

```
.ao/
├── workflows/                  # Workflow YAML definitions
│   ├── builtin/                # Planning workflows (shipped with AO)
│   │   ├── vision-draft.yaml
│   │   ├── vision-refine.yaml
│   │   ├── requirements-draft.yaml
│   │   ├── requirements-refine.yaml
│   │   └── requirements-execute.yaml
│   ├── standard-workflow.yaml  # Default task workflow
│   ├── hotfix-workflow.yaml    # Fast-track workflow
│   └── research-workflow.yaml  # Research-only workflow
│
├── state/                      # Runtime state (managed by AO)
│   ├── vision.json             # Vision document
│   ├── requirements.json       # Requirements with acceptance criteria
│   ├── tasks.json              # Task registry
│   ├── agent-runtime-config.v2.json  # Agent profiles and model routing
│   └── ...                     # Other state files
│
├── daemon.log                  # Daemon log output
└── daemon.log.1                # Rotated log (at 10MB)
```

### `workflows/`

Contains all YAML workflow definitions. AO distinguishes three categories:

- **Builtin workflows** (`builtin/`): Planning-lifecycle workflows that ship with AO. These handle vision drafting, requirements generation, and task creation.
- **Task workflows**: Workflows assigned to tasks (e.g., `standard-workflow`, `hotfix-workflow`). These define the phase pipeline an agent follows to implement a task.
- **Custom workflows**: Any workflow you define for your own purposes (incident response, lead qualification, nightly CI, etc.).

### `state/`

Runtime state managed exclusively by AO commands and MCP tools. These are JSON files that track vision, requirements, tasks, workflow execution state, and agent configuration.

**Do not hand-edit files in `.ao/state/`.** Always use `ao` commands or MCP tools to modify state. Direct edits bypass validation and can corrupt state.

## Repository Scoping

AO scopes runtime data to each repository. Worktrees and execution artifacts live at:

```
~/.ao/<repo-scope>/worktrees/<task-id>/
```

The `<repo-scope>` is derived from your repository's git remote URL or directory path. This means multiple clones of the same repository share the same AO state scope.

Each task gets its own isolated git worktree, so agents can write code, run tests, and commit without interfering with each other or your working copy.

## Global Configuration

Machine-wide AO configuration lives at:

```
~/.config/agent-orchestrator/
```

This includes global preferences, default model settings, and credential references. Project-level configuration in `.ao/` takes precedence over global settings.

## Agent Runtime Configuration

The agent runtime config at `.ao/state/agent-runtime-config.v2.json` controls:

- **Agent profiles**: Default model and tool for each agent persona
- **Model routing**: Which model handles which workflow phase
- **Capacity settings**: Max concurrent workflows, slot headroom

The config cascade for model selection is:

1. Phase-level `runtime.model` override (highest priority)
2. Agent profile `model` field
3. Compiled defaults in `protocol/src/model_routing.rs` (lowest priority)

Set agent profile fields to `null` to fall through to compiled defaults.

## Project Root Override

In scripts and automation, always specify the project root explicitly:

```bash
ao --project-root "$(pwd)" task list
```

AO also reads the `PROJECT_ROOT` environment variable as a fallback.

## Next Steps

- [Quick Start](quick-start.md) -- Run your first autonomous workflow.
- [A Typical Day](typical-day.md) -- See the full lifecycle in action.
