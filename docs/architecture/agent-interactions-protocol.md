# Agent Interactions Protocol (questions + approvals)

Status: kernel Layers 1 + 4 implemented (interactions store, block/suspend wait
modes, workflow pause-on-suspend + resume-with-answer, notifier fan-out);
Layer 2/3 protocol surface targeted at animus-protocol v0.1.13.5.

Animus agents (spawned `claude` / `codex` / `gemini` / `opencode` CLI sessions) need
two human-in-the-loop primitives:

1. **Questions** — block or suspend on a clarifying answer mid-run.
2. **Approvals** — request permission for a sensitive action, governed by a
   per-agent policy, fail-closed.

The design rides the kernel-hosted MCP server wherever possible: the kernel hosts
`animus mcp serve`, so MCP-path interactions need **zero plugin-protocol changes**.
New protocol surface exists only where a provider has a native HITL channel that
bypasses MCP.

## Layer 1 — interactions store, tools, inbox (kernel, no protocol change)

- **Store:** `~/.animus/<repo-scope>/interactions/` — one JSON record per
  interaction: `{ id, kind: question|approval, agent_id, workflow_id?, task_id?,
  session_id?, created_at, payload, status: pending|answered|expired, answer?,
  answered_at?, answered_by? }`. Cross-process flock'd read-modify-write, atomic
  tmp+rename writes (same pattern as `agent_state.rs`).
- **MCP tools (agent-facing):**
  - `animus.agent.ask` — `{ agent_id, question, options?, timeout_secs?, wait? }`
  - `animus.agent.request_approval` — `{ agent_id, action, tool_name?, arguments?,
    timeout_secs?, wait? }`
  - Both consult the agent profile's `approval_policy` first (approvals only):
    `auto_allow` / `auto_deny` lists short-circuit without escalation; `default`
    (ask | allow | deny) governs unmatched actions.
- **Wait modes:**
  - `wait: "block"` (default for ad-hoc `animus agent run` / `animus chat`) —
    the tool call parks, emitting MCP progress notifications every ~15s to hold
    client timeouts open, until answered or timeout. Approval timeout ⇒ **deny**
    (fail closed). Question timeout ⇒ structured error.
  - `wait: "suspend"` (default for daemon workflow phases) — the tool returns
    immediately with `{ status: "pending", interaction_id, instruction }` telling
    the agent to wrap up its turn cleanly. The phase checkpoints; the session
    resumes when answered (Layer 4).
  - The kernel selects the default per run path; agents may override only from
    suspend→block, never block→suspend silently.
- **Choosing block vs suspend:** use **block** when a human operator is sitting
  at the terminal for the run (ad-hoc `animus agent run` / `animus chat`) and a
  synchronous answer within the timeout is realistic — the call holds the
  process open and resolves in place. Use **suspend** for asynchronous,
  daemon-driven workflow phases where no one is watching live: the phase returns
  immediately, the workflow pauses to its inbox, and answering later (via
  `animus agent interactions answer` or a notifier-delivered link) resumes the
  run with the decision threaded in as feedback — no pool slot or process stays
  parked waiting. When in doubt for a workflow phase, prefer suspend; reserve
  block for genuinely interactive sessions.
- **Inbox:** `animus agent interactions list|show|answer <id>` (`--text` for
  questions, `--allow|--deny [--message]` for approvals), `animus.cli.v1` JSON
  envelopes. Non-blocking `animus.interactions.list` / `animus.interactions.answer`
  MCP tools for UI consumers (desktop app, web UI).
- **Identity defaulting:** tools default `agent_id` (and workflow/task ids) from
  `ANIMUS_AGENT_ID` / `ANIMUS_WORKFLOW_ID` / `ANIMUS_TASK_ID` env vars injected via
  the runtime contract and `.mcp.json` env, so prompts don't need to teach agents
  their own identity.

## Layer 2 — transport wiring (animus-session-backend, v0.1.13.5)

1. **claude:** when the request carries `extras.approvals: true` (kernel sets it
   when the agent profile has an `approval_policy` or `--approvals` is passed),
   the transport adds `--permission-prompt-tool
   mcp__animus__animus_agent_request_approval`. Every native permission decision
   then routes through the kernel tool — enforced, not voluntary.
2. **codex / gemini / opencode:** no exec-mode approval hook. Wiring is
   `permission_mode` mapping (already shipped) plus system-prompt injection: when
   approvals are enabled the kernel appends an instruction block directing the
   agent to call `animus.agent.request_approval` before destructive actions and
   `animus.agent.ask` for blocking questions. Voluntary compliance; documented as
   such.
3. **Suspend handshake:** a suspended interaction's `instruction` text tells the
   agent to summarize in-progress state and end its turn; transports do not need
   changes for suspend — ending a turn is the normal completion path.

**`permission_mode` × `approval_policy` compose (not a conflict).** A profile
may declare both. `permission_mode` is the **transport-level guard** — it rides
the typed `SessionRequest.permission_mode` and maps to the provider CLI's own
permission flag (claude `--permission-mode`, codex `-c approval_policy`, gemini
approval mode), governing whether the provider acts autonomously or escalates at
all. `approval_policy` is the **kernel inbox layer** — its presence is exactly
what sets `extras.approvals: true` (above), routing escalations through
`animus.agent.request_approval` where `ApprovalPolicy::evaluate` auto-allows /
auto-denies / asks. The two are orthogonal and both apply: `permission_mode`
decides whether the provider asks; `approval_policy` decides what happens to the
asks that reach the kernel.

## Layer 3 — plugin-protocol surface (additive, v0.1.13.5)

Only for providers with native HITL channels (codex app-server approvals; future
interactive providers). Capability-gated; absent capability changes nothing.

- **Notification (plugin → host):** `agent/interactionRequested`
  `{ interaction_id, session_id, kind, payload { action?, tool_name?, arguments?,
  question?, options? }, expires_at? }` — host writes it into the same store, same
  inbox, same policy engine.
- **Request (host → plugin):** `agent/respond`
  `{ interaction_id, session_id, response { decision?, answer?, message? } }`
  → `{ accepted }`. Plugin forwards to its CLI's native channel.
- **Capability:** `"agent/respond"` in the plugin manifest `capabilities` array;
  the host only routes responses to plugins that declare it. Unknown notifications
  are ignored by the host (verified additive-safe).
- **SessionEvent additions (kernel enum, additive):**
  `InteractionRequested { id, kind }` / `InteractionResolved { id, decision }` so
  chat and agent-run streams and the workflow events broadcaster render a
  "waiting on approval" state instead of silence.

## Layer 4 — workflow suspend/resume (kernel)

Decision: suspended phases use the **chat continuity model** — the agent process
exits cleanly and the provider session is resumed with the answer, rather than
keeping a process parked.

1. Agent in a workflow phase calls a tool with (default) `wait: "suspend"` → tool
   records the interaction (with the phase's provider `session_id` from the
   dispatch record) and returns `pending`.
2. The workflow runner observes the pending interaction for its phase and
   checkpoints the workflow to `BlockedAwaitingDecision` with
   `reason: interaction_pending` and the interaction id + session id in checkpoint
   context — visible in `animus workflow list` / `animus status`, no pool slot
   held.
3. Human answers via inbox / MCP / notifier deep link.
4. Answering an interaction whose record carries a workflow id triggers the
   workflow resume path (the same detached-runner spawn used by
   `animus workflow resume`): the runner resumes the provider session
   (`agent/resume`, session_id from checkpoint context) with a resume prompt
   carrying the decision: "Approval granted/denied for `action`: `message`.
   Continue." — mirroring chat's resume-with-history-XOR-session rule.
5. Auto-resume is gated by the daemon being up; with no daemon, `animus agent
   interactions answer` prints the `animus workflow resume <id>` command to run.

Failure semantics:

| Scenario | Behavior |
| --- | --- |
| Approval timeout (block mode) | deny, fail closed |
| Question timeout (block mode) | structured error, agent proceeds on judgment |
| Suspended workflow, answer never arrives | workflow stays `BlockedAwaitingDecision`; surfaced by `animus status`; no auto-expiry in v1 |
| Agent process dies while blocked | record stays pending, inbox marks the agent dead, answering warns no-op |
| Daemon restart | store is on disk; suspended workflows resume normally on answer |
| Two answers race | flock RMW; first commit wins, second errors `already answered` |

## Notifier push (first wave)

New pending interaction ⇒ daemon event (`interaction_created`) ⇒
`notifier_dispatcher` fan-out to installed notifier plugins (Slack / Telegram /
HTTP). Payload: kind, agent, action/question summary, and the answer command
(`animus agent interactions answer <id> --allow`). Answered/expired transitions
emit `interaction_resolved`.

## Rollout sequencing

1. Kernel Layer 1 (block mode) + `permission_mode` exposure — in flight.
2. animus-protocol **v0.1.13.4** (MCP pass-down) — in flight, unchanged; this
   feature does not fold into it.
3. Kernel Layer 4 (suspend/resume + checkpoint integration + notifier dispatch) —
   **done**: `animus mcp serve --workflow-id` / `ANIMUS_MCP_WORKFLOW_ID` pin,
   suspend wait mode, workflow pause + session-checkpoint stamp, answer-path
   detached-runner resume with feedback, daemon-tick notifier fan-out of
   `interaction_*` events.
4. animus-protocol **v0.1.13.5**: Layer 2 transports + Layer 3 surface, one tag.
   Provider plugins release once against 13.4 + 13.5 together; the workflow-runner
   pin bump (which also carries the OAuth bearer-headers fix) rides the same wave.
5. Codex-native `agent/respond` implementation: capability-gated follow-up.
