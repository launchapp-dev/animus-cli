# Workflow YAML Schema Reference

AO workflows are defined in `.ao/workflows/*.yaml` files. These YAML files are compiled into the effective workflow configuration via `ao workflow config compile`. This document is the formal specification of the YAML format.

For the target direction of phase output contracts, universal verdicts, and
YAML-defined phase-local fields, see [Phase Contracts](../architecture/phase-contracts.md).

## Top-Level Structure

A workflow YAML file can contain any combination of these top-level sections:

```yaml
mcp_servers:     # MCP server definitions
agents:          # Agent profile definitions
variables:       # Variable declarations with defaults
pipelines:       # Named workflow pipelines (collections of phases)
```

All sections are optional. Multiple YAML files in `.ao/workflows/` are merged during compilation.

---

## mcp_servers

Declares external MCP servers that agents can connect to during execution.

```yaml
mcp_servers:
  <server_name>:
    command: <string>           # Required. Binary to execute.
    args: [<string>, ...]       # Optional. Command arguments.
    transport: <string>         # Optional. Transport type (default: stdio).
    env:                        # Optional. Environment variables.
      KEY: "value"
    tools:                      # Optional. Allowed tool name prefixes.
      - "tool.prefix"
    config:                     # Optional. Arbitrary key-value config.
      key: value
```

### Fields

| Field | Type | Required | Description |
|---|---|---|---|
| `command` | string | yes | Executable command (e.g., `npx`, `ao`, `python`) |
| `args` | string[] | no | Arguments passed to the command |
| `transport` | string | no | MCP transport protocol (default: stdio) |
| `env` | map\<string, string\> | no | Environment variables for the server process |
| `tools` | string[] | no | Tool name prefixes to allow from this server |
| `config` | map\<string, any\> | no | Arbitrary configuration passed to the server |

### Variable Interpolation

Environment values support `${VAR}` interpolation from the host environment:

```yaml
mcp_servers:
  hubspot:
    command: npx
    args: ["-y", "@hubspot/mcp-server"]
    env:
      HUBSPOT_ACCESS_TOKEN: "${HUBSPOT_ACCESS_TOKEN}"
```

---

## agents

Declares agent profiles that phases can reference. Each profile specifies the model, tool, and behavioral configuration for an agent.

```yaml
agents:
  <profile_name>:
    description: <string>        # Optional. Human-readable description.
    system_prompt: |             # Optional. System prompt for the agent.
      You are a code reviewer...
    role: <string>               # Optional. Role identifier.
    model: <string>              # Optional. Model to use (e.g., claude-sonnet-4-6).
    tool: <string>               # Optional. CLI tool to use (e.g., claude, codex, gemini).
    mcp_servers:                 # Optional. MCP server names this agent can access.
      - "ao"
      - "hubspot"
    skills:                      # Optional. Skill identifiers.
      - "skill-name"
    capabilities:                # Optional. Boolean capability flags.
      can_write: true
    tool_policy:                 # Optional. Tool access control.
      mode: "allowlist"
      allowed: ["tool.name"]
```

### Fields

| Field | Type | Required | Description |
|---|---|---|---|
| `description` | string | no | Human-readable description of the agent |
| `system_prompt` | string | no | System prompt injected into the agent's context |
| `role` | string | no | Role identifier for the agent |
| `model` | string | no | LLM model identifier |
| `tool` | string | no | CLI tool to invoke (claude, codex, gemini, etc.) |
| `mcp_servers` | string[] | no | Names of `mcp_servers` entries this agent can use |
| `skills` | string[] | no | Skill identifiers to attach |
| `capabilities` | map\<string, bool\> | no | Capability flags |
| `tool_policy` | object | no | Tool access control policy |

Agent profiles defined in YAML are merged into the agent runtime config during compilation. Phase definitions reference agents by profile name.

---

## variables

Declares variables that can be used throughout the workflow. Variables support defaults and can be overridden at runtime via `--input-json`.

```yaml
variables:
  - name: target_branch
    description: "Branch to merge into"
    required: false
    default: "main"
  - name: reviewer
    description: "Assigned reviewer"
    required: true
```

### Fields

| Field | Type | Required | Description |
|---|---|---|---|
| `name` | string | yes | Variable name |
| `description` | string | no | Human-readable description |
| `required` | boolean | no | Whether the variable must be provided (default: false) |
| `default` | string | no | Default value if not provided |

---

## pipelines (Workflow Definitions)

Pipelines define named workflow sequences. Each pipeline is a `WorkflowDefinition` with an ordered list of phases and optional post-success hooks.

A pipeline is defined as a top-level key under `pipelines` (or directly as a workflow definition with `id`, `name`, `description`, `phases`, etc.):

```yaml
# Defining workflows directly at the top level
id: my-workflow
name: My Workflow
description: A workflow that does things
phases:
  - research
  - implementation
  - id: code-review
    agent: po-reviewer
    max_rework_attempts: 3
    on_verdict:
      rework:
        target: implementation
      advance:
        target: testing
      fail:
        target: ""
    skip_if:
      - "task.type == 'hotfix'"
  - testing
post_success:
  merge:
    strategy: merge
    target_branch: main
    create_pr: true
    auto_merge: false
    cleanup_worktree: true
variables:
  - name: target_branch
    default: main
```

### Workflow Definition Fields

| Field | Type | Required | Description |
|---|---|---|---|
| `id` | string | yes | Unique workflow identifier |
| `name` | string | yes | Human-readable workflow name |
| `description` | string | no | Workflow description |
| `phases` | PhaseEntry[] | yes | Ordered list of phase entries |
| `post_success` | PostSuccessConfig | no | Actions to perform after all phases succeed |
| `variables` | Variable[] | no | Variables used by this workflow |

## Phase Output Contracts

Today, workflow YAML supports execution configuration such as `decision_contract`,
`output_contract`, and `output_json_schema`. The intended long-term direction is
to keep YAML as the authored surface while moving toward a simpler phase contract
model:

- every phase emits the same universal verdict-driven envelope
- YAML defines extra phase-local fields and their descriptions
- the runtime composes and validates an effective contract in memory
- users do not manage standalone JSON schema files

See [Phase Contracts](../architecture/phase-contracts.md) for the target model.

### Phase Entry Types

Each entry in the `phases` array can be one of three types:

#### Simple (string)

A bare string referencing a phase definition by ID:

```yaml
phases:
  - research
  - implementation
  - testing
```

#### Rich (object with `id`)

An inline phase configuration with routing, rework limits, and conditional skipping:

```yaml
phases:
  - id: code-review
    agent: po-reviewer
    max_rework_attempts: 3
    system_prompt_override: "Focus on security"
    skip_if:
      - "task.type == 'docs'"
    on_verdict:
      rework:
        target: implementation
      advance:
        target: testing
      fail:
        target: ""
```

| Field | Type | Required | Description |
|---|---|---|---|
| `id` | string | yes | Phase definition ID to execute |
| `agent` | string | no | Agent profile name to use for this phase |
| `max_rework_attempts` | integer | no | Maximum rework loops before failing (default: 3) |
| `system_prompt_override` | string | no | Override the agent's system prompt for this phase |
| `skip_if` | string[] | no | Conditions under which to skip this phase |
| `on_verdict` | map\<string, TransitionConfig\> | no | Routing rules keyed by verdict name |

#### SubWorkflow (object with `workflow_ref`)

Embeds another workflow definition as a nested sub-workflow:

```yaml
phases:
  - workflow_ref: hotfix-pipeline
```

| Field | Type | Required | Description |
|---|---|---|---|
| `workflow_ref` | string | yes | ID of the workflow definition to embed |

### on_verdict Routing

The `on_verdict` map controls what happens after a phase produces a decision. Keys are verdict names, values are transition configs:

```yaml
on_verdict:
  rework:
    target: implementation     # Go back to implementation phase
  advance:
    target: testing            # Proceed to testing phase
  fail:
    target: ""                 # Terminate the workflow
  skip:
    target: deployment         # Jump to deployment
```

| Verdict | Description |
|---|---|
| `rework` | Phase needs rework; route to the specified target phase |
| `advance` | Phase succeeded; proceed to the specified target phase |
| `fail` | Phase failed fatally; terminate or route to error handling |
| `skip` | Phase should be skipped; jump to the specified target |

Each transition has:

| Field | Type | Required | Description |
|---|---|---|---|
| `target` | string | yes | Phase ID to transition to (empty string = terminate) |
| `guard` | string | no | Optional guard condition for the transition |

### post_success

Actions to perform after all phases complete successfully:

```yaml
post_success:
  merge:
    strategy: merge            # merge, squash, or rebase
    target_branch: main        # Branch to merge into
    create_pr: true            # Create a pull request
    auto_merge: false          # Auto-merge the PR
    cleanup_worktree: true     # Remove the worktree after merge
```

| Field | Type | Default | Description |
|---|---|---|---|
| `merge.strategy` | string | `"merge"` | Git merge strategy: `merge`, `squash`, or `rebase` |
| `merge.target_branch` | string | `"main"` | Target branch for the merge |
| `merge.create_pr` | boolean | `false` | Whether to create a pull request |
| `merge.auto_merge` | boolean | `false` | Whether to auto-merge the PR |
| `merge.cleanup_worktree` | boolean | `true` | Whether to remove the worktree after merge |

---

## PhaseDecision

When a phase completes, the agent (or automated system) produces a `PhaseDecision`:

| Field | Type | Description |
|---|---|---|
| `kind` | string | Decision type identifier |
| `phase_id` | string | The phase that produced this decision |
| `verdict` | string | One of: `advance`, `rework`, `fail`, `skip` |
| `confidence` | float | Confidence score (0.0 to 1.0) |
| `risk` | string | Risk level of the decision |
| `reason` | string | Human-readable explanation |
| `evidence` | string[] | Supporting evidence for the decision |
| `target_phase` | string? | Explicit target phase (overrides on_verdict routing) |

---

## Complete Annotated Example

```yaml
# .ao/workflows/custom.yaml

# Agent profiles
agents:
  default:
    model: claude-sonnet-4-6
    tool: claude

  po-reviewer:
    system_prompt: |
      You are a Product Owner reviewing completed development work.
      Verify that ALL acceptance criteria are fully met.
    model: claude-sonnet-4-6
    tool: claude

  requirements-refiner:
    system_prompt: |
      You are a requirements analyst. Take vague task descriptions
      and refine them into well-specified, testable acceptance criteria.
    model: claude-sonnet-4-6
    tool: claude

# MCP server integrations
mcp_servers:
  ao:
    command: ao
    args: ["mcp", "serve"]

# Workflow: standard development pipeline
id: default
name: Default Pipeline
description: Standard development workflow with research, implementation, and review
phases:
  # Phase 1: Research the codebase
  - research

  # Phase 2: Implement the solution
  - implementation

  # Phase 3: Review with rework routing
  - id: code-review
    agent: po-reviewer
    max_rework_attempts: 3
    on_verdict:
      rework:
        target: implementation
      advance:
        target: testing

  # Phase 4: Run tests
  - testing

post_success:
  merge:
    strategy: squash
    target_branch: main
    create_pr: true
    auto_merge: false
    cleanup_worktree: true

variables:
  - name: target_branch
    default: main
```

See also: [Configuration](configuration.md), [Status Values](status-values.md).
