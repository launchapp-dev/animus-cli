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
schedules:       # Cron-driven workflow dispatch
triggers:        # Event-driven workflow dispatch
daemon:          # Daemon-wide runtime tuning
secrets:         # Declarative secret references (v0.5.5+)
```

All sections are optional. Multiple YAML files in `.animus/workflows/` are merged,
and project YAML can override installed pack workflows.

---

## secrets (v0.5.5+)

Declares logical secret names backed by process environment variables.
Reference them in any YAML scalar with `${secret.<name>}` — resolution
happens at config-compile time, with the file path and line number
included in any error.

```yaml
secrets:
  linear_token:
    env: LINEAR_API_TOKEN
    required: true                # default; compile-fails if unset
    description: Linear GraphQL token
  optional_pat:
    env: OPTIONAL_GITHUB_PAT
    required: false               # resolves to empty string when unset

mcp_servers:
  linear:
    command: linear-mcp
    env:
      LINEAR_API_TOKEN: "${secret.linear_token}"
```

### Resolution semantics

- The mapped env var is read at compile time. Required-but-unset env vars
  fail the compile.
- Referencing an undeclared key (`${secret.unknown}`) fails the compile.
- The compiled `workflow-config.v2.json` contains the *resolved string*
  — plugins consume the same scalar shape they always did. There is no
  runtime secret-store indirection.

### Sensitive-interpolation lint

When a workflow YAML contains `${VAR}` whose name matches
`TOKEN|KEY|SECRET|PASSWORD` (case-insensitive) and the reference is
**not** inside the `secrets:` block or a `*_env:` declaration field
(which name env vars rather than interpolate their values), the
compiler emits a warning to stderr. This is a hint to move the value
under `secrets:`; it does not fail the compile, since trusted
workflows may have legitimate uses for direct env-var references.

---

## worktree (v0.5.5+)

Controls whether the workflow runner creates a fresh git worktree for
the phase. Available at the workflow level (under a workflow
definition) and at the phase level (where it overrides the workflow
default).

```yaml
phases:
  doc-update:
    mode: agent
    agent: writer
    directive: "Update docs."
    worktree: skip                 # short-form scalar

workflows:
  - id: standard
    phases: [requirements, implementation, code-review]
    worktree:
      mode: auto                   # auto | required | skip
      cleanup: true                # remove worktree on success (default true)
      base_ref: main               # branch to fork from (default: project default)
```

### Modes

- `auto` (default) — create a worktree when the subject implies write
  work. Matches the historical, always-on behavior.
- `required` — always create a worktree; fail-fast if creation fails.
  Use this when the phase **must** be isolated from the project root.
- `skip` — never create a worktree; the phase runs in the project
  root. Use this for read-only / report-only phases.

A phase-level `worktree:` always overrides the workflow-level value.
The short-form `worktree: skip` expands to `{ mode: skip, cleanup: true, base_ref: null }`.

### Runtime split

The kernel **parses, validates, and surfaces** the `worktree:` block on
the compiled workflow config. Actual worktree creation is owned by the
installed workflow runner plugin (`launchapp-dev/animus-workflow-runner-default`
v0.4.0+). Older runners that don't yet understand the field treat it
as `auto`; upgrade the runner plugin via `animus plugin install-defaults`
to pick up `required` and `skip` enforcement.

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

### HTTP transport with OAuth (v0.5.5)

For HTTP-transport MCP servers (`transport: http` + `url:`), Animus can resolve
an OAuth bearer token at contract-assembly time and inject it as an
`Authorization: Bearer <token>` header into the additional MCP server entry
passed to the agent. This lets `codex`, `gemini`, `opencode`, and any other
HTTP-MCP-aware tool talk to OAuth-gated MCP endpoints without each tool
implementing its own auth flow.

```yaml
mcp_servers:
  robinhood-trading:
    transport: http
    url: https://agent.robinhood.com/mcp/trading
    oauth:
      flow: client_credentials       # client_credentials | refresh_token | manual_bearer
      token_url: https://auth.robinhood.com/token
      client_id_env: ROBINHOOD_CLIENT_ID
      client_secret_env: ROBINHOOD_CLIENT_SECRET
      scopes: [trade.read, trade.write]
      audience: https://api.robinhood.com   # optional
      cache: true                            # optional, default true
```

| Field | Required for | Description |
| --- | --- | --- |
| `flow` | always | `client_credentials`, `refresh_token`, or `manual_bearer`. |
| `token_url` | `client_credentials`, `refresh_token` | OAuth token endpoint. Must be `http://` or `https://`. |
| `client_id_env` | `client_credentials` | Env var name holding the client id. |
| `client_secret_env` | `client_credentials` | Env var name holding the client secret. |
| `refresh_token_env` | `refresh_token` | Env var name holding the initial refresh token. |
| `bearer_env` | `manual_bearer` | Env var name holding a pre-baked bearer token. |
| `scopes` | optional | OAuth scopes, joined with spaces and sent as `scope=`. |
| `audience` | optional | Auth0-style `audience=` parameter. |
| `cache` | optional | When `false`, the on-disk token cache is bypassed. Default `true`. |

Tokens are cached under `~/.animus/<repo-scope>/mcp-oauth-cache/<server>.json`
with `0600` permissions on Unix and a 60-second skew margin before the
recorded `expires_at`. Setting `cache: false` skips the cache entirely.

**Flows:**

- **`client_credentials`** — POSTs `grant_type=client_credentials` plus the
  resolved `client_id` / `client_secret` (and optional `scope` / `audience`)
  to `token_url`. The returned `access_token` is cached until 60 seconds
  before its `expires_in`.
- **`refresh_token`** — POSTs `grant_type=refresh_token` plus the cached or
  env-var refresh token (and any optional `client_id` / `client_secret`)
  to `token_url`. If the response returns a rotated `refresh_token`, the
  cache file is updated so the next phase uses the new token; the original
  env var stays unchanged (so the env-var seed only kicks in on a cold cache).
- **`manual_bearer`** — Reads `bearer_env` directly. No network call, no
  refresh, no expiry. The escape hatch for tokens minted by an external
  system.

**Failure modes:** A failed token fetch logs a structured warning and emits
the MCP entry **without** the bearer header — the downstream HTTP MCP call
then surfaces the auth error directly to the user instead of the kernel
silently dropping the server. Token text never appears in logs.

**Validation:** `oauth` is only valid with `transport: http`. Missing
required env-var pointers or `token_url` fail the configuration compile
with a path-qualified error (e.g., `mcp_servers['svc'].oauth.client_id_env
is required for flow="client_credentials"`).

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

---

## schedules

Declares cron-driven dispatchers that enqueue a workflow on a recurring
schedule. Each entry compiles into a `WorkflowSchedule` and is evaluated once
per daemon scheduler tick.

```yaml
schedules:
  - id: nightly-housekeeping
    cron: "0 2 * * *"
    workflow_ref: housekeeping
    input: { include_archived: true }
  - id: hourly-dispatch
    cron: "0 * * * *"
    workflow_ref: dispatch-batch
  - id: weekday-report
    cron: "0 9 * * 1-5"
    workflow_ref: send-report
    enabled: true
```

### Fields

| Field | Type | Required | Description |
|---|---|---|---|
| `id` | string | yes | Unique identifier within the project; used for state tracking |
| `cron` | string | yes | Standard 5-field cron expression evaluated in UTC |
| `workflow_ref` | string | yes | ID of the workflow to enqueue when the schedule fires |
| `input` | object | no | Structured JSON forwarded as the spawned workflow's input |
| `enabled` | boolean | no | Whether the schedule is active (default: `true`) |

The `command:` field still exists on the underlying `WorkflowSchedule` struct
for back-compat, but the current config validator rejects schedules that use it
(`schedules['<id>'].command is no longer supported; use workflow_ref`). Wrap any
shell work you need in a workflow whose phase runs `mode: command` and point
the schedule at that workflow instead.

### Runtime semantics

- Schedules are evaluated each daemon scheduler tick (default 5s, configurable
  via the persisted daemon project config; see [`daemon`](#daemon) below).
- Missed runs (daemon down at fire time) are **not** replayed when the daemon comes
  back up — schedules fire forward-only from the next tick that matches the cron
  expression.
- Per-schedule activity is tracked under the scoped runtime state in
  `ScheduleRunState` with three counters:
  - `last_run` — UTC timestamp of the most recent dispatch attempt. Updated
    when the schedule fires AND the daemon got far enough to invoke the
    spawn — including spawn failures other than tick-budget exhaustion (this
    prevents the same minute from retrying every tick).
  - `run_count` — total dispatch attempts since project init (successes and
    non-budget spawn failures both increment it).
  - `missed_count` — increments only when the per-tick budget rejected the
    spawn slot; `last_run` is **not** updated in that case so the schedule
    gets another shot on the next tick within the same cron minute. Ticks
    skipped outside `active_hours` do not touch either counter — the whole
    schedule branch is bypassed.

See `crates/orchestrator-core/src/services/schedule_state.rs` for the on-disk
schema.

---

## triggers

Declares event-driven dispatchers that enqueue a workflow when an external event
fires. Each entry compiles into a `WorkflowTrigger` and is processed each daemon
tick after the cron schedule block.

```yaml
triggers:
  - id: docs-rebuild
    type: file_watcher
    workflow_ref: docs-build
    config:
      paths: ["docs/**/*.md"]
      debounce_secs: 10
      ignore: ["docs/_drafts/**"]
  - id: github-push
    type: github_webhook
    workflow_ref: ci-pipeline
    config:
      secret_env: GITHUB_WEBHOOK_SECRET
      max_triggers_per_minute: 30
  - id: slack-inbound
    type: plugin
    workflow_ref: triage
    # `config:` is accepted but not currently forwarded to plugins; see below.
```

### Common fields

| Field | Type | Required | Description |
|---|---|---|---|
| `id` | string | yes | Unique trigger identifier within the project |
| `type` | string | yes | Event source: `file_watcher`, `webhook`, `github_webhook`, or `plugin` |
| `workflow_ref` | string | yes | Workflow to enqueue when the trigger fires. Optional in the struct but validation rejects triggers that omit it (`triggers['<id>'] must define workflow_ref`) |
| `enabled` | boolean | no | Whether the trigger is active (default: `true`) |
| `config` | object | depends on `type` | Type-specific configuration. Required for `file_watcher` (must supply `paths`) and `webhook`/`github_webhook` (must supply `max_triggers_per_minute > 0` — see note below). Optional for `plugin` |
| `input` | object | no | Structured JSON forwarded as the spawned workflow's input |

**Webhook config caveat:** for webhook/github_webhook triggers you must supply
an explicit `config:` block — even an empty `config: {}` is fine, because
serde then applies the per-field default of `max_triggers_per_minute = 10`.
Omitting `config:` entirely makes the field default to `Value::Null`, which
the parser falls back through `WebhookTriggerConfig::default()` to
`max_triggers_per_minute = 0`, and the config validator rejects that
(`triggers['<id>'].config.max_triggers_per_minute must be greater than zero`).

### `file_watcher` config

Watches local filesystem paths and fires when they change.

| Field | Type | Default | Description |
|---|---|---|---|
| `paths` | string[] | — | **Required.** Glob patterns to watch, relative to the project root. Validation rejects an empty list (`triggers['<id>'].config.paths must not be empty for file_watcher triggers`) |
| `debounce_secs` | integer | `5` | Debounce window in seconds before re-dispatching after a burst of changes |
| `ignore` | string[] | `[]` | Glob patterns to exclude from watching, relative to the project root |

```yaml
triggers:
  - id: schema-rebuild
    type: file_watcher
    workflow_ref: regenerate-schema
    config:
      paths:
        - "crates/protocol/src/**/*.rs"
        - "schema/**/*.json"
      debounce_secs: 15
      ignore:
        - "**/target/**"
        - "**/*.tmp"
```

### `webhook` and `github_webhook` config

Both kinds drain inbound webhook events from a daemon-managed queue. The
in-tree daemon does **not** itself register `POST /triggers/{id}` HTTP routes —
the public ingress is provided by an installed transport plugin (e.g.
`launchapp-dev/animus-transport-http`), which is expected to honour
`secret_env` and `max_triggers_per_minute` when it forwards events into the
queue. (Note: the `WebhookTriggerConfig` doc comment in
`crates/orchestrator-config/src/workflow_config/types.rs` still describes the
in-tree HTTP path that existed before transports were extracted out-of-tree
— treat the description in this section as authoritative until that comment
is refreshed.)

The in-tree path that *every* deployment can rely on is
[`animus trigger fire <trigger_id> --payload <json>`](cli/index.md), which
appends a synthetic event into the same pending-events queue that the daemon's
trigger dispatcher drains each tick. Use it for local testing or for piping
events from custom upstreams.

`github_webhook` behaves the same as `webhook` but is intended for GitHub-style
event payloads; filtering on the GitHub event type is left to the receiving
workflow today.

| Field | Type | Default | Description |
|---|---|---|---|
| `secret_env` | string | `null` | Environment variable name. Transport plugins that implement HTTP ingress read this env var to validate the `sha256=<hex>` signature header on incoming requests. `animus trigger fire` ignores it (events are already trusted) |
| `max_triggers_per_minute` | integer | `10` | Soft rate limit that transport plugins enforce by returning HTTP `429` on excess. `animus trigger fire` ignores it. Must be `> 0` |

```yaml
triggers:
  - id: deploy-hook
    type: webhook
    workflow_ref: deploy
    config:
      secret_env: DEPLOY_WEBHOOK_SECRET
      max_triggers_per_minute: 5
```

The signing secret is read from the transport plugin's process environment at
request time — it is not stored in YAML. Use shell `export`, your service
manager's env file, or `direnv` to populate it.

### `plugin` config

The daemon discovers every installed `trigger_backend` plugin via the stdio
plugin host and supervises one session per plugin. Each plugin emits
`trigger/event` notifications, which the daemon routes into the same
`pending_events` queue used by webhook triggers and drains via
`TriggerDispatch::process_due_triggers` on each tick.

**Important caveat (current behaviour):** the supervisor sends
`TriggerWatchParams::default()` to each plugin and does **not** forward the
per-trigger `config:` map from this YAML block to the plugin. Trigger plugins
that need configuration (Slack tokens, watch paths, etc.) currently source it
from their own environment or sidecar config files, not from this YAML. Putting
keys under `config:` for a `type: plugin` trigger is accepted by the config
parser but has no runtime effect today.

A dedicated trigger-plugin authoring guide is planned for `docs/guides/`; until
it lands, the plugin host contract lives in `crates/animus-plugin-protocol`
(see `TriggerWatchParams`, `TriggerEvent`, `TriggerAckParams`) and the
supervisor in `crates/orchestrator-daemon-runtime/src/schedule/trigger_supervisor.rs`.

### Plugin kill-switch

Setting `ANIMUS_DAEMON_DISABLE_TRIGGERS=1` skips the trigger plugin supervisor on
daemon start and interrupts any in-progress restart backoff. Schedules, webhooks,
and file watchers configured via this YAML block continue to run; only
`type: plugin` triggers are suppressed.

---

## daemon

Top-level block that tunes parts of the daemon at workflow-config compile time.
This block lives at the **top level** of the workflow YAML — not under a
workflow — and compiles into `DaemonConfig`.

```yaml
daemon:
  auto_run_ready: true
  active_hours: "09:00-17:00"
  phase_routing:
    implementation:
      tool: claude
      model: claude-sonnet-4-6
  mcp:
    # daemon-side MCP runtime config
```

### Fields read from workflow YAML

The daemon currently honours these fields from the YAML `daemon:` block:

| Field | Type | Default | Description |
|---|---|---|---|
| `auto_run_ready` | boolean | `false` | When `true`, the daemon auto-dispatches Ready subjects without a manual `animus workflow run`. Used as the fallback when the persisted daemon config does not pin `auto_run_ready` and no CLI override is passed |
| `active_hours` | string | unset (24/7) | Local-time window during which the daemon's project tick will dispatch new schedule- AND trigger-driven work, e.g. `"09:00-17:00"`. Outside this window the tick skips **both** `process_due_schedules` and `process_due_triggers`, so cron schedules, webhook events, file-watcher events, and plugin events are all suppressed. Missed cron fires are **not** replayed when the window reopens — the next tick re-evaluates the cron expression against the new current minute, so an 08:00 cron does not get a delayed run when a 09:00 window opens. Webhook and plugin events stay queued in `pending_events` until the window opens and drain then. In-flight phases are not interrupted. Schedules suppressed this way do **not** bump `missed_count`. Read on every tick from workflow YAML (the persisted daemon config has no `active_hours` field) |
| `phase_routing` | object | unset | Per-phase model/tool routing overrides applied at daemon spawn time. See [Model Routing](../guides/model-routing.md) |
| `mcp` | object | unset | Daemon-side MCP runtime config (forwarded to `ProcessManager`). See [MCP Tools](mcp-tools.md) |

### Fields parsed but not consumed by the daemon

The remaining fields exist on the `DaemonConfig` struct and round-trip through
config compilation, but the daemon does not currently read them from workflow
YAML. The persisted daemon config lives at
`~/.animus/<repo-scope>/daemon/pm-config.json` (not the project-local
`.animus/` tree). Set persisted fields via
`animus daemon config --<flag> <value>` (a leaf command — flags directly, no
`set` subcommand) or pass equivalent flags to `animus daemon run` /
`animus daemon start`.

| Field | Type | Where to set it today |
|---|---|---|
| `interval_secs` | integer | `animus daemon config --interval-secs <n>` (persisted, hot-reloaded) or `animus daemon run --interval-secs <n>` |
| `pool_size` | integer | `animus daemon config --pool-size <n>` (persisted, hot-reloaded) or `animus daemon run --pool-size <n>`. Alias: `max_agents` |
| `auto_merge` | boolean | `animus daemon config --auto-merge <bool>` |
| `auto_pr` | boolean | `animus daemon config --auto-pr <bool>` |
| `auto_commit_before_merge` | boolean | `animus daemon config --auto-commit-before-merge <bool>` |
| `auto_prune_worktrees` | boolean | `animus daemon config --auto-prune-worktrees-after-merge <bool>` |
| `max_task_retries` | integer | **No wired sink today.** The field exists on `DaemonConfig` but is not read from workflow YAML and is not a field on `DaemonProjectConfig`. Setting it has no runtime effect |
| `retry_cooldown_secs` | integer | **No wired sink today.** Same as `max_task_retries` |

Setting these keys under `daemon:` in workflow YAML is harmless (the config
round-trips), but it will not change daemon behaviour. Use
[`animus daemon config`](cli/index.md) (which accepts flags directly — there is
no `set` subcommand) or the equivalent CLI flags on
`animus daemon run` / `animus daemon start` instead.

### How `daemon:` blocks merge across files

`schedules:` and `triggers:` entries merge by `id` across all
`.animus/workflows/*.yaml` files (later overlays override earlier entries with
the same id). The `daemon:` block does **not** field-merge: as each overlay is
applied the YAML compiler runs `yaml.daemon.or(base.daemon)`, where `yaml` is
the overlay being applied. The result is that any overlay which defines a
`daemon:` block replaces the previously-accumulated block wholesale — fields
defined only in earlier overlays are dropped. Keep `daemon:` in a single
workflow YAML file to avoid losing settings to a partial later override.

---

See also: [Configuration](configuration.md), [Status Values](status-values.md).
