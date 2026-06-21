# RFC: v0.6 Approval Protocol

Status: accepted (Sami, 2026-06-20). Supersedes the ad-hoc, git-destructive-only
approval records and the claude-only permission gate.

## Goal

Make approval a first-class, **protocol-layer** infrastructure rather than a
voluntary escalation buried in one CLI handler. Two halves with a clean split:

- **Kernel owns the DECISION.** Policy modes (manual / approve-everything /
  deny / LLM auto-approve), the LLM judge, human escalation, and the unified
  inbox live in the kernel behind a stable contract.
- **Plugins own the INTERCEPTION.** Routing each tool call / action through the
  contract is the provider plugin's job — "in the end of the day it's the
  plugins' job to implement this." The kernel guarantees the contract + the
  endpoint + the decision engine so any provider can comply.

Approvals and questions are ONE infrastructure: they share the interaction
store and inbox, and in auto (LLM) mode the judge both **gates tool calls** and
**answers questions** from context (full autopilot).

## The contract (protocol layer)

1. **Approval back-channel** — `animus.agent.request_approval` (MCP) is THE
   approval contract. A provider that gates tool calls invokes it per tool call
   with `{ agent_id?, tool_name, input, action? }` and receives an SDK-shaped
   verdict in the result text: `{ behavior: "allow", updatedInput }` or
   `{ behavior: "deny", message }`, with the legacy `{ tool, result: { decision,
   source, message } }` envelope alongside. `source` is `policy` | `llm` |
   `human` | `timeout`.
2. **Capability + endpoint** — the session protocol's
   `SessionCapabilities.supports_permissions` declares a backend gates tool
   calls; the kernel passes the approval MCP endpoint + `permission_mode` to such
   backends. claude wires this natively as `--permission-prompt-tool`; other
   providers implement an equivalent hook (their plugin's responsibility).
3. **Question back-channel** — `animus.agent.ask` / native `AskUserQuestion`
   share the same interaction store and the `animus.interactions.{list,answer}`
   inbox, so approvals and questions surface and resolve through one surface.

## Decision engine (kernel)

`ApprovalPolicy` (per-agent profile; daemon-level default planned) — evaluated
on the request's `tool_name` (else `action`):

- `auto_deny` glob list  → **Deny** (wins on overlap, fail closed)
- `auto_allow` glob list → **Allow**
- otherwise `default`:
  - `ask`  — **manual**: escalate to a pending human interaction (inbox)
  - `allow` — **approve everything** ("dangerous" mode): auto-allow
  - `deny`  — auto-deny
  - `llm`   — **auto-approve**: a judge model reads the tool call (+ optional
    context) and returns allow/deny; **questions** are answered by the judge
    from context instead of escalating. `evaluator_model` picks the judge
    (defaults to the agent's own model); `evaluator_instructions` appends an
    operator rubric to the built-in conservative judge prompt. The judge runs
    as a one-shot provider session with **no MCP endpoint** (no tools → cannot
    recurse into approval). **Any** evaluator failure (no model, session error,
    unparseable verdict) falls back to manual `ask` — an LLM outage never
    silently auto-allows or hard-denies.

## Question integration (BOTH)

- **Unified inbox** — approvals + questions already share the interaction store
  + `animus.interactions.{list,answer}`. One answer path resolves either.
- **LLM auto-answer** — in `llm` mode, `animus.agent.ask` is answered by the
  judge from context (flat answer or option selection) rather than escalating;
  `ask` (manual) mode still escalates to a human. Same fail-safe: any failure
  falls back to a human interaction.

## Plugin responsibility (out-of-tree, tracked separately)

Per-provider tool-call interception is plugin work:

- **claude** — `--permission-prompt-tool mcp__animus__animus_agent_request_approval`
  already routes every gated tool call through the contract. Works today.
- **codex / gemini / opencode / portal** — route tool calls through
  `request_approval` (or a native permission hook that calls it). Each provider
  plugin implements this; the kernel only guarantees the contract + endpoint.

The kernel does NOT try to force interception on providers that don't declare
`supports_permissions`; "applies to all tool calls" is realized by the provider
plugins implementing the hook, not by kernel magic.

## Status

- Decision engine — implemented (`ApprovalPolicyDefault::Llm` + `evaluator_model`
  / `evaluator_instructions`, `ApprovalPolicyDecision::Evaluate`, the in-process
  judge + JSON verdict parser, wired into `request_approval`). Tests green.
- LLM auto-answer for `animus.agent.ask` — this change.
- Contract docs (`request_approval` modes, `permission_mode`/`supports_permissions`)
  — this change.
- Provider interception beyond claude — follow-up plugin work, per-provider repos.
