# Workflow YAML Schema Reference

Animus workflow YAML is authored in `.animus/workflows.yaml` and `.animus/workflows/*.yaml`.
Those files are merged with installed pack overlays to produce the
effective workflow configuration that `workflow-runner` executes. This document
describes the authored YAML surface.

For the target direction of phase output contracts, universal verdicts, and
YAML-defined phase-local fields, see [Phase Contracts](../architecture/phase-contracts.md).

> **YAML is the default config source.** As of v0.6 the daemon can read
> workflow and agent config from an installed `config_source` plugin instead
> of these YAML files (e.g. `animus-config-postgres` for the LaunchApp portal).
> This document describes the YAML surface, which remains the default for all
> projects that do not have a `config_source` plugin installed.
> See [Config sources (v0.6)](configuration.md#config-sources-v06) for details.

## Top-Level Structure

A workflow YAML file can contain any combination of these top-level sections:

```yaml
mcp_servers:     # MCP server definitions
agents:          # Agent profile definitions
agent_channels:  # Agent communication channel definitions
models:          # Named model+tool registry entries (agents reference by name)
phase_catalog:   # Phase UI metadata (label, description, category, tags)
phases:          # Reusable phase execution definitions
workflows:       # Named workflow pipelines
schedules:       # Cron-driven workflow dispatch
triggers:        # Event-driven workflow dispatch
daemon:          # Daemon-wide runtime tuning
secrets:         # Declarative secret references (v0.5.5+)
workspaces:          # Named repo sets an environment materializes (v0.7+)
environment_routing: # Default + rule-based environment plugin selection (v0.7+)
```

All sections are optional. Multiple YAML files in `.animus/workflows/` are merged,
and project YAML can override installed pack workflows.

---

## secrets (v0.5.5+)

Declares logical secret names backed by env-var names. Reference them in
any YAML scalar with `${secret.<name>}` — resolution happens at
config-compile time, with the file path and line number included in any
error. The lookup chain is explicit process env first, then the
project-scoped keychain entry for the same env-var key if one exists.

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

- The mapped env var key is resolved at compile time. Explicit process env
  wins; if it is unset, Animus falls back to the project-scoped keychain
  store populated by `animus secret`.
- Required-but-unset keys fail the compile.
- Referencing an undeclared key (`${secret.unknown}`) fails the compile.
- The compiled `workflow-config.v2.json` contains the *resolved string*
  — plugins consume the same scalar shape they always did. There is no
  runtime secret-store indirection.
- Parse diagnostics redact resolved values: when a substituted
  `${secret.<name>}` value (or a plain `${VAR}` value that came from the
  keychain fallback) would be echoed in a YAML error message, it is
  replaced with `[redacted:<name>]`. Plain `${VAR}` values resolved from
  the explicit process environment are not redacted.
- `animus workflow phases upsert` / `definitions upsert` never serialize
  resolved secret values back into the project tree: the generated
  overlay at `.animus/workflows/generated-workflow.yaml` carries only the
  upserted phase/pipeline entries (with any `${...}` references preserved
  unresolved), not a dump of the compiled config.

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

References inside YAML comments are **not** interpolated: a `#` that begins a
comment (preceded by start-of-line or whitespace, outside quoted scalars and
block scalar content) suppresses substitution through end of line, so a
comment like `# export ${LINEAR_TOKEN}` never fails the compile when the
variable is unset. `#` inside quoted strings (`key: "#tag ${X}"`) and inside
block scalar bodies (`|` / `>` prompts) still interpolates normally.

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

For HTTP-transport MCP servers (`transport: http` + `url:`), Animus rewrites
any server with an `oauth:` block to the local `animus-mcp-proxy` stdio
bridge. The agent never receives a resolved bearer token directly; the proxy
resolves the live credential itself at connect time, injects
`Authorization: Bearer <token>` upstream, and retries once after an upstream
auth failure.

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

For `authorization_code`, the proxy uses the OS keychain entry created by
`animus mcp auth`. For broker-backed M2M flows, bearer resolution and cache
behavior stay inside the OAuth broker; the resolved token never rides the
runtime contract, `.mcp.json`, or provider argv.

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

**Failure modes:** A failed token resolution leaves the proxy entry intact and
the downstream MCP call surfaces the auth error from the proxy at runtime. The
agent contract is not downgraded to an unauthenticated HTTP entry.
Token text never appears in logs.

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
      max_entries: 200           # Optional. FIFO cap; oldest entries trim on
                                 # append. Omitted ⇒ default 200. Must be > 0.
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
    approval_policy:             # Optional. animus.agent.request_approval routing.
      auto_allow: ["cargo *", "git.commit"]
      auto_deny: ["git.push*"]
      default: ask               # ask (manual) | allow (approve everything) | deny | llm (auto-approve)
      evaluator_model: anthropic/claude-haiku-4-5   # for default: llm — judge model (falls back to the agent's model)
      evaluator_instructions: "Deny anything touching billing or prod." # optional extra rubric for the judge
    hooks:                       # Optional. Author-controlled harness-hook config.
      policy_rules:              #   Guardrail rules merged into the compiled hook policy.
        - id: no-prod-deploy
          tools: ["Bash"]
          input_matchers:
            - field: command
              regex: "deploy .*--env(=| )prod"
          decision: deny         #   deny | ask | allow | defer (deny always wins)
          reason: "Production deploys require a human."
      observers:                 #   Extra events routed to animus-hook (record-only).
        - events: ["UserPromptSubmit"]
          action: record         #   only `record` this wave (no arbitrary shell)
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
| `reasoning_effort` | string | no | Provider reasoning/thinking effort: `low`, `medium`, or `high`. Mapped per provider (codex `-c model_reasoning_effort="<level>"`, claude `--effort <level>`); other providers ignore it. Validated at compile time |
| `permission_mode` | string | no | Provider permission/approval mode, forwarded verbatim (claude: `default`/`acceptEdits`/`bypassPermissions`/`plan`; codex: `untrusted`/`on-failure`/`on-request`/`never`; gemini: `default`/`auto_edit`/`yolo`). Unknown values warn but pass through. The `animus agent run` / `animus chat send` `--permission-mode` flag overrides it |
| `mcp_servers` | string[] | no | Names of `mcp_servers` entries this agent can use |
| `skills` | string[] | no | Skill identifiers to attach. Skills resolve from built-ins, `.animus/config/skill_definitions/*.yml`, and Markdown skills such as `.animus/skills/<name>/SKILL.md` or `.animus/skills/<name>.md` |
| `capabilities` | map\<string, bool\> | no | Capability flags |
| `tool_policy` | object | no | Tool access control policy |
| `approval_policy` | object | no | Routing for `animus.agent.request_approval` MCP calls. `auto_allow` / `auto_deny` are `*`-glob pattern lists matched against the request's `tool_name` when present, otherwise its `action`; `auto_deny` wins on overlap (fail closed). `default` selects the mode when no list matches: `ask` (manual — escalate to a pending human interaction, the default), `allow` (approve everything / "dangerous" mode), `deny`, or `llm` (auto-approve: a judge model reads the tool call and returns allow/deny). For `llm` mode, `evaluator_model` picks the judge model (defaults to the agent's own model) and `evaluator_instructions` appends an operator rubric to the built-in judge prompt; the judge runs with no MCP tools (cannot recurse), and any evaluator failure falls back to manual `ask` so an LLM outage never silently auto-approves. Allow/deny decisions are recorded with `source: "llm"` and the judge's reason |
| `hooks` | object | no | Author-controlled harness-hook config. `policy_rules` are guardrail rules (`protocol::HookPolicyRule` shape) merged into the compiled hook policy; `observers` route extra harness events to `animus-hook` (constrained to a named built-in `action` — only `record` this wave — never an arbitrary command). An author rule can only **add** restriction: `deny` always wins over `allow` regardless of source. claude-only this wave; gated behind `ANIMUS_DISABLE_HARNESS_HOOKS`. See [harness-hooks.md](harness-hooks.md) |

Agent profiles defined in YAML are merged into the agent runtime config during compilation. Phase definitions reference agents by profile name.

The merge is presence-aware per field: a field you write in YAML always wins
over the base profile (builtin defaults or pack overlays), even when you set
it back to its default value — `memory: { enabled: false }`, `mcp_servers: []`,
or `skills: []` explicitly disable what a pack enabled. A field you omit
inherits the base profile's value.

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

> **The participant roster is advisory, not an access-control boundary.**
> `participants` documents the intended membership and drives which agents
> the channel context is injected for; it does not hard-deny a write. The
> load-bearing gate is the per-profile `communication` block
> (`enabled: true` + the channel listed under `channels`). Treat the roster
> as a coordination hint, not a security control — do not rely on it to keep
> an agent out of a channel.

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
| `evals` | object | no | Eval gate definition (see [evals](#evals-experimental--runtime-enforcement-deferred)). Parsed and validated in v0.5.5; runtime enforcement lands when the out-of-tree workflow-runner plugin pin bumps |

Phase `skills` are validated during config load and applied at phase runtime. The phase's effective skill set is the union of the phase-level `skills:` list and the executing agent profile's `skills:` (phase entries first, deduplicated; a workflow-YAML `agents.<id>.skills` declaration — even an explicit empty list — wins over the agent runtime config profile). Skill names resolve at workflow dispatch time, daemon-side, against the same scoped sources and trust rules as the ad-hoc `--skill` path: project > user > installed > agent-host prompt-only (agent-host `SKILL.md` skills have structural fields like `tool_policy`, `env`, and `mcp_servers` stripped at load). The resolved definitions ride the dispatch payload to the workflow runner (`ANIMUS_PHASE_SKILLS_JSON` on the spawn environment); a runner paired with an older daemon resolves locally with the same rules.

A skill name that does not resolve is a loud warning in the daemon dispatch log plus a `missing` record in the phase execution metadata — not a hard failure (the autonomous dispatch path never wedges on a stale skill reference, unlike the interactive `--skill` flag, which hard-errors). Misses of skills declared only by builtin agent-profile defaults (e.g. the `animus.core-skills` pack references) log at debug level instead. Explicit `skills:` declarations in workflow YAML (`phases.<id>.skills` and `agents.<id>.skills`) are additionally checked at compile/validate time against the project's skill sources: an unresolvable name surfaces as a warning (never an error) on the compile stderr and in the `warnings` array of `animus workflow config validate` / `compile`, suggesting `animus skill list`. Implicit references from builtin agent-profile defaults are exempt — they are legitimately absent until the matching pack is installed. After a run, `animus output phase-outputs --workflow-id <id>` shows per-phase requested/applied/missing skills in its human view.

Activation gating (`activation.tools` / `activation.models`) is evaluated at phase execution, against the tool/model the runner actually selects. Applied skills can inject prompt fragments (system/prefix/suffix/directives), tool policy, MCP server attachments (resolved by name against workflow-YAML `mcp_servers:` or project config, joining the phase contract the same way `phase_mcp_bindings` do; unknown names warn and are skipped), launch args/env, and capability overrides. The phase execution metadata records `requested_skills` (declared names), `resolved_skills` (names that resolved to a definition), and `applied_skills` (skills whose activation matched and whose contributions were injected). `animus workflow prompt render` previews the skill-injected prompt. Installed registry skills work the same as local skills when a definition snapshot is present in Animus state.

When `runtime.tool_profile` is set, the effective tool must resolve to
`claude`. Animus looks up the named profile in the user's global config and injects
its environment into the Claude launch contract.

The phase `runtime` block accepts the same provider knobs as an agent
profile — including `reasoning_effort` (`low`/`medium`/`high`) and
`permission_mode`. Resolution cascades **phase runtime → agent profile**: a
non-empty `runtime.reasoning_effort` or `runtime.permission_mode` on the
phase wins over the agent profile's value, mirroring how `model` and
`tool` cascade. The resolved effort level maps per provider (codex
`-c model_reasoning_effort`, claude `--effort`) and is validated at compile
time; `permission_mode` is forwarded verbatim (claude `--permission-mode`,
codex `-c approval_policy`, gemini approval mode) and unknown values only
warn. The `animus agent run` / `animus chat send` `--reasoning-effort` and
`--permission-mode` flags override both. Note: the ad-hoc surfaces
(`animus agent run` / `animus chat send` with `--agent`) honour
`permission_mode` today; workflow phase execution enforces it once the
out-of-tree workflow-runner plugin pin consumes the new field.

### evals (experimental — runtime enforcement deferred)

> **Status (v0.5.5):** the YAML surface, config types, validators, and
> runner library (`animus_runtime_shared::evals`) ship in v0.5.5. The
> workflow-runner pin that calls `run_evals` between phase output and
> phase advance lives in `launchapp-dev/animus-workflow-runner-default`
> and is pending its next release. **Until that lands, a phase advances
> regardless of an `evals:` block** — author/test the gate now, but do
> not yet rely on it for production trust. Declaring an `evals:` block
> therefore emits a declared-but-unenforced warning at compile time and
> in `animus workflow config validate` so you are never silently
> unprotected.

`evals` declares a quality gate that runs **after** the phase emits an
`advance` decision and **before** the workflow advances. Each check returns
pass/fail; the gate advances when `pass_rate >= pass_threshold`. Failures
route to either `rework` (re-execute the phase, up to `max_reworks`) or
`block` (pause the workflow for manual approval).

```yaml
phases:
  implementation:
    mode: agent
    agent: implementer
    evals:
      pass_threshold: 0.8           # 80% of checks must pass; default 1.0
      on_fail: rework               # rework | block; default block
      max_reworks: 2                # default 0; required > 0 when on_fail=rework
      checks:
        - id: unit-tests
          kind: command
          command: cargo
          args: [test, --workspace]
          working_dir: $REPO_ROOT   # falls back to the phase working dir
          timeout_secs: 300
          expected_exit: 0
        - id: code-quality
          kind: llm_judge
          agent: po-reviewer
          prompt: "Does the implementation address the spec? Reply PASS or FAIL."
```

#### Fields

| Field | Type | Required | Description |
|---|---|---|---|
| `pass_threshold` | float | no | Minimum pass rate (0.0–1.0) required to advance. Default `1.0` |
| `on_fail` | enum | no | `rework` or `block`. Default `block` |
| `max_reworks` | int | no | Rework attempts available when `on_fail=rework`. Default `0`. Must be `> 0` if `on_fail=rework` |
| `checks` | list | yes | At least one [eval check](#eval-check-kinds) |

#### Eval check kinds

**`kind: command`** — spawns the program in the phase's working directory
(or `working_dir` when set; `$REPO_ROOT` resolves to the default. Do NOT
use `${REPO_ROOT}` — that form is consumed by the workflow YAML env-var
interpolation layer at load time and never reaches the runner. Relative
paths anchor on the default working directory). Waits up to `timeout_secs`
(default `300`) and passes when the process exit code matches
`expected_exit` (default `0`). On timeout the entire process group is
killed (Unix; Windows kills the direct child only). `command` is required
and is validated against `tools_allowlist` when that is non-empty.

**`kind: llm_judge`** — dispatches a one-shot agent call. Requires `agent`
(must resolve through `agent_profiles`) and `prompt`. The judge sees the
just-produced phase output via `phase_output_summary` in the request
context. Pass: the response's first whitespace-delimited token is `PASS`
(case-insensitive, optional trailing punctuation; words that merely START
with `PASS` such as `PASSIVE` or `PASSAGE` do NOT count). Anything else
is a fail.

#### Decision-log records

Each check appends one `animus.eval.v1` record to the workflow decision
log:

```json
{
  "schema": "animus.eval.v1",
  "phase_id": "implementation",
  "check_id": "unit-tests",
  "kind": "command",
  "passed": true,
  "duration_ms": 12345,
  "exit_code": 0,
  "output_excerpt": "<capped at ~2 KiB; head+tail elision for long runs>"
}
```

`exit_code` is omitted on `kind: llm_judge` records. `output_excerpt` is
empty when the runner could not capture output (e.g. process spawn
failure). An `error` field is populated when the check failed for a reason
other than a clean exit-code mismatch (spawn error, timeout, judge
backend missing, etc.).

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
workflows:
  - id: my-workflow
    name: My Workflow
    description: A workflow that does things
    phases:
      - research
      - implementation
      - code-review:
          max_rework_attempts: 3
          skip_if:
            - "task.type == 'hotfix'"
          on_verdict:
            rework:
              target: implementation
            advance:
              target: testing
            fail:
              target: ""
      - testing
      - create-pr        # command phase running `gh pr create`
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
| `variables` | Variable[] | no | Variables used by this workflow |
| `budget` | BudgetConfig | no | Cost ceiling for the whole workflow run (v0.5.5+) |
| `environment` | string | no | Environment plugin id (or routing rule key) every phase in this workflow runs in unless the phase overrides it (v0.7+). See [environment](#environment). |
| `workspace` | string | no | Named [`workspace`](#workspaces) (repo set) this workflow runs against (v0.7+). |

A rich phase entry (see [Phase Entry Types](#phase-entry-types)) additionally
accepts a phase-level `environment:` and `workspace:` that override the
workflow-level values:

```yaml
    phases:
      - implementation:
          environment: firecracker   # this phase runs in the microVM env
          workspace: fullstack       # against the multi-repo workspace
```

## budget

> **Enforcement status (v0.5.x):** the daemon enforces budget caps on its
> housekeeping cadence — once per heartbeat interval (`interval_secs`,
> default 30s), not mid-phase token-by-token. Each sweep rescans run
> spend, evaluates declared caps, and acts on any NEWLY crossed cap:
> a breach decision is written to the breaching run's
> `runs/<run_id>/decisions.jsonl` (visible via `animus output decisions`)
> and to the scoped fleet log (visible via `animus cost decisions`), the
> declared action is applied — `on_exceed: pause` pauses the workflow
> through the standard pause path, `on_exceed: fail` fails the current
> phase terminally, `warn` records + notifies only — and a
> `workflow-budget-breach` event reaches notifier plugins —
> exactly once per breach. Two honest caveats: a phase in flight can
> overshoot the cap by up to one sweep before the pause lands, and
> enforcement requires a running daemon — with no daemon, caps are only
> recorded (never paused) when an `animus cost` command runs. The
> declared-but-unenforced warning that `budget:` blocks used to emit at
> compile/validate time was removed when this enforcement landed.

The `budget:` block declares cost ceilings. It can live at three places:

1. **Top-level on a workflow** — cap that applies across all phases of a
   single workflow run. The ceiling is authoritative; if a phase has a
   higher cap, the workflow-level cap still wins.
2. **Inline on a rich phase entry** — cap that applies for one phase
   inside one workflow run. Resets per rework attempt.
3. **Anywhere either of the two above applies**, the workflow runner
   pauses, fails, or warns according to `on_exceed`.

```yaml
workflows:
  - id: expensive-flow
    name: Expensive Flow
    phases:
      - exploration:
          budget:
            max_tokens: 100_000
            max_cost_usd: 1.00
            on_exceed: fail
      - implementation
    budget:
      max_tokens: 1_000_000
      max_cost_usd: 5.00
      on_exceed: pause
```

### BudgetConfig Fields

| Field | Type | Required | Description |
|---|---|---|---|
| `max_tokens` | integer | one of two | Cap on combined input + output + reasoning tokens. Cache reads/writes are tracked but excluded from the cap. |
| `max_cost_usd` | number | one of two | USD cost ceiling. Cents precision. |
| `on_exceed` | `pause` \| `fail` \| `warn` | no (default `pause`) | What to do when a cap is crossed. |

Validation rules (from `validate_workflow_config`):

- at least one of `max_tokens` or `max_cost_usd` must be set;
- `max_tokens` must be greater than 0;
- `max_cost_usd` must be a finite number greater than 0;
- `on_exceed` must be one of `pause`, `fail`, or `warn`.

The `animus cost` CLI surface (`summary`, `workflow`, `top`, `trends`)
reports against the same per-run rollup the budget enforcer reads. See
[`docs/reference/cli/index.md`](cli/index.md) and
[`docs/reference/configuration.md`](configuration.md) for runtime
configuration including the per-model USD rate table.


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

#### Rich (single-key map)

An inline phase override with routing, rework limits, budget caps, and conditional
skipping. The YAML form is a **single-key map** whose key is the phase ID and
whose value is the configuration block. An `id:` sibling key is not used and
will cause a parse error — the phase id IS the map key.

```yaml
phases:
  - code-review:
      max_rework_attempts: 3
      skip_if:
        - "task.type == 'hotfix'"
      on_verdict:
        rework:
          target: implementation
        advance:
          target: testing
        fail:
          target: ""
      budget:
        max_cost_usd: 2.00
        on_exceed: pause
```

| Field | Type | Required | Description |
|---|---|---|---|
| `<phase-id>` (map key) | string | yes | Phase definition ID to execute — this IS the map key, not a sibling `id:` field |
| `max_rework_attempts` | integer | no | Maximum rework loops before failing (default: 3) |
| `skip_if` | string[] | no | Conditions under which to skip this phase |
| `on_verdict` | map\<string, TransitionConfig\> | no | Routing rules keyed by verdict name |
| `budget` | BudgetConfig | no | Phase-level cost ceiling (v0.5.5+) |

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

### Git operations are command phases

> **`post_success.merge` was removed in v0.5.x.** Animus no longer performs git
> operations (commit, push, PR, merge) as runner automation. A workflow YAML that
> still sets a `post_success.merge` block — or `integrations.git.auto_merge` — is
> rejected at parse time with an actionable error.

Express commit, push, PR creation, and merge as ordinary **command phases**: a
phase with a `command:` block that runs `git` or `gh`. They sequence in `phases:`
like any other phase, run in the task worktree, and surface their exit code
through the standard `output_contract`. Because they are real phases, they also
get verdict routing, retries, and approval gates for free.

```yaml
phases:
  create-pr:
    mode: command
    directive: Open a GitHub PR from the current branch
    command:
      program: gh
      args: ["pr", "create", "--fill", "--base", "main"]
      cwd_mode: task_root
      timeout_secs: 60
      success_exit_codes: [0]

workflows:
  - id: ship
    name: Ship
    phases:
      - implementation
      - testing
      - create-pr      # command phase: `gh pr create`
```

A human still performs the final merge of the PR (or you add an explicit
`gh pr merge` command phase, gated by an approval if desired). See the
[phases](#phases) reference for the full `command:` schema.

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

  # Phase 3: Review with rework routing (rich entry: single-key map)
  - code-review:
      max_rework_attempts: 3
      on_verdict:
        rework:
          target: implementation
        advance:
          target: testing

  # Phase 4: Run tests
  - testing

  # Phase 5: Open a PR (command phase running `gh pr create`)
  - create-pr

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
- An occurrence that falls between ticks (long tick, `interval_secs` above 60)
  is caught up on the next tick. Only the **most recent** missed occurrence
  fires — older occurrences inside the catch-up window are skipped, so a
  schedule resuming after a gap never replays a backlog. The catch-up scan
  only looks back 10 minutes: runs missed for longer (daemon down at fire
  time, `active_hours` window closed) are **not** replayed — schedules fire
  forward-only from the next occurrence.
- Per-schedule activity is tracked under the scoped runtime state in
  `ScheduleRunState` with three counters:
  - `last_run` — UTC timestamp of the cron occurrence covered by the most
    recent dispatch attempt. Updated when the schedule fires AND the daemon
    got far enough to invoke the spawn — including spawn failures other than
    tick-budget exhaustion and the workflow-concurrency cap (this prevents
    the same occurrence from retrying every tick).
  - `run_count` — total dispatch attempts since project init (successes and
    non-budget spawn failures both increment it).
  - `missed_count` — increments only when the per-tick budget or the
    workflow-concurrency cap rejected the spawn slot; `last_run` is **not**
    updated in that case so the schedule gets another shot at the same
    occurrence on the next tick. Ticks skipped outside `active_hours` do not
    touch either counter — the whole schedule branch is bypassed.

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
A `config:` block whose fields have the wrong type (e.g. `secret_env: 123`) is
rejected at validation time with the underlying deserialization error
(`triggers['<id>'].config is not a valid webhook config: ...`) instead of being
silently replaced with defaults — a malformed `secret_env` can no longer
silently disable signature verification.

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
        - "crates/orchestrator-config/src/**/*.rs"
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
| `active_hours` | string | unset (24/7) | Local-time window during which the daemon's project tick will dispatch new schedule- AND trigger-driven work, e.g. `"09:00-17:00"`. Outside this window the tick skips **both** `process_due_schedules` and `process_due_triggers`, so cron schedules, webhook events, file-watcher events, and plugin events are all suppressed. Missed cron fires are **not** replayed when the window reopens — the next tick re-evaluates the cron expression against the new current minute, so an 08:00 cron does not get a delayed run when a 09:00 window opens. Webhook and plugin events stay queued in `pending_events` until the window opens and drain then. In-flight phases are not interrupted. Schedules suppressed this way do **not** bump `missed_count`. Read on every tick from workflow YAML (the persisted daemon config has no `active_hours` field) |
| `phase_routing` | object | unset | Per-phase model/tool routing overrides applied at daemon spawn time. See [Model Routing](../guides/model-routing.md) |
| `mcp` | object | unset | Daemon-side MCP runtime config (forwarded to `ProcessManager`). See [MCP Tools](mcp-tools.md) |
| `budget` | object | unset | Fleet-level daily spend cap. See [`daemon.budget`](#daemonbudget-fleet-daily-spend-cap) below |

### `daemon.budget` (fleet daily spend cap)

The `daemon.budget` block is a **fleet-level** spend cap, distinct from the
per-workflow / per-phase [`budget:`](#budget) caps. Per-workflow caps stop a
single runaway workflow; the fleet cap bounds the daemon's **total** rolling
spend so that many individually in-budget workflows cannot collectively blow
through a daily wallet.

```yaml
daemon:
  budget:
    max_cost_usd_per_day: 50.0   # pause new dispatch past $50 in any rolling 24h
    on_exceed: pause             # only `pause` is honored today
```

| Field | Type | Default | Description |
|---|---|---|---|
| `max_cost_usd_per_day` | float | unset (uncapped) | USD ceiling on the daemon's total spend over a **rolling 24-hour** window. A non-positive value means uncapped |
| `on_exceed` | `pause` \| `fail` \| `warn` | `pause` | Action when the cap is crossed. Only `pause` is honored today; the field is carried for forward compatibility |

**Window semantics.** The cap is measured over a **rolling 24-hour** window,
not a calendar day — no timezone applies. When the rolling spend crosses the
cap, the daemon stops picking up *all* new work (cron schedules, triggers,
ready tasks, and explicit queue-drain entries) until either spend ages out of
the window or the cap is raised/cleared, at which point dispatch resumes
automatically on the next sweep. In-flight phases are never interrupted.

**Precedence.** The scoped daemon runtime config takes precedence over this
YAML block: set it with `animus daemon config --max-daily-usd <N>` (persisted
to `~/.animus/<repo-scope>/daemon/pm-config.json`, hot-reloaded). Pass
`--max-daily-usd 0` to explicitly clear the cap — an explicit pm-config value
(positive or zero) always overrides the YAML cap. The cap, today's rolling
spend, remaining headroom, and the pause state are surfaced under
`budget_enforcement.daily_cap` in `animus daemon health --json` and as the
`daily_*` fields in `animus cost summary --json`.

The fleet cap honors the `ANIMUS_DAEMON_DISABLE_BUDGET_ENFORCEMENT` kill-switch:
when budget enforcement is disabled, the daily-cap latch is neither reconciled
nor enforced.

### Removed daemon keys

The following keys were **removed from the workflow `DaemonConfig` struct**.
They no longer parse into a field; declaring them under `daemon:` in workflow
YAML still compiles but emits a removed-key warning.

| Key | Where to set it today |
|---|---|
| `interval_secs` | `animus daemon config --interval-secs <n>` (persisted to `~/.animus/<repo-scope>/daemon/pm-config.json`, hot-reloaded) or `animus daemon run --interval-secs <n>` |
| `pool_size` (alias `max_agents`) | `animus daemon config --pool-size <n>` (persisted, hot-reloaded) or `animus daemon run --pool-size <n>` |
| `max_task_retries` | No runtime sink — the daemon never read it; drop it from the daemon block |
| `retry_cooldown_secs` | No runtime sink — the daemon never read it; drop it from the daemon block |
| `auto_run_ready` | Removed — the daemon is queue-only and never auto-dispatches Ready subjects; enqueue work with `animus queue enqueue` or drive it from a `schedules:` cron entry |

The persisted daemon config lives at
`~/.animus/<repo-scope>/daemon/pm-config.json` (not the project-local
`.animus/` tree). Set persisted fields via
`animus daemon config --<flag> <value>` (a leaf command — flags directly, no
`set` subcommand) or pass equivalent flags to `animus daemon run` /
`animus daemon start`.

The daemon git/merge policy keys (`auto_merge`, `auto_pr`,
`auto_commit_before_merge`, `auto_prune_worktrees`) were **removed in v0.5.x**
along with their `animus daemon start` / `animus daemon run` /
`animus daemon config` flags. Declaring them in YAML still compiles but emits a
removed-key warning. Animus no longer performs git operations as runner
automation — express commit/push/PR/merge as
[command phases](#git-operations-are-command-phases) instead.

Declaring any of these keys in workflow YAML emits a compile-time warning
(stderr, on every path that compiles YAML — daemon start, workflow run,
config reload) and a structured `warnings` entry in
`animus workflow config validate` / `animus workflow config compile`
(including `--json`). The warnings are advisory only: the config still
compiles and validates. The warning registry lives in
`crates/orchestrator-config/src/workflow_config/validation.rs`
(`UNENFORCED_RULES`); entries are removed as enforcement lands.

### How `daemon:` blocks merge across files

`schedules:` and `triggers:` entries merge by `id` across all
`.animus/workflows/*.yaml` files (later overlays override earlier entries with
the same id). The `daemon:` block **field-merges**: as each overlay is applied,
only the fields the overlay explicitly sets override the previously-accumulated
block — fields defined only in earlier overlays survive a later partial
`daemon:` block.

---

## workspaces (v0.7+)

> Foundation config for the `environment` plugin kind. The kernel parses,
> merges, and compiles these fields today; the runtime that materializes a
> workspace and executes the harness inside it is delivered by the environment
> plugin (worktree / container / microVM) and the workflow runner. With no
> environment plugin installed the runner falls back to local execution and
> `workspaces:` / `environment_routing:` are inert.

A **workspace** is a named set of repositories an environment materializes as a
single checkout tree. Workflows and phases reference a workspace by name via
`workspace:`; when unset, the environment's default single-repo workspace is
used.

```yaml
workspaces:
  fullstack:
    repos:
      - url: https://github.com/acme/api
        primary: true          # the default command cwd (first entry if none set)
      - url: https://github.com/acme/web
        name: web              # checkout subdir (defaults to last url path segment)
        git_ref: develop       # branch/tag/commit (defaults to remote default)
```

### WorkspaceRepo fields

| Field | Type | Required | Description |
|---|---|---|---|
| `url` | string | yes | Clone URL or local path for the repository |
| `name` | string | no | Subdirectory to check out under (defaults to the last `url` path segment) |
| `git_ref` | string | no | Git ref (branch/tag/commit) to check out (defaults to the remote default branch) |
| `primary` | bool | no | Marks the primary repo (default command cwd); the first entry wins if none is marked |

## environment_routing (v0.7+)

Config-level selection of WHICH environment plugin runs a given unit of work.
The kernel evaluates `rules` top-to-bottom (first match wins) and falls back to
`default` when none match.

```yaml
environment_routing:
  default: local              # environment plugin id used when no rule matches
  rules:
    - match:
        kind: task            # subject kind (unset = wildcard)
        harness: claude       # provider/tool id (unset = wildcard)
      environment: firecracker
      spec:                   # optional spec overrides merged into the compiled EnvironmentSpec
        image: acme/dev:latest
    - match:
        harness: codex        # any kind, codex harness -> sandbox env
      environment: sandbox
```

### Resolution precedence

For each unit of work the environment plugin id is resolved as (highest first):

1. Phase-level `environment:` override.
2. First matching `environment_routing.rules` entry (a rule matches when its
   `match.kind` is unset-or-equals the subject kind **AND** its `match.harness`
   is unset-or-equals the harness — an all-unset `match` is a catch-all).
3. Workflow-level `environment:` override.
4. `environment_routing.default`.
5. None — no explicit environment; the runner uses built-in local execution.

### EnvironmentRule fields

| Field | Type | Required | Description |
|---|---|---|---|
| `match.kind` | string | no | Match on subject kind (unset = wildcard) |
| `match.harness` | string | no | Match on harness / provider tool id (unset = wildcard) |
| `environment` | string | yes | Environment plugin id to route matching work to |
| `spec` | map | no | Opaque spec overrides merged into the compiled `EnvironmentSpec` |

## environment (v0.7+)

The `environment:` key (on a workflow definition or a rich phase entry) pins a
unit of work to a specific environment plugin id, overriding
`environment_routing`. Phase-level wins over workflow-level. See the resolution
precedence above. The `environment` plugin kind is an **optional** preflight
role: `animus daemon preflight` / `animus plugin status` report an installed
environment plugin, but its absence never blocks daemon boot.

---

See also: [Configuration](configuration.md), [Status Values](status-values.md).
