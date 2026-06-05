# Provider Session Host

This file keeps the historical `llm-cli-wrapper-session-backends.md` path for
old links, but the current implementation no longer has an in-tree
`llm-cli-wrapper` crate or `agent-runner` sidecar. Provider execution now
crosses two boundaries:

1. `orchestrator-cli` or a `workflow_runner` plugin prepares the runtime
   contract and session request.
2. `orchestrator-plugin-host::session` resolves and drives installed provider
   plugins through `orchestrator-plugin-host`.

For the broader plugin architecture, see [Plugin System](plugin-system.md).

## Source Files

| Area | Source |
|---|---|
| Provider resolver | [`crates/orchestrator-plugin-host/src/session/session_backend_resolver.rs`](../../crates/orchestrator-plugin-host/src/session/session_backend_resolver.rs) |
| Provider plugin backend | [`crates/orchestrator-plugin-host/src/session/plugin_backend.rs`](../../crates/orchestrator-plugin-host/src/session/plugin_backend.rs) |
| Provider supervisor | [`crates/orchestrator-plugin-host/src/session/plugin_supervisor.rs`](../../crates/orchestrator-plugin-host/src/session/plugin_supervisor.rs) |
| Agent runtime entrypoint | [`crates/orchestrator-cli/src/services/runtime/runtime_agent/`](../../crates/orchestrator-cli/src/services/runtime/runtime_agent/) |
| Stdio host | [`crates/orchestrator-plugin-host/src/host.rs`](../../crates/orchestrator-plugin-host/src/host.rs) |

## Current Flow

```mermaid
sequenceDiagram
    participant Caller as animus agent / workflow runner
    participant Resolver as orchestrator-plugin-host::session
    participant Host as PluginHost
    participant Plugin as provider plugin

    Caller->>Resolver: resolve provider tool
    Resolver->>Resolver: discover provider plugins
    Resolver->>Host: spawn + initialize
    Host->>Plugin: agent/run or agent/resume
    Plugin-->>Host: notifications
    Host-->>Resolver: canonical events
    Resolver-->>Caller: session events
    Caller->>Host: agent/cancel on active session
```

The resolver discovers plugins with `plugin_kind == "provider"`. There is no
in-tree provider fallback. If a requested provider is not installed, the resolver
returns a hard error with the install command.

## Provider Names

The resolver canonicalizes the OpenAI-compatible runner names:

- `oai-runner` -> `oai`
- `animus-oai-runner` -> `oai`

Reserved first-party provider names are:

- `claude`
- `codex`
- `gemini`
- `opencode`
- `oai`
- `oai-runner`

Installing a plugin that shadows a reserved provider name requires the explicit
override path in the plugin installer.

## Session Model

`PluginSessionBackend` spawns and handshakes a provider plugin for each
dispatch, then keeps a session map so follow-up operations can reach the same
live plugin host.

Important behavior:

- `agent/run` starts a new provider session.
- `agent/resume` resumes a provider-owned session when supported.
- `agent/cancel` routes through the existing active session host, not through a
  fresh plugin process.
- Provider spawns pin cwd to the resolved `project_root` so provider-local state
  and child CLI relative paths stay anchored to the repository.
- The default `agent/run` request timeout is 1800 seconds.
- The cancel timeout is 10 seconds.
- Provider retries happen once for death-like failures only when no
  notifications were forwarded yet.
- Structured JSON-RPC errors are surfaced directly and do not consume restart
  budget.

The supervisor default is three restarts in a 60 second window, followed by a
five minute cooldown.

## Event Boundary

Provider plugins emit JSON-RPC notifications while handling `agent/run` or
`agent/resume`. The host's single-reader router separates responses from
notifications, forwards provider events to the runner, and keeps pending
requests keyed by JSON-RPC id.

This keeps provider-specific launch, resume, event extraction, and cancellation
behavior behind the provider plugin contract rather than a separate in-tree
sidecar.
