# Writing Custom Workflows

AO workflows are defined in YAML and live in `.ao/workflows/`. They describe agents, phases, pipelines, and post-success hooks that the daemon executes against tasks.

For the target design of universal phase verdicts and YAML-defined phase-local
fields, see [Phase Contracts](../architecture/phase-contracts.md).

## File Location

Place workflow files in your project's `.ao/workflows/` directory:

```
.ao/
  workflows/
    custom.yaml
    premium.yaml
```

AO loads all `.yaml` files from this directory. You can split workflows across multiple files or keep everything in one.

## YAML Structure Overview

A workflow file can contain the following top-level sections:

```yaml
mcp_servers:    # External tool integrations
agents:         # Agent definitions (model, tool, system_prompt)
phase_catalog:  # Phase metadata (label, description, category, tags)
phases:         # Phase execution config (mode, directive, command)
workflows:      # Workflow definitions (sequence of phases)
pipelines:      # Reusable pipeline definitions (sequence of phases)
```

## Agents

Agents define which model and tool to use, plus an optional system prompt:

```yaml
agents:
  default:
    model: claude-sonnet-4-6
    tool: claude

  po-reviewer:
    system_prompt: |
      You are a Product Owner reviewing completed development work.
      Your job is to verify that ALL acceptance criteria and requirements
      from the original task are fully met by the implementation.
    model: claude-sonnet-4-6
    tool: claude

  research-agent:
    model: gemini-3.1-pro-preview
    tool: gemini
```

The `tool` field specifies which CLI tool runs the agent: `claude`, `codex`, `gemini`, `opencode`, or `oai-runner`.

## MCP Servers

Declare MCP servers that agents can use as external tools:

```yaml
mcp_servers:
  ao:
    command: ao
    args: ["mcp", "serve"]

  hubspot:
    command: npx
    args: ["-y", "@hubspot/mcp-server"]
    env:
      HUBSPOT_ACCESS_TOKEN: "${HUBSPOT_ACCESS_TOKEN}"
    tools:
      - contacts.search
      - contacts.get
```

Environment variables use `${VAR}` interpolation syntax. The `tools` field is optional and restricts which tools from the server are exposed to agents.

Agents reference MCP servers by name:

```yaml
agents:
  sales-agent:
    model: claude-sonnet-4-6
    tool: claude
    mcp_servers:
      - ao
      - hubspot
```

## Phase Catalog

The phase catalog provides metadata for phases. This is separate from the phase execution config:

```yaml
phase_catalog:
  implementation:
    label: "Implementation"
    description: "Implement production-quality code changes"
    category: development
    tags: [coding, implementation]

  code-review:
    label: "Code Review"
    description: "Review implementation for defects and maintainability"
    category: review
    tags: [review, quality]
```

Categories include: `planning`, `development`, `review`, `verification`.

## Phases

Phases define how work is executed. There are two modes: agent-driven and command-driven.

Longer term, every phase should also participate in the same universal
verdict-driven output model, with YAML declaring any additional phase-specific
fields that the phase emits.

### Agent-Driven Phases

Agent phases dispatch work to an AI agent:

```yaml
phases:
  implementation:
    agent: default
    directive: "Implement the task requirements"
    max_rework_attempts: 3

  code-review:
    agent: po-reviewer
    directive: "Review the implementation"
```

### Command-Driven Phases

Command phases run a CLI command directly:

```yaml
phases:
  unit-test:
    mode: command
    directive: "Run the workspace test suite"
    command:
      program: cargo
      args: ["test", "--workspace"]
      timeout_secs: 600

  lint:
    mode: command
    directive: "Run clippy with deny warnings"
    command:
      program: cargo
      args: ["clippy", "--workspace", "--", "-D", "warnings"]
      timeout_secs: 120
```

## Pipelines

Pipelines are named sequences of phases that can be reused across workflows:

```yaml
pipelines:
  - id: review-cycle
    name: "Review Cycle"
    description: "Reusable code review and testing sequence"
    phases:
      - code-review:
          on_verdict:
            rework:
              target: code-review
      - testing

  - id: quick-fix
    name: "Quick Fix"
    description: "Fast pipeline for small bug fixes"
    phases:
      - implementation
      - testing
```

### Phase Configuration Within Pipelines

Phases in a pipeline support these options:

**`on_verdict` routing** -- When a review phase returns a rework verdict, route back to a target phase:

```yaml
phases:
  - code-review:
      on_verdict:
        rework:
          target: implementation
```

**`skip_if` conditions** -- Skip a phase based on task properties:

```yaml
phases:
  - implementation:
      skip_if:
        - "task_type == docs"
```

**Sub-pipeline references** -- Embed a reusable pipeline:

```yaml
phases:
  - requirements
  - implementation
  - pipeline: review-cycle
```

## Workflows

Workflows tie everything together. They are the top-level units that get dispatched against tasks:

```yaml
workflows:
  - id: requirement-task-generation
    name: "Requirement Task Generation"
    description: "Create or refine requirement-linked AO tasks"
    phases:
      - requirement-task-generation
```

## Post-Success Hooks

Define what happens after a pipeline completes successfully:

```yaml
pipelines:
  - id: full
    name: "Full Lifecycle"
    phases:
      - triage
      - refine-requirements
      - implementation
      - unit-test:
          on_verdict:
            rework:
              target: implementation
      - code-review:
          on_verdict:
            rework:
              target: implementation
      - po-review:
          on_verdict:
            rework:
              target: implementation
    post_success:
      merge:
        strategy: squash
        target_branch: main
        create_pr: true
        auto_merge: true
        cleanup_worktree: true
```

Post-success options:

| Field | Description |
|-------|-------------|
| `merge.strategy` | Merge strategy: `squash`, `merge`, `rebase` |
| `merge.target_branch` | Branch to merge into |
| `merge.create_pr` | Create a pull request |
| `merge.auto_merge` | Automatically merge the PR |
| `merge.cleanup_worktree` | Remove the task worktree after merge |

## Variables

Define variables with defaults that can be overridden at runtime:

```yaml
variables:
  - name: target_branch
    default: main
  - name: review_depth
    default: standard
```

## Complete Example: Code Review Workflow

```yaml
agents:
  reviewer:
    system_prompt: |
      You are a senior code reviewer. Focus on correctness, performance,
      and adherence to the project's coding conventions. Flag any security
      concerns. Use AO MCP tools to update task checklists with findings.
    model: claude-sonnet-4-6
    tool: claude

  implementer:
    model: claude-sonnet-4-6
    tool: claude

phase_catalog:
  implement:
    label: "Implementation"
    category: development
    tags: [coding]
  review:
    label: "Code Review"
    category: review
    tags: [review, quality]
  test:
    label: "Testing"
    category: verification
    tags: [testing]

phases:
  run-tests:
    mode: command
    directive: "Run tests"
    command:
      program: cargo
      args: ["test", "--workspace"]
      timeout_secs: 600

pipelines:
  - id: reviewed-implementation
    name: "Reviewed Implementation"
    description: "Implement, test, review with rework loop"
    phases:
      - implement:
          agent: implementer
      - run-tests:
          on_verdict:
            rework:
              target: implement
      - review:
          agent: reviewer
          on_verdict:
            rework:
              target: implement
    post_success:
      merge:
        strategy: squash
        target_branch: main
        create_pr: true
        auto_merge: false
        cleanup_worktree: true
```

## Tips

- Use `${VAR}` syntax for environment variable interpolation in `env` fields.
- Reuse agents across phases by referencing the same agent name.
- Order phases so that fast-failing checks (lint, tests) run before expensive reviews.
- Keep `max_rework_attempts` reasonable (2-3) to avoid infinite loops.
- Validate your workflow config with `ao workflow config validate`.
- Compile YAML workflows with `ao workflow config compile` to check for errors.
