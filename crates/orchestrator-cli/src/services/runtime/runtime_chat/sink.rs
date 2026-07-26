//! Streaming sinks for the chat turn loop.
//!
//! The turn loop ([`super::turn::run_turn`]) translates each provider
//! [`SessionEvent`] into a normalized [`ChatStreamEvent`] and hands it to a
//! [`ChatStreamSink`]. Decoupling the sink from the loop keeps the
//! continuity logic testable (tests use a capturing sink) and lets the same
//! loop drive multiple output shapes:
//!
//! * [`JsonlStdoutSink`] — one JSON object per line on stdout, for
//!   `animus chat send --stream --json`. Each line is self-describing so a
//!   downstream app can render incrementally.
//! * [`TextStdoutSink`] — plain text deltas to stdout, for an interactive
//!   `--stream` (no `--json`) session.
//! * [`NullSink`] — discards events, for the non-streaming path where only
//!   the final persisted message matters.
//!
//! [`SessionEvent`]: animus_session_backend::session::SessionEvent

use anyhow::Result;
use serde::Serialize;

/// Normalized, provider-agnostic streaming event. Mirrors the meaningful
/// subset of `SessionEvent` that a chat UI cares about, plus chat-level
/// framing (`turn_started` / `turn_completed`).
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum ChatStreamEvent {
    /// Emitted once before any provider output, carrying the conversation +
    /// continuity context for this turn.
    TurnStarted {
        conversation_id: String,
        tool: String,
        model: String,
        /// Whether this turn resumed a live native session (`true`) or
        /// replayed full history into the prompt (`false`). Surfaced so a
        /// UI / test can confirm the XOR continuity decision.
        resumed: bool,
    },
    /// Durable boundary: the canonical user message is stored. A provider
    /// failure after this frame must not be reported as an unaccepted send.
    UserMessageAccepted {
        status: orchestrator_core::ChatOperationStatus,
        conversation_id: String,
        seq: u64,
        message_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        operation_id: Option<String>,
    },
    /// Incremental assistant text.
    TextDelta { text: String },
    /// Visible reasoning trace.
    Thinking { text: String },
    /// Agent invoked a tool.
    ToolCall { tool_name: String, arguments: serde_json::Value },
    /// A tool returned a result. `output` carries the full tool result the
    /// provider surfaced (file contents, command output, structured JSON)
    /// so a consumer can stream everything the agent did, not just whether
    /// the call succeeded. Omitted from the wire when the provider gave no
    /// output payload.
    ToolResult {
        tool_name: String,
        success: bool,
        #[serde(skip_serializing_if = "serde_json::Value::is_null")]
        output: serde_json::Value,
    },
    /// Provider metadata frame (usage / cost).
    Metadata {
        #[serde(skip_serializing_if = "Option::is_none")]
        cost_usd: Option<f64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        tokens: Option<protocol::TokenUsage>,
    },
    /// Recoverable provider warning — the turn continues.
    Warning { message: String },
    /// Terminal frame: the turn finished and the assistant message was
    /// persisted. Carries the captured continuity pointer for the next turn.
    TurnCompleted {
        status: orchestrator_core::ChatOperationStatus,
        conversation_id: String,
        seq: u64,
        message_id: String,
        user_seq: u64,
        user_message_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        operation_id: Option<String>,
        /// `session_id` captured from `SessionRun` for the next turn, if the
        /// provider returned one.
        session_id: Option<String>,
    },
    /// Terminal partial-success frame. The user message is canonical but the
    /// assistant did not complete; exact retries replay this bounded receipt.
    TurnFailed {
        status: orchestrator_core::ChatOperationStatus,
        conversation_id: String,
        user_seq: u64,
        user_message_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        operation_id: Option<String>,
        error_code: String,
        error_message: String,
    },
}

/// Receives [`ChatStreamEvent`]s as the turn streams. Implementations must
/// be cheap; the loop calls `emit` on the hot path.
pub(crate) trait ChatStreamSink {
    fn emit(&mut self, event: &ChatStreamEvent) -> Result<()>;
}

/// Emits one JSON object per line to stdout. Used by
/// `animus chat send --stream --json`.
pub(crate) struct JsonlStdoutSink;

impl ChatStreamSink for JsonlStdoutSink {
    fn emit(&mut self, event: &ChatStreamEvent) -> Result<()> {
        let line = serde_json::to_string(event)?;
        println!("{line}");
        Ok(())
    }
}

/// Emits human-readable text deltas (and a terse marker for non-text
/// frames) to stdout. Used by interactive `--stream` without `--json`.
pub(crate) struct TextStdoutSink;

impl ChatStreamSink for TextStdoutSink {
    fn emit(&mut self, event: &ChatStreamEvent) -> Result<()> {
        use std::io::Write;
        match event {
            ChatStreamEvent::TextDelta { text } => {
                print!("{text}");
                std::io::stdout().flush().ok();
            }
            ChatStreamEvent::ToolCall { tool_name, .. } => {
                eprintln!("[tool: {tool_name}]");
            }
            ChatStreamEvent::Warning { message } => {
                eprintln!("[warning: {message}]");
            }
            ChatStreamEvent::TurnCompleted { .. } => {
                println!();
            }
            ChatStreamEvent::TurnFailed { error_message, .. } => {
                eprintln!("[assistant failed: {error_message}]");
            }
            _ => {}
        }
        Ok(())
    }
}

/// Discards every event. Used by the non-streaming path.
pub(crate) struct NullSink;

impl ChatStreamSink for NullSink {
    fn emit(&mut self, _event: &ChatStreamEvent) -> Result<()> {
        Ok(())
    }
}

#[cfg(test)]
pub(crate) struct CapturingSink {
    pub events: Vec<ChatStreamEvent>,
}

#[cfg(test)]
impl CapturingSink {
    pub(crate) fn new() -> Self {
        Self { events: Vec::new() }
    }
}

#[cfg(test)]
impl ChatStreamSink for CapturingSink {
    fn emit(&mut self, event: &ChatStreamEvent) -> Result<()> {
        self.events.push(event.clone());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jsonl_event_serializes_with_type_tag() {
        let event = ChatStreamEvent::TextDelta { text: "hi".into() };
        let line = serde_json::to_string(&event).unwrap();
        assert_eq!(line, r#"{"type":"text_delta","text":"hi"}"#);
    }

    #[test]
    fn turn_started_carries_resumed_flag() {
        let event = ChatStreamEvent::TurnStarted {
            conversation_id: "c1".into(),
            tool: "claude".into(),
            model: "claude-sonnet-4-6".into(),
            resumed: true,
        };
        let value: serde_json::Value = serde_json::to_value(&event).unwrap();
        assert_eq!(value["type"], "turn_started");
        assert_eq!(value["resumed"], true);
    }

    #[test]
    fn terminal_partial_success_frame_has_canonical_bounded_fields() {
        let event = ChatStreamEvent::TurnFailed {
            status: orchestrator_core::ChatOperationStatus::AssistantFailed,
            conversation_id: "c1".into(),
            user_seq: 4,
            user_message_id: "msg-user".into(),
            operation_id: Some("op-1".into()),
            error_code: "provider_failed".into(),
            error_message: "boom".into(),
        };
        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(value["type"], "turn_failed");
        assert_eq!(value["status"], "assistant_failed");
        assert_eq!(value["conversation_id"], "c1");
        assert_eq!(value["user_seq"], 4);
        assert_eq!(value["user_message_id"], "msg-user");
        assert_eq!(value["operation_id"], "op-1");
    }
}
