# Workflow YAML Schema Reference

Animus workflow YAML is authored in `.animus/workflows.yaml` and `.animus/workflows/*.yaml`.
Those files are merged with installed pack overlays to produce the
effective workflow configuration that `workflow-runner` executes. This document
describes the authored YAML surface.

For the target direction of phase output contracts, universal verdicts, and
YAML-defined phase-local fields, see [Phase Contracts](../architecture/phase-contracts.md).

## Top-Level Structure

A workflow YAML file can contain any combination of these top-level sections:

```yaml
mcp_servers:     # MCP server definitions
agents:          # Agent profile definitions
agent_channels:  # Agent communication channel definitions
phases:          # Reusable phase execution definitions
workflows:       # Named workflow pipelines
```

All sections are optional. Multiple YAML files in `.animus/workflows/` are merged,
and project YAML can override installed pack workflows.

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
| `command` | string | yes | Executable command (e.g., `npx`, `animus`, `python`) |
| `args` | string[] | no | Arguments passed to the command |
| `transport` | string | no | MCP transport protocol (default: stdio) |
| `env` | map\<string, string\> | no | Environment variables for the server process |
| `tools` | string[] | no | Tool name prefixes to allow from this server |
| `config` | map\<string, any\> | no | Arbitrary configuration passed to the server |

### Variable Interpolation

Every string scalar in `.animus/workflows.yaml`, `.animus/workflows/*.yaml`, and
pack-shipped workflow overlays supports shell-style `${VAR}` interpolation. Substitution
runs **before** YAML parsing, so subject backend configs, provider tokens, MCP `env`
blocks, phase env overrides, and any other string field all use the same syntax:

| Form | Meaning |
| --- | --- |
| `${VAR}` | Required. Errors with file path + line number if `VAR` is unset. |
| `${VAR:-default}` | Optional. Falls back to literal `default`. |
| `${VAR:?message}` | Required with custom error message. |
| `$$` | Literal `$`. |

```yaml
mcp_servers:
  hubspot:
    command: npx
    args: ["-y", "@hubspot/mcp-server"]
    env:
      HUBSPOT_ACCESS_TOKEN: "${HUBSPOT_ACCESS_TOKEN}"
      HUBSPOT_BASE_URL: "${HUBSPOT_BASE_URL:-https://api.hubapi.com}"
```

For subject backend and provider config patterns (and guidance on keeping **secrets** out of
YAML), see
[Workflow YAML environment variable interpolation](configuration.md#workflow-yaml-interpolation-non-secret-config).

---

## agents

Declares agent profiles that phases can reference. Each profile specifies the model, tool, and behavioral configuration for an agent.

```yaml
agents:
  <profile_name>:
    name: <string>               # Optional. Display name used in prompts/UI.
    description: <string>        # Optional. Human-readable description.
    system_prompt: |             # Optional. System prompt for the agent.
      You are a code reviewer...
    system_prompt_file: <path>   # Optional. Load system prompt from a UTF-8 file.
    role: <string>               # Optional. Role identifier.
    persona:                     # Optional. Personality/style configuration.
      style: <string>
      traits: [<string>, ...]
      instructions: <string>
      customizations: {}
    memory:                      # Optional. Project-scoped memory behavior.
      enabled: true
      scope: project
      max_context_chars: 6000
      write_policy: explicit
    communication:               # Optional. Project-scoped channel access.
      enabled: true
      channels: [engineering]
      can_message: [reviewer]
      max_context_chars: 8000
    model: <string>              # Optional. Model to use (e.g., claude-sonnet-4-6).
    tool: <string>               # Optional. CLI tool to use (e.g., claude, codex, gemini).
    tool_profile: <string>       # Optional. Named global Claude profile; only valid with tool=claude.
    mcp_servers:                 # Optional. MCP server names this agent can access.
      - "animus"
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
| `name` | string | no | Human-readable display name for this agent |
| `description` | string | no | Human-readable description of the agent |
| `system_prompt` | string | no | System prompt injected into the agent's context |
| `system_prompt_file` | string | no | Path to a UTF-8 file whose contents are inlined into `system_prompt` at compile time; mutually exclusive with `system_prompt` |
| `role` | string | no | Role identifier for the agent |
| `persona` | object | no | Personality/style config injected into the agent's system context |
| `memory` | object | no | Project-scoped memory settings. When enabled, bounded memory entries are injected into phase prompts |
| `communication` | object | no | Channel and direct-message permissions. When enabled, bounded recent channel messages are injected into phase prompts |
| `model` | string | no | LLM model identifier |
| `tool` | string | no | CLI tool to invoke (claude, codex, gemini, etc.) |
| `tool_profile` | string | no | Named global Claude profile to resolve into launch env; only valid for `claude` |
| `mcp_servers` | string[] | no | Names of `mcp_servers` entries this agent can use |
| `skills` | string[] | no | Skill identifiers to attach. Skills resolve from built-ins, `.animus/config/skill_definitions/*.yml`, and Markdown skills such as `.animus/skills/<name>/SKILL.md` or `.animus/skills/<name>.md` |
| `capabilities` | map\<string, bool\> | no | Capability flags |
| `tool_policy` | object | no | Tool access control policy |

Agent profiles defined in YAML are merged into the agent runtime config during compilation. Phase definitions reference agents by profile name.

When prompts get large, prefer `system_prompt_file` over embedding long prose
directly in YAML. Relative paths resolve from the source YAML file's parent
directory, absolute paths are allowed for project YAML, and the file contents
are copied verbatim into the compiled runtime config.

Claude profile references resolve against the user's global Animus config, not the
repository. This keeps account-specific paths such as `CLAUDE_CONFIG_DIR` out
of project files.

---

## agent_channels

Declares project-scoped communication channels for YAML-defined agents.

```yaml
agent_channels:
  engineering:
    description: Implementation coordination
    participants: [architect, implementer, reviewer]
    max_context_chars: 8000
```

Messages are stored under the scoped runtime state directory and can be written
through `animus agent message send` or the MCP tool `animus.agent.message.send`. Agents
only receive channel context when their profile has `communication.enabled:
true` and lists that channel.

| Field | Type | Required | Description |
|---|---|---|---|
| `description` | string | no | Human-readable channel description |
| `participants` | string[] | yes | Agent profile IDs allowed in the channel |
| `max_context_chars` | number | no | Maximum recent channel context injected into prompts |

---

## phases

Declares reusable phase execution definitions. Workflow phase entries reference these definitions by ID.

```yaml
phases:
  implementation:
    mode: agent
    agent: default
    directive: "Implement the change."
    skills:
      - implementation
      - code-review
    runtime:
      tool: claude
      model: claude-sonnet-4-6
      tool_profile: overflow
  code-review:
    mode: agent
    agent: po-reviewer
    skills:
      - code-review
```

### Fields

| Field | Type | Required | Description |
|---|---|---|---|
| `mode` | string | yes | Execution mode: `agent`, `command`, or `manual` |
| `agent` | string | no | Agent profile name to use for the phase |
| `directive` | string | no | Phase-specific instruction appended to the prompt contract |
| `system_prompt` | string | no | Phase-specific system prompt |
| `skills` | string[] | no | Skill identifiers to resolve, validate, and apply at phase runtime. Markdown skills in `.animus/skills` are loaded as prompt-only skills |
| `runtime` | object | no | Tool/model/runtime overrides for the phase |
| `capabilities` | object | no | Structured phase capability flags |
| `output_contract` | object | no | Structured result contract for the phase |
| `output_json_schema` | object | no | Additional JSON schema constraints for the result |
| `decision_contract` | object | no | Structured phase decision contract |
| `retry` | object | no | Retry policy for the phase |
| `command` | object | no | Command execution definition when `mode: command` |
| `manual` | object | no | Manual gate definition when `mode: manual` |
| `default_tool` | string | no | Default tool hint for the phase |

Phase `skills` are validated during config load. At runtime they can inject prompt fragments, model/tool policy overrides, MCP attachments, timeout overrides, launch args/env, and capability overrides. Installed registry skills work the same as local skills when a definition snapshot is present in Animus state.

When `runtime.tool_profile` is set, the effective tool must resolve to
`claude`. Animus looks up the named profile in the user's global config and injects
its environment into the Claude launch contract.

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

## triggers

Event-driven entries that enqueue a workflow when an external event fires.
Triggers live alongside `schedules:` at the top level of workflow YAML and
are processed each daemon tick after the cron block.

```yaml
triggers:
  - id: fswatch-default
    type: plugin
    workflow_ref: review-source-change
    config:
      trigger_id: fswatch-default
      globs: [src/**/*.rs]
      debounce_ms: 250
```

### Fields

| Field | Type | Required | Description |
|---|---|---|---|
| `id` | string | yes | Unique trigger id; must match what a `type: plugin` source emits on `trigger_id` |
| `type` | enum | yes | One of `file_watcher`, `webhook`, `github_webhook`, `plugin` |
| `workflow_ref` | string | yes | Workflow id to enqueue when the trigger fires |
| `enabled` | bool | no | Default `true`. Set `false` to keep the trigger declared but quiet |
| `config` | object | no | Type-specific configuration; forwarded opaquely to plugin triggers |
| `input` | object | no | Static input merged into the spawned workflow run |

### Trigger types

| `type:` | What it is | When to use |
|---|---|---|
| `file_watcher` | Built-in glob watcher inside the project root | Simple filesystem fan-out under the project tree |
| `webhook` | Built-in HTTP webhook listener | Inbound HTTP POSTs into the daemon's HTTP transport |
| `github_webhook` | Built-in GitHub webhook listener with event-shape validation | GitHub pushes, PRs, issues |
| `plugin` | External `trigger_backend` plugin | Anything else: Slack sockets, IDE hooks, custom adapters, cron, IMAP — anything that needs first-party process state |

For `type: plugin`, the `config` block is forwarded opaquely on
`trigger/watch`. The host does not validate its shape; each plugin
documents its own schema. See
[Authoring Trigger Plugins](../guides/authoring-trigger-plugins.md) for
the protocol surface, daemon lifecycle, and how to scaffold a custom
trigger with `animus plugin scaffold trigger <name>`.

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
# .animus/workflows/custom.yaml

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
  animus:
    command: animus
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
