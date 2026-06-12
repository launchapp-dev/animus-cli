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
  created_at, question?, action?, options?, tool_name?, arguments?,
  questions?: [{ question, header?, options: [{ label, description? }],
  multi_select }], suggestions?, timeout_secs?, suspended, status:
  pending|answered|expired, answer?, answer_message?, answers?:
  { <question text>: <string | [string]> }, response?, updated_input?,
  updated_permissions?, answered_at?, answered_by? }`. The structured fields
  (`questions`/`answers`/`response`/`suggestions`/`updated_*`) are additive —
  pre-existing records load unchanged; flat `question`/`options`/`answer` stay
  for `animus.agent.ask` back-compat. Cross-process flock'd read-modify-write,
  atomic tmp+rename writes (same pattern as `agent_state.rs`).
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
  - **Third criterion — crash-replayability:** a parked **block**-mode call
    lives only in the holding process; if the daemon or runner crashes while
    it waits, the call is lost and the run cannot replay it (the agent must
    re-ask on a fresh run). **Suspend** persists the pending interaction id
    into the phase session checkpoint, so a crash mid-wait is recoverable —
    the answer still resumes the workflow after restart. For any path where a
    crash window matters (long-lived, daemon-driven, or expensive-to-restart
    phases), prefer suspend on durability grounds alone. The wait-mode
    failure semantics (block timeout → proceed/best-judgment for questions,
    deny/fail-closed for approvals) are tabulated in
    [MCP Tools — Interactions](../reference/mcp-tools.md#interactions-2-tools)
    and [the agents guide](../guides/agents.md#human-in-the-loop-questions-and-approvals).
- **Inbox:** `animus agent interactions list|show|answer <id>` (`--text` for
  questions, `--allow|--deny [--message]` for approvals), `animus.cli.v1` JSON
  envelopes. Non-blocking `animus.interactions.list` / `animus.interactions.answer`
  MCP tools for UI consumers (desktop app, web UI).
- **Identity defaulting:** tools default `agent_id` (and workflow/task ids) from
  `ANIMUS_AGENT_ID` / `ANIMUS_WORKFLOW_ID` / `ANIMUS_TASK_ID` env vars injected via
  the runtime contract and `.mcp.json` env, so prompts don't need to teach agents
  their own identity.

## Native channel — Claude Agent SDK permission-prompt-tool conformance

When the claude transport wires `--permission-prompt-tool
mcp__animus__animus_agent_request_approval` (Layer 2 below), the claude CLI
invokes that MCP tool for **every** gated tool call — ordinary tool approvals
AND the native `AskUserQuestion` clarifying-questions tool. The wire contract
(verified against the claude CLI v2.1.175 binary and
<https://code.claude.com/docs/en/agent-sdk/user-input>):

- **Input to the tool:** `{ tool_name: string, input: object, tool_use_id?: string }`
  — nothing else. No `agent_id`, no `action`. Identity comes from the server
  pin (`animus mcp serve --agent-id` / `ANIMUS_MCP_AGENT_ID`); `action` is
  derived as `use tool <tool_name>` when absent.
- **Result the CLI parses:** the FIRST text content block of the tool result
  must be a JSON string matching the SDK permission-result schema:
  - allow: `{ "behavior": "allow", "updatedInput": <object, REQUIRED>,
    "updatedPermissions"?: PermissionUpdate[] }`
  - deny: `{ "behavior": "deny", "message": <string, REQUIRED>, "interrupt"?: bool }`
  Unknown top-level keys are stripped (non-strict schema), so Animus keeps its
  legacy `{ tool, result: { decision, source, message?, answered_by?, id? } }`
  envelope alongside the SDK keys in the same JSON object — additive
  back-compat for anything reading the old shape.
- **Ordinary tool allow:** `updatedInput` MUST carry the original tool input
  (pass-through) or an operator-modified replacement. The kernel stores the
  original `input` on the interaction record (`arguments`) and echoes it back;
  `animus agent interactions answer <id> --allow --updated-input '<json>'`
  substitutes a modified input.
- **AskUserQuestion:** `tool_name == "AskUserQuestion"` routes to a structured
  **Question** record (`kind: question`, `questions[]` parsed from
  `input.questions`, raw input preserved verbatim). It bypasses the approval
  policy (questions are not approvals) and surfaces in the same inbox /
  notifier flow as `animus.agent.ask`. The answer emits
  `{ "behavior": "allow", "updatedInput": { questions: <original array>,
  answers: { "<question text>": <label | [labels] | free text> },
  response?: <freeform> } }`.
- **Suggestions / remember:** an optional `suggestions` array
  (SDK `PermissionUpdate[]`) in the prompt-tool input is stored on the record
  and rendered by `show`. Answering `--allow --remember` echoes the
  `localSettings`-destination subset back as `updatedPermissions` (the SDK
  "Approve and remember" flow). There is no permission-rule engine in the
  kernel — this is a pure echo.
- **Block mode vs SDK pending:** the SDK `canUseTool` callback may stay
  pending indefinitely; Animus's block mode parks the prompt-tool call with a
  timeout (default 600s, max 3600s) and **denies on timeout** (fail closed;
  questions deny with a proceed-on-best-judgment message).
- **Suspend replaces SDK `defer`:** in suspend mode (workflow-pinned server)
  a native prompt-tool call cannot return `pending` — the CLI only understands
  `behavior: allow|deny`. The kernel answers `behavior: "deny"` with the
  end-your-turn instruction in `message`, suspends the record, and pauses the
  workflow. **Suspend-mode native questions resume via session feedback, not
  via the original tool result**: the answer path resumes the provider session
  with feedback text carrying the per-question answers
  ("The user answered your questions:\n- \"Q\": label …") or the freeform
  response ("The user responded to your questions: …"). Voluntary
  (agent-initiated) suspend calls keep the `{ status: "pending", … }` payload.

## Layer 2 — transport wiring (animus-session-backend, v0.1.13.5)

1. **claude:** when the request carries `extras.approvals: true` (kernel sets it
   when the agent profile has an `approval_policy` or `--approvals` is passed),
   the transport adds `--permission-prompt-tool
   mcp__animus__animus_agent_request_approval`. Every native permission decision
   then routes through the kernel tool — enforced, not voluntary. The transport
   wires the tool NAME only; the request/response contract above is implemented
   entirely in the kernel tool.
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

## Approval gates: what goes where today

Two separate gate mechanisms exist in Animus. Understanding which to use:

### `animus approval` — destructive git operations

`animus approval` gates _operator-initiated_ destructive git operations:

| Operation | Gate |
|---|---|
| `animus git push --force` | `animus approval request --operation-type force_push` |
| `animus git worktree remove` | `animus approval request --operation-type remove_worktree` |
| `animus git worktree prune` | `animus approval request --operation-type prune_worktrees` |
| Hard reset, clean untracked | `--operation-type hard_reset / clean_untracked` |

The caller requests an approval record, a human responds via
`animus approval respond --request-id <ID> --approved`, then passes
`--confirmation-id <ID>` back to the destructive command. Records are stored in
the project-local git-confirmations store; they are _not_ agent inbox interactions.

### `animus agent interactions` — agent human-in-the-loop

Agents (spawned `claude` / `codex` / `gemini` / `opencode` sessions) use the
kernel MCP tools `animus.agent.ask` and `animus.agent.request_approval` to
pause and ask a human mid-run. These land in the **agent interactions inbox**:

```sh
animus agent interactions list
animus agent interactions answer <ID> --allow          # approval
animus agent interactions answer <ID> --allow --remember               # + echo localSettings suggestions
animus agent interactions answer <ID> --allow --updated-input '{"command":"rm -rf build/sandbox"}'
animus agent interactions answer <ID> --deny --message "too risky"
animus agent interactions answer <ID> --text "Use the v2 API"  # question
animus agent interactions answer <ID> --select "Format=Summary" \
  --select "2=Introduction,Conclusion" --text "keep it short"   # structured (AskUserQuestion)
```

For structured questions, `--select` takes the question text, its `header`,
or its 1-based index on the left of `=`; comma-separate labels (or repeat
`--select` for the same question) for multi-select. Bare `--text` maps to the
single question's answer on one-question records and to the freeform
`response` on multi-question records.

This store is completely separate from `animus approval`. The two surfaces
do not share records.

### RBAC note

The `--as <PRINCIPAL>` flag is honored today on all approval and secret
mutation surfaces for _audit attribution_ — the declared principal is written
to the audit log. Enforcement (rejecting operations from principals that lack
the required role) is planned for v0.6. Until then the flag is honor-system:
a caller can declare any principal. This is documented behavior, not a bug.

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
