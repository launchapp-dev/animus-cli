# Plugin Host Concurrency

`PluginHost` is the runtime boundary for one live stdio plugin process. The
current implementation uses a single-reader router so concurrent requests,
responses, and notifications can share one process without losing messages.

## Source Files

| Area | Source |
|---|---|
| Host/router implementation | [`crates/orchestrator-plugin-host/src/host.rs`](../../crates/orchestrator-plugin-host/src/host.rs) |
| Subject kind router | [`crates/orchestrator-plugin-host/src/subject_router.rs`](../../crates/orchestrator-plugin-host/src/subject_router.rs) |
| Provider plugin backend | [`crates/orchestrator-plugin-host/src/session/plugin_backend.rs`](../../crates/orchestrator-plugin-host/src/session/plugin_backend.rs) |
| Provider supervisor | [`crates/orchestrator-plugin-host/src/session/plugin_supervisor.rs`](../../crates/orchestrator-plugin-host/src/session/plugin_supervisor.rs) |
| Concurrency tests | [`crates/orchestrator-plugin-host/tests/concurrency.rs`](../../crates/orchestrator-plugin-host/tests/concurrency.rs) |

## Runtime Contract

- One reader task owns plugin stdout.
- Outbound requests get a JSON-RPC id and a pending response channel.
- Responses are matched to pending request ids.
- Notifications are broadcast to subscribers.
- Unknown responses are logged and discarded.
- Plugin shutdown resolves pending requests with a host error.
- Request timeouts are enforced at the caller boundary.

This is the key invariant: no caller reads directly from plugin stdout. That
prevents concurrent requests from stealing each other's responses.

## Notification Backpressure

Notifications are delivered through a broadcast channel. Capacity defaults to
256 and can be overridden with:

```bash
ANIMUS_PLUGIN_BROADCAST_CAPACITY=512
```

Slow subscribers may miss notifications when they lag behind the broadcast
buffer. The request path still continues because response routing is independent
from subscriber consumption.

## Cancellation

Provider cancellation routes through the same active provider session host. The
session backend keeps a map from session id to live `PluginHost`, so
`agent/cancel` reaches the plugin process that owns the running session.

This matters because a fresh plugin process cannot cancel provider-owned state
held by the original process.

## Provider Retry and Supervision

Provider dispatch retries once only for death-like failures when no
notifications have been forwarded yet. Structured JSON-RPC errors are returned
directly to the runner and do not consume restart budget.

The supervisor defaults are:

- 3 restarts
- 60 second restart window
- 5 minute cooldown

## Environment Boundary

Plugin processes start with a cleared environment. The host forwards the base
allowlist plus variables declared in the manifest and request-local extras:

- `PATH`
- `HOME`
- `USER`
- `SHELL`
- `TERM`
- `TMPDIR`
- `LANG`
- `LC_ALL`
- `RUST_LOG`
- `RUST_BACKTRACE`
- `TZ`

## Tests

Run the focused host tests when changing routing, cancellation, or notification
behavior:

```bash
cargo test -p orchestrator-plugin-host --test concurrency
cargo test -p orchestrator-plugin-host --test plugin_supervisor
```

Run the runtime binary set before release:

```bash
cargo animus-bin-check
```
