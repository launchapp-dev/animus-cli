# Provider Runtime & Approval — unified architecture

**Date:** June 2026
**Status:** Design — Proposal (supersedes the bespoke per-CLI approval-hook sketch)
**Companions:** [`acp-provider-client.md`](./acp-provider-client.md) (Animus as ACP client), [`acp-integration.md`](./acp-integration.md) (Animus as ACP server).
**Related memory:** approval wiring gap, per-provider approval hooks, claude approval wiring.

## 1. Context

Animus drives external coding-agent CLIs (claude / codex / gemini / opencode / oai) as out-of-tree `provider` plugins. Three problems surfaced while making approvals real on hardware:

1. **Approval gating was inert, fragile, and heterogeneous.** The kernel set `extras.approvals` but the plugin host dropped it (fixed); the wiring was stranded on a dead protocol branch (fixed → trunk tag `v0.1.16`); and each CLI needs a *different* gating mechanism.
2. **Every provider re-implements the same scaffolding** — lifecycle, streaming, cancellation, logging, and approval interception — once per repo.
3. **One-shot CLI print mode (`claude -p`, `codex exec`, `gemini -p`) is less flexible** than persistent server/session modes: it re-spawns per turn and, critically, several CLIs **cannot gate tool calls in print/exec mode at all** (verified: `codex exec` forces `approval:never`; `gemini --approval-mode default` headless auto-denies; `opencode run` auto-rejects "ask"). Persistent session protocols expose a **native permission callback** and give multi-turn / streaming / interrupt.

**Goal:** one common provider library owning all cross-cutting concerns (including **logging** and **approval routing**); thin per-provider transports that prefer **persistent, gated session modes**; a **single approval decision core**; and **ACP as the standard on-ramp** so future harnesses plug in free.

## 2. Principles

- **DRY runtime.** Cross-cutting concerns live once in `animus-plugin-runtime`. A new provider = a thin transport + a declared gating mechanism.
- **Prefer persistent gated modes over one-shot print.** Use a CLI's session/server protocol where it exists; fall back to print only where it doesn't.
- **One decision core.** Every gating mechanism funnels into `decide_approval` (policy + LLM judge + human escalation), never a parallel implementation.
- **ACP is the universal standard on-ramp;** native paths only for CLIs that don't speak it.
- **Never override the operator's CLI config** (`CLAUDE_CONFIG_DIR`/settings). The gate is authoritative via the protocol callback, not by mutating user settings (we *warn* when an operator's global auto-approve would bypass a CLI's own gate).

## 3. The common lib — `animus-plugin-runtime`

This crate already centralizes the plugin shell (`run_provider(ProviderInfo, backend)` + the `ProviderBackend` trait: stdio JSON-RPC lifecycle, `--manifest`/`--help`, `agent/run|resume|cancel`, streaming notifications, cancellation). It should additionally own, **consistently for every provider**:

- **Logging.** Install a log forwarder in the *live* `run_provider` so plugin `tracing` logs stream as log frames over the JSON-RPC channel to the host, which persists them to scoped log storage (`animus-log-storage`), queryable via `animus logs` / the logs MCP tools. *(Today the forwarder exists only in `session_provider.rs`, which §6 shows is not compiled — that gap is the immediate fix.)*
- **Approval routing.** A single seam where, when `extras.approvals` is set, the transport's gating callback is wired to `decide_approval`. The lib provides the plumbing + the decision call; each transport declares only *its* mechanism.

## 4. Per-provider transport + gating matrix

| Provider | Drive mode | Gating mechanism → `decide_approval` | Status |
|---|---|---|---|
| **claude** | `claude -p` (Agent SDK / print) — *or* **ACP** via an adapter | `--permission-prompt-tool` → MCP `request_approval`; *or* ACP `session/request_permission` | ✅ native shipped; ACP = future consolidation |
| **gemini** | **ACP** (`gemini --acp`) | `session/request_permission` | ✅ built — `animus-provider-acp` (`848e80d`, 36 tests, live opencode + mock) |
| **opencode** | **ACP** (`opencode acp`) | `session/request_permission` | ✅ built — `animus-provider-acp` (same), live opencode turn |
| **codex** | `codex mcp-server` (persistent) | exec / apply-patch approval **elicitation** | ✅ built — `animus-provider-codex-mcp` (`8ff2404`, 39 tests, LIVE codex 0.140.0 deny/allow) |
| **oai** | our own harness | native in-process hook | ✅ built — `animus-provider-oai` (`e92f8b3`, 29 tests, fail-closed) |

**Status (2026-06-21): all five core providers gate, all fail-closed, all through the one `decide_approval` keystone.** Remaining is release/rollout (gated): create remotes + push the two new provider repos + the oai commit; cut a new `animus-protocol` tag bundling the logging port + the `run_provider` approvals-carry fix (`f6ce11f`) so providers repin to trunk; merge the ao-cli worktree keystone via a `release/v0.6.0` PR.

claude can **also** be driven over ACP (so it joins the single ACP path with gemini/opencode), via one of two adapters that wrap the Claude Agent SDK and forward `session/request_permission`:
- `zed-industries/claude-code-acp` — TypeScript, mature (the adapter Zed ships), adds a Node process hop.
- `claude-code-acp-rs` (soddygo, v0.1.x) → `claude-code-agent-sdk` (soddygo, v0.1.39) — **pure Rust**, no Node. But note the chain: `claude-code-agent-sdk` itself just **shells out to the `claude` CLI** (same approach as our `ClaudeSessionBackend`), it's **single-maintainer + early**, and hasn't been updated since Feb 2026 while the claude CLI is 2.1.181 — version-drift risk for a flagship dependency. Treat these as **reference implementations**, not load-bearing deps; if we want persistent claude streaming we can add it to our own session-backend (which we control).

The native `claude -p` + `--permission-prompt-tool` path is already shipped and lowest-risk, so it stays claude's **default**; claude-over-ACP is validated through `animus-provider-acp` alongside gemini/opencode and can become the unified path once proven.

Not used for gated driving: `claude mcp serve` / `codex mcp-server` **tool-provider** surfaces expose a CLI's *tools* to a client, but the tools run in the provider's process where Animus can't gate them (governed by the operator's own CLI settings). They're useful for letting Animus agents *borrow* a CLI's tools — a separate capability, not an approval path.

## 5. The keystone — `decide_approval`

A single `pub(crate) async fn` (extraction in progress) that runs the entire decision: identity-pinned agent profile → `ApprovalPolicy::evaluate` (auto_deny → auto_allow → default of ask/allow/deny/llm) → LLM judge for `llm` mode (audit-gated allow, fail-closed) → human escalation/inbox for `ask`. Returns allow / deny (+ optional edited input). Called by all four gating mechanisms above, so behavior is identical regardless of provider or transport.

## 6. ACP as the standard on-ramp — `animus-provider-acp`

A generic ACP **client** provider (per [`acp-provider-client.md`](./acp-provider-client.md)) implementing the `Client` trait: session lifecycle, `session/update` → `SessionEvent`s, `session/request_permission` → `decide_approval`, plus `fs/*` + `terminal/*` callbacks. It drives **any** ACP agent by binary+args config (`gemini --acp`, `opencode acp`, future harnesses) — no new repo per harness, and every future ACP harness gets gating for free.

**ACP coverage:** with the claude adapters (above), ACP can drive **claude + gemini + opencode** — 3 of 5 providers — through the *one* client + `session/request_permission`. Only **codex** (no ACP) and **oai** (ours) stay outside ACP. This is the strongest available consolidation toward a single driver.

**Decision (this cycle):** keep the existing per-CLI plugins (claude/codex/gemini/opencode/oai) and add `animus-provider-acp` as **scaffolding + the gating path for gemini/opencode** (and a claude-over-ACP validation target). ACP is additive now; it can become the primary path for ACP-speaking CLIs (claude/gemini/opencode/future) once proven, at which point the bespoke claude `--permission-prompt-tool` path is retired.

## 7. Known issue to resolve (from the `v0.1.16` de-strand merge)

`animus-plugin-runtime/src/session_provider.rs` was merged in but **is not declared in the module tree** (`lib.rs` has no `mod session_provider;`), so it is **not compiled** — which is why its dangling imports (`install_log_forwarder`, …) didn't fail the build. It carries the richer runtime (the native `agent/respond` interaction channel) and the *only* log-forwarder call. Resolution: **(b) port the log forwarder into `run_provider` and remove the orphan now** (the native `agent/respond` channel stays deferred), and **revisit (a) adopting the richer runtime** when building `animus-provider-acp`, which needs persistent-session handling anyway.

## 8. Phasing

1. **Keystone** `decide_approval` extraction *(in progress)*.
2. **Logging fix** — install the forwarder in `run_provider`; resolve the orphan (§7).
3. **`animus-provider-acp`** — ACP client; gemini + opencode gating via `session/request_permission`.
4. **codex** — `mcp-server` elicitation rearchitecture.
5. **oai** — native in-process hook.
6. **Capability matrix doc + per-transport tests**; provider re-releases against `v0.1.16`; coordinated fleet rollout (gated by the user).

## 9. Non-goals / constraints

- No single universal *driving* protocol exists today; ACP is the closest standard and the chosen on-ramp.
- Do not override operator CLI config; warn instead.
- Kernel stays Rust-only; providers remain out-of-tree plugins consuming this common lib.
