# Harness Hooks

Animus can wire a provider CLI's native **harness hook** mechanism so every
tool call an autonomous agent makes flows through an Animus guardrail before it
runs. This is the kernel's enforcement spine for agent permissions: it turns a
declarative `tool_policy` (and optional author-supplied guardrail rules) into a
compiled policy the harness consults on every gated tool call.

This page covers the activation wiring (the `animus-hook` binary, the compiled
policy file, and the per-session harness config), the **agent-level
author-controlled `hooks:` block**, the merge + deny-wins semantics, the
security/trust model, and the kill-switch.

## Components

| Piece | What it is |
| --- | --- |
| `animus-hook` binary | Sibling of `animus` (ships in the same package as `animus` and `animus-mcp-proxy`). Reads a harness hook payload on stdin, records the event to scoped state, and — for gate events — evaluates the compiled policy and prints the harness decision JSON. Always exits 0; decisions travel via stdout. Fail-closed: an explicitly-requested but unloadable policy denies. |
| `animus-policy.json` | The compiled `protocol::HookPolicy` (`hook-policy.v1.json` schema). Written per session under the run dir. |
| `animus-hooks.settings.json` | A minimal claude settings file containing ONLY a `hooks` block. Passed to claude via `--settings` (additive). Written per session under the run dir. |

The policy is provider-agnostic. The per-session harness config is
provider-specific; **only the claude `--settings` vector is generated this
wave** (see [Provider coverage](#provider-coverage)).

## Activation (claude, this wave)

When a phase runs against a **claude** provider and
`ANIMUS_DISABLE_HARNESS_HOOKS` is unset, the kernel injector
(`inject_harness_hooks`) does three things into the per-session run dir:

1. Compiles the resolved agent `tool_policy` + agent-authored policy rules into
   `<session_dir>/animus-policy.json`.
2. Writes `<session_dir>/animus-hooks.settings.json` — a settings file whose
   `hooks` block routes every wired event to the resolved `animus-hook` binary:
   - **Gate events** `PreToolUse` and `PermissionRequest` run
     `animus-hook emit --event <E> --session <id> --project-root <root> --policy <policy>`.
   - **Observability events** `PostToolUse`, `Stop`, `SessionStart`,
     `SessionEnd` (plus any author observer events) run the same `emit` without
     `--policy` (record-only, never a gate).
3. Appends `--settings <path>` to the claude launch args (ahead of the prompt).

`--settings` is **additive** in claude v2.1.x: the user's own `~/.claude` and
project `.claude` hooks still run alongside the Animus-injected ones. Animus
**never** writes to `~/.claude` or any shared settings — only the per-session
run dir, which is reaped with the run.

> **PreToolUse is the load-bearing gate.** In headless (`--print`) claude
> sessions — which is how Animus launches claude — `PermissionRequest` hooks do
> not fire, so the gate is enforced through `PreToolUse`. `PermissionRequest` is
> still wired so interactive sessions are also governed.

> **`--include-hook-events` is intentionally NOT emitted.** It is not a real
> claude CLI flag (verified against claude v2.1.x); adding it would error the
> session. The `--settings` file is sufficient to wire all hook events.

### Decision mapping

- `PreToolUse`: a `deny`/`ask`/`allow` verdict prints
  `{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"<d>","permissionDecisionReason":"<r>"}}`.
  A `defer` verdict prints nothing (abstain) — the harness's normal permission
  flow and the user's own hooks decide.
- `PermissionRequest`: `deny`/`allow` map to the `decision.behavior` payload;
  `ask`/`defer` abstain.

## Agent-level author-controlled hooks (`hooks:`)

An agent profile may carry an optional `hooks:` block. It is authored in BOTH
the agent-runtime config (`agents.<id>.hooks`) and workflow-YAML agent
definitions (`agent_profiles.<id>.hooks`) — they share the same
`AgentProfile`/`AgentProfileOverlay` shape, so the block compiles identically
from either source. The block is additive: a config without `hooks:` loads
unchanged.

```yaml
agent_profiles:
  trader:
    tool_policy:
      deny:
        - "Bash(* --live*)"        # kernel guardrail (see Compilation below)
        - "Bash(*submit-order*)"
    hooks:
      # (a) Author guardrail POLICY RULES — merged into animus-policy.json.
      policy_rules:
        - id: no-prod-deploy
          tools: ["Bash"]
          input_matchers:
            - field: command
              regex: "deploy .*--env(=| )prod"
          decision: deny
          reason: "Production deploys require a human."
      # (b) Author OBSERVERS — extra events routed to animus-hook (record-only).
      observers:
        - events: ["UserPromptSubmit"]
          action: record
```

### Two authoring surfaces

- **`policy_rules`** — a list of `protocol::HookPolicyRule` (the same shape the
  kernel compiles `tool_policy` into). They merge into the compiled
  `animus-policy.json` alongside the kernel rules. Pure data evaluated by the
  kernel's severity-ordered evaluator — they never run shell. An author rule may
  only **tighten**, enforced at compile time:
  - `allow` / `defer` are never honored from authors (downgraded to `defer`
    abstain) — an author `allow` could otherwise bypass the harness prompt.
  - `deny` always survives (maximally restrictive).
  - `ask` survives only when it is strictly more restrictive than the compiled
    policy default. Under an **allowlist** (`tool_policy.allow` non-empty, so the
    default is `deny`) an author `ask` is dropped — it would otherwise undercut
    the allowlist's deny-by-default into an ask prompt.

  (Kernel `tool_policy.allow` entries are intentionally allow rules — they are
  operator-authored guardrail intent, not the constrained agent surface.)
- **`observers`** — additional harness events the author wants routed to the
  Animus hook spine for observability/automation. **Constrained for safety**: an
  observer names `events` and a built-in `action` (only `record` this wave). The
  kernel generates the command — always the validated `animus-hook emit`
  invocation — so an author can widen *observation* but cannot inject an
  arbitrary command into the agent's session.

### Merge + deny-wins semantics

`inject_harness_hooks` builds ONE `animus-policy.json` from, in order:

1. `tool_policy.deny` → deny rules
2. `tool_policy.allow` → allow rules
3. agent-authored `hooks.policy_rules` (verbatim)

…and ONE settings `hooks` block from the kernel gate + observability events plus
the author observer events.

**Order is irrelevant to safety.** The kernel evaluator
(`protocol::hook_policy`) collects every matching rule and the most restrictive
decision wins: `Deny` > `Ask` > `Allow` > `Defer`. So:

- An author rule can always **add** restriction (a new `deny`/`ask`).
- An author `allow` can **never weaken** a kernel/`tool_policy` `deny` for the
  same call — deny wins regardless of rule source or order.

The compiled policy's `default_decision` (what an unmatched call gets) mirrors
`AgentToolPolicy::is_tool_permitted`:

- **`tool_policy.allow` empty** → `default_decision = defer`. Unmatched calls
  abstain, so the agent's own harness permission flow and the user's own hooks
  still govern anything Animus has no opinion on.
- **`tool_policy.allow` non-empty** → `default_decision = deny`. A non-empty
  allow list is an **allowlist**: every tool not explicitly allowed (or matched
  by an `ask` rule) is denied. When using an allow list, include every tool the
  agent legitimately needs — including the `animus.*` / MCP tools it relies on —
  or those calls will be denied.

### Compilation: `tool_policy` → policy rules

`tool_policy` entries use claude matcher syntax and compile as:

| `tool_policy` entry | Compiled rule |
| --- | --- |
| `Bash` | tool glob `Bash`, no input matcher |
| `Bash(* --live*)` | tool glob `Bash` + `input_matcher` on `command` with regex `.* --live.*` |
| `mcp__github__*` | tool glob `mcp__github__*` |
| `*` | match-all (empty tools list) |

Parenthesized argument globs translate to an **unanchored** regex over the
`command` field (`*` → `.*`, other characters escaped), so `Bash(* --live*)`
blocks exactly the live invocations, not every `Bash` call.

## Security / trust model

| Source | Power | Safety |
| --- | --- | --- |
| `tool_policy` (kernel) | Compiles to deny/allow rules | Always safe — kernel-generated data. |
| `hooks.policy_rules` (author) | Adds deny/ask/allow rules | Safe — pure data; deny-wins guarantees an author can only tighten, never loosen, a kernel deny. |
| `hooks.observers` (author) | Routes extra events to `animus-hook` | Safe by construction — the author picks **events + a named built-in action**, never a raw shell command. The kernel emits the `animus-hook emit` command. |

Author raw hook commands (arbitrary shell in the agent's session) are **not**
supported. The observer surface is option (a) from the design space — a named
built-in action — chosen so an author-controlled profile can never smuggle an
arbitrary payload into a session it controls. If a future wave needs custom
observer automation, it should add named built-in actions, not open the door to
arbitrary commands.

## Provider coverage

`harness_hook_config_vector(tool)` classifies each provider's harness hook
config vector:

| Provider | Vector | Status |
| --- | --- | --- |
| claude | `Settings` (`--settings` file) | **Generated this wave** |
| codex, opencode | `HooksJson` | Classified; not yet generated (P3) |
| gemini | `GeminiHooks` | Classified; not yet generated (P3) |
| others | `None` | No harness hook vector |

Non-claude providers are a complete no-op for now — no policy file, no settings
file, no launch flags.

## Wiring (where the injector runs)

`inject_harness_hooks` is a kernel pub API in `animus-runtime-shared`, wired the
same way as the other per-phase launch-contract injectors
(`inject_agent_tool_policy`, `inject_response_schema_into_launch_args`,
`inject_workflow_mcp_servers`, …): the **workflow-runner** that assembles the
final per-phase launch contract calls it with the per-session run dir. That
assembler ships out-of-tree as `launchapp-dev/animus-workflow-runner-default`
(backed by `launchapp-dev/animus-runtime-shared`), so there is no in-tree
phase-assembly call site — by design (the in-tree workflow-runner binary/lib was
extracted in v0.5.1). End-to-end activation lands when that runner is bumped to
call `inject_harness_hooks` alongside the existing injectors; the same bump is
where the non-claude provider vectors (`HooksJson`, `GeminiHooks`) become
generated rather than just classified (P3).

## Kill-switch

Set `ANIMUS_DISABLE_HARNESS_HOOKS=1` (any non-empty value) to make
`inject_harness_hooks` a complete no-op: no policy file, no settings file, and
no `--settings` launch flag. See
[configuration.md](configuration.md#plugin-kill-switches).
