# llm-cli-wrapper Session Backends

## Summary

`llm-cli-wrapper` currently gives AO a thin compatibility layer for:

- runtime-contract launch parsing
- machine-output flag injection
- basic CLI discovery and health checks
- text normalization from provider-specific JSON output

That is enough for the current subprocess model, but it leaves most of the
session lifecycle inside `agent-runner`. AO still owns:

- process creation and shutdown
- provider-specific resume/session behavior
- stdout/stderr event extraction
- MCP-aware tool-call normalization
- fallback behavior when a provider surface is incomplete

The new direction is to make `llm-cli-wrapper` the session boundary for
CLI-backed agents while keeping `agent-runner` focused on workspace validation,
policy enforcement, persistence, and orchestration.

## Goal

Define one AO-facing backend contract that can drive:

- an AO-owned Claude backend
- an AO-owned Codex backend
- an AO-owned Gemini backend
- subprocess fallback for unsupported or incomplete cases

The studied community libraries are references for protocol shape and lifecycle
design:

- `claude-agent-sdk`
- `codex-sdk-rs`
- `gemini-cli-sdk`

This is not a lowest-common-denominator API. The wrapper owns canonical AO
session events, but each backend may expose additional provider-specific
capabilities through metadata and explicit opt-in fields.

## Canonical Contract

The wrapper contract must cover:

- backend identity and stability level
- capability discovery
- session start
- session resume
- session termination
- permission mode selection
- MCP/tool-use support reporting
- event streaming back to AO in a canonical format

The canonical event surface should support:

- text deltas
- final text
- tool calls
- tool results
- thinking traces
- artifacts
- usage metadata
- structured errors
- completion

## Canonical Event Model

AO should normalize all CLI-backed agent sessions into one stream:

| Event | Required payload |
|---|---|
| `Started` | backend, provider tool, session id if known |
| `TextDelta` | incremental text |
| `FinalText` | final assistant text when the backend separates it |
| `ToolCall` | tool name, arguments, optional server |
| `ToolResult` | tool name, success flag, payload |
| `Thinking` | provider-exposed reasoning/thinking text |
| `Artifact` | identifier, path or metadata |
| `Metadata` | token usage, cost, provider metadata |
| `Error` | error class, message, recoverability |
| `Finished` | exit status / terminal state |

Lossy mapping is acceptable when a provider backend cannot distinguish
`TextDelta` from `FinalText`, or when the underlying CLI surface does not expose
artifacts or usage. That loss must be explicit in capability reporting.

## Required Backend Methods

Every backend should implement the following semantics:

| Method | Purpose |
|---|---|
| `info()` | backend identity, stability, provider tool name |
| `capabilities()` | supported session, MCP, permissions, resume, and event surfaces |
| `start_session(request)` | launch a new session and return a streaming handle |
| `resume_session(request, session_id)` | reuse an existing session when supported |
| `terminate_session(session_id)` | best-effort stop of a running session |

The session request should carry:

- tool id
- model id
- prompt
- cwd
- optional project root
- optional MCP endpoint/config
- timeout hints
- permission mode
- provider-specific extras

## Capability Matrix

This matrix reflects current evaluation of the candidate reference libraries and
the corresponding AO-owned backend direction. It is an implementation decision
aid, not a promise that all features work in AO today.

| AO backend target | Reference input | Session model | Resume | MCP / tools | Permissions | Notes | Ship level |
|---|---|---|---|---|---|---|
| Claude native backend | `claude-agent-sdk` | Rich client/session surface | Yes | Yes | Yes | Strongest shape and documentation of the three | Stable target |
| Codex native backend | `codex-sdk-rs` | Promising structured session/event surface | Likely yes | Partial / evolving | Unknown | Good protocol fit, weak maturity signals | Experimental target |
| Gemini native backend | `gemini-cli-sdk` | ACP-based session client | Yes | Yes | Yes | Strong API shape, but depends on experimental Gemini CLI ACP mode | Experimental target |
| subprocess fallback | Current AO path | Yes, via AO runtime contract logic | Yes, via current parsing and policy layers | Partial and tool-specific | Safety net for unsupported or broken native backends | Stable fallback |

## Stable vs Experimental Recommendation

### Stable now

- AO-owned Claude backend, informed by `claude-agent-sdk`
- existing subprocess backend

### Experimental until proven in AO

- AO-owned Codex backend, informed by `codex-sdk-rs`
- AO-owned Gemini backend, informed by `gemini-cli-sdk`

The experimental designation is driven by maturity and upstream stability, not
by architectural fit. Both native backends should be integrated behind feature
gates or runtime backend selection so AO can fall back to subprocess mode.

## Fallback Rules

AO must keep subprocess mode when any of the following are true:

- the native backend is disabled by config
- the requested feature is unsupported by the selected backend
- resume/session reuse is required but unavailable
- MCP-only policy cannot be enforced with equivalent semantics
- the native backend drifts from a newer upstream CLI release

Fallback must be deterministic. AO should report:

- requested backend
- selected backend
- reason for fallback

## Integration Boundary

Responsibilities should be divided like this:

### llm-cli-wrapper

- backend selection
- AO-owned native backend integration
- provider session lifecycle
- canonical event emission
- capability reporting
- subprocess fallback implementation

### agent-runner

- workspace guardrails
- env sanitization
- timeout policy
- MCP-only enforcement policy
- run persistence
- IPC transport

This keeps AO from duplicating provider-specific logic in both crates.

## Migration Plan

1. Add the canonical session backend facade to `llm-cli-wrapper`.
2. Implement a subprocess backend using the current launch/parser path.
3. Add the Claude native backend and prove parity on one end-to-end flow.
4. Add Codex and Gemini native backends behind experimental gating.
5. Move `agent-runner` happy-path session startup to the wrapper facade.
6. Keep current parsing-based fallback until the native backends are
   operationally proven.

## Validation Targets

Before AO can treat the new facade as complete, it must verify:

- agent execution across Claude, Codex, and Gemini
- at least one session continuation/resume path
- tool-call and tool-result normalization
- fallback behavior when a native backend is unavailable
- preservation of existing run persistence and timeout behavior
