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
- **Continuity pointer.** Conversation meta stores the current `session_id` + `tool` + `model`. The loop captures `SessionRun.session_id` into meta after every turn so the next turn can resume.

Animus still persists every turn — the user message before the provider call (crash safety), the assistant message after — but that store serves portability / query / fallback, **not** live-session prompt replay. There is no double-bookkeeping of context into the prompt.

### Resume-failure fallback

If a resume turn (case 1) comes back with an error indicating the native session is gone or invalid (`session not found`, `session expired`, `could not resume`, ...), the loop falls back to case 2 (replay full history, drop the stale `session_id`) and retries **exactly once**. If the replay attempt still reports a stale session, the turn fails. A tool change mid-conversation is detected up front (`meta.tool != requested tool`) and forces a replay without ever reusing the prior tool's `session_id`.

## CLI surface

```bash
# Start an empty conversation (prints the conversation id)
animus chat new [--id <id>] [--title <title>] \
  [--as-user <user-id>] [--visibility private|shared]

# Send a turn (creates a conversation if --conversation is omitted)
animus chat send "your message" \
  [--conversation <id>] [--tool claude] [--model <model>] [--cwd <path>] \
  [--stream] [--title <title>] [--as-user <user-id>] [--visibility private|shared]

# Read a transcript, optionally returning a bounded slice
animus chat get <id> [--as-user <user-id>] [--limit <n>] [--offset <n>]

# List conversations, most-recently-updated first, optionally paged
animus chat list [--as-user <user-id>] [--limit <n>] [--offset <n>]

# Set or clear a conversation title
animus chat rename <id> --title <title> [--as-user <user-id>]

# Permanently delete a conversation
animus chat delete <id> [--as-user <user-id>]

# Export a conversation transcript as Markdown or JSON
animus chat export <id> [--format markdown|json] [--output <path>] [--as-user <user-id>]
```

`animus chat send --title` names a freshly-created conversation or renames the
target one before the turn runs; surrounding whitespace is trimmed, and an
empty string clears the stored title. `animus chat rename` applies the same
trimming and clear-on-empty behavior. `animus chat delete` is idempotent: a
missing conversation is treated as already deleted rather than an error.
`animus chat export` defaults to Markdown, can emit the full `{ meta, messages
}` JSON shape, and writes raw transcript content to stdout unless `--output` is
supplied.

### Streaming and output modes

`animus chat send` selects its sink from the flags:

- `--json` → **JsonlStdoutSink**: one self-describing JSON object per line (`turn_started`, `text_delta`, `thinking`, `tool_call`, `tool_result`, `metadata`, `warning`, `turn_completed`). The `turn_started` frame carries `resumed: true|false` so a downstream app can confirm the XOR continuity decision. The `turn_completed` frame carries the captured `session_id` for the next turn.
- `--stream` (no `--json`) → plain text deltas to stdout for an interactive session.
- neither → the final assistant turn is printed once the turn completes.

## Cost

```bash
animus cost conversation <id>   # token + USD spend for one conversation
```

Per-turn `usage` and `cost_usd` are recorded on each assistant `ChatMessage` from the provider's metadata frames; `animus cost conversation` folds them into a per-conversation total.

## Ownership and visibility

Each conversation carries two optional identity fields on its `ConversationMeta`:

- `owner` — the authenticated user id that owns the conversation. `None` for **unowned** conversations: legacy on-disk metas (the field is serde-defaulted, so existing conversations load unchanged) and ones created without `--as-user`.
- `visibility` — `private` (the default) or `shared`.

`--as-user <id>` stamps an owner on `animus chat new` and on an `animus chat send` that auto-creates a conversation; `--visibility` sets the initial visibility. `animus chat list --as-user <id>` returns that user's own conversations PLUS any `shared` ones; `animus chat list` with no `--as-user` returns everything (the legacy/admin view).

Owner filtering is applied at the query layer (the in-tree store has no auth context and `list` always returns everything). Beyond `list`, the kernel also enforces the same owner/shared rule client-side on every **direct-id** verb when `--as-user` is given — `chat get`, `export`, `rename`, `delete`, and a `chat send` into an existing conversation: accessing another user's `private` conversation is rejected as `not found` (a uniform error, so a probe cannot tell a private conversation it may not see from one that does not exist). With no `--as-user`, all access is permitted (the legacy/admin view). When a `conversation_store` plugin is installed, the acting user from `--as-user` ALSO rides every per-conversation RPC (`load_meta`, `save_meta`, `append_message`, `load_messages`, `delete`) so the backend can authorize server-side; the client-side check is a backstop for backends that do not.

For `chat list`, owner filtering happens before `--offset` / `--limit`, so
inaccessible rows neither consume page slots nor affect the visible page
shape. `chat get` applies those flags to the ordered `messages` array while
preserving the `{ meta, messages }` JSON shape. The current
`conversation_store` protocol has no server-side paging fields, so the backend
still returns the full candidate list or transcript before the CLI bounds its
output.

## Pluggable conversation store

Chat persistence is served by an **optional** `conversation_store` plugin role. With no plugin installed, the in-tree filesystem store (below) is used — chat works with zero plugins. When a `conversation_store` plugin is discovered, the chat data ops (`create` / `load_meta` / `save_meta` / `append_message` / `load_messages` / `list` / `delete`) route to it over JSON-RPC instead; this is how an out-of-tree Postgres backend serves chat history with real per-user ownership + sharing.

The role is optional: it is **not** a required preflight role, and the daemon never refuses to start without it. The contract (method names + request/response types) lives in `crates/animus-plugin-protocol/src/lib.rs` under the `conversation_store` module; the `Visibility` enum, `ConversationMeta`, and `ChatMessage` wire shapes match the on-disk `meta.json` / `messages.jsonl` shapes exactly. Cross-process turn serialization (`try_lock_conversation`) is intentionally NOT on the wire — a DB-backed plugin uses a transaction or advisory lock per conversation instead. Each plugin call spawns a host, runs one RPC, and reaps the host (`host.shutdown().await`).

## State layout

When no `conversation_store` plugin is installed, conversations live under the scoped runtime root:

- `~/.animus/<repo-scope>/chat/<conversation-id>/meta.json` — `ConversationMeta` (the continuity pointer: `session_id` + `tool` + `model`, plus counts and timestamps).
- `~/.animus/<repo-scope>/chat/<conversation-id>/messages.jsonl` — append-only `ChatMessage` event log. Assistant turns carry both aggregated `content` and, when available, an ordered `blocks[]` timeline for text, thinking text, and tool activity.

As with all Animus state, treat these as tool-managed — use the `animus chat` surface rather than hand-editing the JSON.

## Implementation notes

Backend selection runs through `ConversationStoreClient` (`crates/orchestrator-cli/src/services/runtime/runtime_chat/client.rs`), which routes to a discovered `conversation_store` plugin or falls back to the in-tree `FileConversationStore`. Both implement the `ConversationStore` trait, so the turn loop and the CLI handlers are backend-agnostic.

The turn loop is factored around a `TurnProducer` trait (`crates/orchestrator-cli/src/services/runtime/runtime_chat/turn.rs`). Production wires `ResolverTurnProducer`, which resolves the installed provider plugin and starts a session. The v0.5.11 `chat_provider` plugin role will slot in as an alternate `TurnProducer` without changing the continuity logic, and tests inject a scripted mock producer. The streaming sink is likewise abstracted behind `ChatStreamSink` so the same loop drives JSONL, text, and discard outputs.
