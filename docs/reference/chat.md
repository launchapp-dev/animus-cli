# Chat

v0.5.10 adds `animus chat` — multi-turn conversations with a provider tool (claude / codex / gemini / opencode / any installed `provider_backend` plugin). It reuses the same `SessionBackendResolver` that drives `animus agent run`, so every provider plugin works with chat unchanged.

## Continuity model: providers own continuity, Animus owns a thin portable layer

This is the load-bearing design decision. Read it before changing the turn loop.

The wrapped CLI tools already have **native session management**. Claude Code stores `~/.claude/projects/<proj>/<session-id>.jsonl`; codex and gemini keep their own native transcripts. The provider contract surfaces this directly: `SessionRun.session_id` comes back out of a run, and `SessionRequest.extras.session_id` goes back in to resume. When you hand a tool its own `session_id`, it replays its full native history itself.

So **continuity is the provider's job, not Animus's.** Per turn, Animus does strictly **one** of two things — never both:

1. **Session alive** — the conversation has a stored `session_id` from a prior turn *with the same tool*:
   - prompt = ONLY the new user message.
   - `extras.session_id = <stored>`. The tool resumes its own native session, which already carries all prior context.
   - Animus does NOT replay history into the prompt.

2. **No live session** — brand-new conversation, OR the provider returned no `session_id`, OR a resume attempt failed, OR the tool changed mid-conversation:
   - prompt = full rendered history from Animus's stored messages.
   - no `extras.session_id`. This is the **only** case where Animus replays.

### Why never both

Doing both is a bug. If Animus passed `extras.session_id` (so the tool resumes its native history) AND replayed the rendered history into the prompt, the tool would see the conversation twice — once in its resumed session, once in the prompt — doubling its context window and cost on every turn. The turn loop couples the prompt shape and the `session_id` so the two modes can never overlap.

## What Animus's conversation store IS (and isn't)

Animus's store is **not** the replay engine for live sessions. It is the portable + queryable + fallback layer:

- **Portable / queryable record.** The tool's native session is tool-specific and machine-local. Animus's normalized `ChatMessage` event log is what `animus chat get` / `chat list` read, what `animus chat send --stream --json` emits, and what `animus cost conversation` aggregates — all provider-agnostic.
- **Ordered assistant timelines.** Assistant turns now persist a `blocks` timeline in arrival order (`text`, `thinking`, `tool_call`, `tool_result`) so reloads can reconstruct the same interleaved view the live stream showed. Thinking blocks keep the accumulated reasoning text when the provider emits it, and older messages that predate this field still load cleanly and fall back to the aggregated `content` text.
- **Resume fallback.** When no native session is alive (case 2 above), Animus replays its stored history into the prompt.
- **Continuity and identity pointer.** Conversation meta stores the current `session_id` + `tool` + `model`, optional canonical `agent_id`, and a monotonic `revision`. The loop captures `SessionRun.session_id` after every turn; a bound `agent_id` is re-resolved on every continuation.

Animus still persists every turn — the user message before the provider call (crash safety), the assistant message after — but that store serves portability / query / fallback, **not** live-session prompt replay. There is no double-bookkeeping of context into the prompt.

### Resume-failure fallback

If a resume turn (case 1) comes back with an error indicating the native session is gone or invalid (`session not found`, `session expired`, `could not resume`, ...), the loop falls back to case 2 (replay full history, drop the stale `session_id`) and retries **exactly once**. If the replay attempt still reports a stale session, the turn fails. A tool change mid-conversation is detected up front (`meta.tool != requested tool`) and forces a replay without ever reusing the prior tool's `session_id`.

## CLI surface

```bash
# Start an empty conversation (prints the conversation id)
animus chat new [--id <id>] [--title <title>] \
  [--actor-json <json>] [--as-user <user-id>] [--visibility private|shared]

# Send a turn (creates a conversation if --conversation is omitted)
animus chat send "your message" \
  [--conversation <id>] [--expected-revision <n>] [--agent <agent-id>] \
  [--tool claude] [--model <model>] [--cwd <path>] \
  [--stream] [--title <title>] [--actor-json <json>] \
  [--as-user <user-id>] [--visibility private|shared]

# Probe the application contract without starting a provider
animus --json chat capabilities

# Durable application send (all three scoped flags are required together)
animus --json chat send "your message" --conversation <id> \
  --actor-json '{"user_id":"alice","tenant_id":"workspace-a","claims":[]}' \
  --as-user alice \
  --idempotency-key <key> \
  --require-shared-authority  # required by Portal/multi-replica callers

# Read a transcript, optionally returning a bounded slice
animus chat get <id> [--actor-json <json>] [--as-user <user-id>] [--limit <n>] [--offset <n>]

# List conversations, most-recently-updated first, optionally paged
animus chat list [--actor-json <json>] [--as-user <user-id>] [--limit <n>] [--offset <n>]

# Set or clear a conversation title
animus chat rename <id> --title <title> [--actor-json <json>] [--as-user <user-id>]

# Permanently delete a conversation
animus chat delete <id> [--actor-json <json>] [--as-user <user-id>]

# Export a conversation transcript as Markdown or JSON
animus chat export <id> [--format markdown|json] [--output <path>] \
  [--actor-json <json>] [--as-user <user-id>]

# Search within the authenticated caller's conversation partition
animus chat search <query> [--limit <n>] [--case-sensitive] \
  [--actor-json <json>] [--as-user <user-id>]
```

`animus chat send --title` names a freshly-created conversation or renames the
target one before the turn runs; surrounding whitespace is trimmed, and an
empty string clears the stored title. `animus chat rename` applies the same
trimming and clear-on-empty behavior. `animus chat delete` is idempotent: a
missing conversation is treated as already deleted rather than an error.
`animus chat export` defaults to Markdown, can emit the full `{ meta, messages
}` JSON shape, and writes raw transcript content to stdout unless `--output` is
supplied.

`--agent` is a durable conversation binding, not a display hint. On an
auto-created conversation the canonical configured map key is stamped by the
create operation; on a pre-created legacy/unbound conversation it is stamped
under the turn lock before the user message. Later sends may omit `--agent` and
still reuse that exact profile's tool, model, system prompt/persona,
`tool_profile`, reasoning effort, permission mode, approval policy, MCP servers,
and tool policy on native-resume and replay-fallback paths. Passing a different
`--agent`, or continuing after the configured profile was renamed, deleted, or
became invisible in the actor/project scope, fails before message persistence or
provider execution. Conversations created before this field remain unbound and
never infer identity from title, owner, tool, or model.

Application layers should read `meta.revision` with `chat get`, then pass it as
`chat send --expected-revision <n> --conversation <id>`. The turn re-checks the
token after acquiring the conversation lock and the conversation-store
`save_meta` RPC reserves the operation with `expected_revision` compare-and-swap
before the first message append. A
binding/title/concurrent mutation between preflight and execution therefore
fails closed instead of running against stale identity.
`animus --json chat capabilities` advertises this as the exact
`send.identity_binding` contract. Application clients must derive `agent_id`
and `revision` from authorized conversation metadata; `client_selectable` is
false, so an untrusted request body may never choose or override the agent.

### Streaming and output modes

`animus chat send` selects its sink from the flags:

- `--json` → **JsonlStdoutSink**: one self-describing JSON object per line (`user_message_accepted`, `turn_started`, `text_delta`, `thinking`, `tool_call`, `tool_result`, `metadata`, `warning`, then exactly one `turn_completed` or `turn_failed`). The acceptance frame carries the canonical user `message_id` + `seq`. Terminal frames carry the operation id and both canonical user and assistant locators. `turn_failed` means the user message is durable even though the assistant did not complete; callers must not present it as an unaccepted send.
- `--stream` (no `--json`) → plain text deltas to stdout for an interactive session.
- neither → the final assistant turn is printed once the turn completes.

### Durable application idempotency and partial success

`chat send --idempotency-key` is intentionally restricted to an explicit
`--conversation`, transport-asserted `--actor-json`, and matching `--as-user`.
The actor must include both a non-empty `user_id` and `tenant_id`, and its
`user_id` must equal `--as-user`. The durable key is scoped by repository,
workspace, actor, and conversation. Reuse with an identical effective request
replays its canonical receipt, while a changed request returns
`idempotency_conflict` and a live lease returns `idempotency_in_progress`.

Durable operation authority follows the selected transcript backend. The file
store creates its SQLite/WAL reservation before the user transcript row. A
plugin store must advertise `conversation_operations_shared_v1`,
`conversation_operation_fenced_append_v1`, and all seven
`conversation/operation_*` RPCs. The fenced-append contract prevents a stale
lease holder from persisting an assistant turn after another host reclaims the
operation; its database clock, operation rows, and rotating lease tokens then
provide one authority shared by every CLI host.
A keyed send fails closed when a selected plugin lacks that capability. It
never silently falls back to host-local SQLite.

Portal and other multi-replica callers must also pass
`--require-shared-authority` with every keyed send. The per-send policy rejects
the file backend, so a plugin that disappears after startup cannot split
operations into host-local SQLite databases. A discovered plugin is still
exercised by the operation RPC on each send; process, handshake, or database
failures therefore fail the request before provider execution.

Once the user row is accepted, a retry never blindly starts the provider again:
an expired process is reconciled to the stored assistant row when present, or
to the terminal `assistant_interrupted` state otherwise. This avoids duplicate
agent/tool side effects. Provider errors persist `assistant_failed`; exact
retries replay the same bounded failure receipt. Use `animus --json chat
capabilities` as the stable Portal capability probe instead of scraping help.
The probe's live `backend` object reports the selected `kind`, `authority_mode`,
whether the shared capability was observed, readiness, and a stable
`error_code`. For a capable plugin, the command also requires the handshake to
declare all seven methods and performs bounded, read-only
`conversation/load_meta` and `conversation/operation_load` probes against a
guaranteed-missing key. This verifies process startup, method routing, and
authoritative database access without writing application data. Portal multi-replica mode is safe only when it reports
`kind: "plugin"`, `authority_mode: "shared_conversation_store_rpc"`, and
`ready: true`.

Admission uses two hashes. The first covers normalized caller intent and is
checked before mutable agent/profile/MCP resolution, so a terminal receipt can
replay even after configuration changes. A pending operation then binds exactly
once to the fully resolved execution snapshot. If that snapshot changes after
a crash, recovery holds the conversation lock: with no durable user row it can
safely rebind because no provider could have started; with a durable user row it
records `assistant_interrupted` and never repeats provider effects. The
conversation meta also carries an internal
`active_operation_id` reservation in the same revision CAS. It proves that the
same operation may resume if the process stopped after consuming the revision
but before appending the user row, and it is cleared on terminal completion or
reconciliation.

Recovery and failed-preparation cleanup hold the conversation lock and renew
the operation authority before scanning or clearing that reservation. Stable
message ids are preferred; an external store using the staged protocol can use
the reservation's canonical pre-append sequence plus role/content validation
when its wire message omits the additive id. If profile/MCP preparation fails
with no user row, the pending admission is released for a safe retry; if the
user row is already durable, the operation becomes `assistant_interrupted`
instead of repeating provider effects.

The in-tree file store persists stable message ids. The current external
`conversation_store` protocol still locates messages by canonical
`{conversation_id, seq}` and does not carry the additive message-id field; a
future protocol revision can preserve that field without changing this
operation contract.

## Cost

```bash
animus cost conversation <id>   # token + USD spend for one conversation
```

Per-turn `usage` and `cost_usd` are recorded on each assistant `ChatMessage` from the provider's metadata frames; `animus cost conversation` folds them into a per-conversation total.

## Ownership and visibility

Each conversation carries these identity/concurrency fields on its `ConversationMeta`:

- `owner` — the authenticated actor's user id. `None` remains valid for legacy
  unscoped local conversations.
- `visibility` — `private` (the default) or `shared`.
- `agent_id` — optional canonical configured profile id. Missing means deliberately unbound; it is never inferred.
- `revision` — monotonic optimistic-concurrency token (`0` for legacy metadata).
- `active_operation_id` — optional internal keyed-send reservation. It is
  omitted from conversation-list summaries and is not a client-selectable
  application field.

Every conversation-store verb accepts `--actor-json`. The transport actor is
authoritative: its `user_id` supplies the owner and access filter, and its
`tenant_id` supplies the storage partition. `--as-user`, when present, is only
a compatibility assertion and must exactly match `actor_json.user_id`; it
cannot create an identity by itself.

When a `conversation_store` plugin is installed, all chat data commands require
a non-empty actor `user_id` and `tenant_id`. The CLI places that tenant in the
flattened `ConversationScope.tenant_id` field and injects the complete actor at
the top level of every conversation RPC. The plugin SDK derives its
authoritative `CallContext` from that actor; legacy `owner` / `as_user` fields
are matching assertions only. A missing actor, a mismatched assertion, or a
cross-tenant attempt fails before a store call.

Without a plugin, omitting the actor is an explicit system/local mode that
retains the legacy repository-scoped store. An actor-scoped local command uses
the actor user for owner filtering; when `tenant_id` is present, the file store
uses a hashed tenant subdirectory so two tenants never share conversation
paths. Direct-id access to another user's private conversation is still
reported as `not found` to avoid existence probes.

For `chat list`, owner filtering happens before `--offset` / `--limit`, so
inaccessible rows neither consume page slots nor affect the visible page
shape. `chat get` applies those flags to the ordered `messages` array while
preserving the `{ meta, messages }` JSON shape. The current
`conversation_store` protocol has no server-side paging fields, so the backend
still returns the full candidate list or transcript before the CLI bounds its
output.

## Pluggable conversation store

Chat persistence is served by an **optional** `conversation_store` plugin role. With no plugin installed, the in-tree filesystem store (below) is used — chat works with zero plugins. When a `conversation_store` plugin is discovered, authenticated chat data ops (`create` / `load_meta` / `save_meta` / `append_message` / `load_messages` / `list` / `delete`) route to it over JSON-RPC instead; this is how an out-of-tree Postgres backend serves tenant-isolated history with real per-user ownership + sharing.

The role is optional: it is **not** a required preflight role, and the daemon never refuses to start without it. The tenant and shared-operation contracts are staged for the next additive `animus-protocol` release. Until that dependency is published and pinned, the CLI's bound request structs emit the exact candidate JSON shape while the workspace remains buildable against rc.11; no machine-local dependency override is required. The `Visibility` enum, `ConversationMeta`, and `ChatMessage` wire shapes match the on-disk `meta.json` / `messages.jsonl` shapes exactly. `conversation/create` can stamp `agent_id` atomically, and `conversation/save_meta` accepts `expected_revision` for compare-and-swap. A durable backend must persist and enforce `active_operation_id` in that same owner-scoped CAS; a different operation cannot replace a live reservation. Cross-process turn serialization (`try_lock_conversation`) is intentionally NOT on the wire — a DB-backed plugin uses a transaction or advisory lock per conversation and MUST enforce the revision precondition. Each plugin call spawns a host, runs one RPC, and reaps the host (`host.shutdown().await`).

## State layout

When no `conversation_store` plugin is installed, conversations live under the scoped runtime root:

- `~/.animus/<repo-scope>/chat/<conversation-id>/meta.json` — `ConversationMeta` (`agent_id` + `revision` + internal `active_operation_id`, the `session_id` / `tool` / `model` continuity pointer, counts, ownership, and timestamps).
- `~/.animus/<repo-scope>/chat/<conversation-id>/messages.jsonl` — append-only `ChatMessage` event log. Assistant turns carry both aggregated `content` and, when available, an ordered `blocks[]` timeline for text, thinking text, and tool activity.
- Actor-scoped local calls with a tenant use
  `~/.animus/<repo-scope>/chat/tenants/<sha256(tenant-id)>/<conversation-id>/...`.

As with all Animus state, treat these as tool-managed — use the `animus chat` surface rather than hand-editing the JSON.

## Implementation notes

Backend selection runs through `ConversationStoreClient` (`crates/orchestrator-cli/src/services/runtime/runtime_chat/client.rs`), which routes to a discovered `conversation_store` plugin or falls back to the in-tree `FileConversationStore`. Both implement the `ConversationStore` trait, so the turn loop and the CLI handlers are backend-agnostic.

The turn loop is factored around a `TurnProducer` trait (`crates/orchestrator-cli/src/services/runtime/runtime_chat/turn.rs`). Production wires `ResolverTurnProducer`, which resolves the installed provider plugin and starts a session. The v0.5.11 `chat_provider` plugin role will slot in as an alternate `TurnProducer` without changing the continuity logic, and tests inject a scripted mock producer. The streaming sink is likewise abstracted behind `ChatStreamSink` so the same loop drives JSONL, text, and discard outputs.
