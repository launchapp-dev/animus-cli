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

use anyhow::{bail, Result};
use serde::Serialize;

const MAX_APPLICATION_PROTOCOL_STRING_BYTES: usize = 512;
const MAX_APPLICATION_CHAT_ERROR_BYTES: usize = 1_024;
const MAX_APPLICATION_CHAT_SEQUENCE: u64 = 9_007_199_254_740_991;

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

fn validate_application_protocol_string(label: &str, value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > MAX_APPLICATION_PROTOCOL_STRING_BYTES
        || value.chars().any(|character| character.is_ascii_control())
    {
        bail!("{label} violates the application receipt string contract");
    }
    Ok(())
}

fn validate_application_receipt_event(event: &ChatStreamEvent) -> Result<()> {
    let validate_seq = |label: &str, value: u64| {
        if value > MAX_APPLICATION_CHAT_SEQUENCE {
            bail!("{label} exceeds the cross-language application sequence limit");
        }
        Ok(())
    };
    match event {
        ChatStreamEvent::UserMessageAccepted {
            status,
            conversation_id,
            seq,
            message_id,
            operation_id: Some(operation_id),
        } => {
            if *status != orchestrator_core::ChatOperationStatus::UserAccepted {
                bail!("application accepted receipt has a non-accepted status");
            }
            validate_application_protocol_string("conversation_id", conversation_id)?;
            validate_application_protocol_string("message_id", message_id)?;
            validate_application_protocol_string("operation_id", operation_id)?;
            validate_seq("seq", *seq)?;
        }
        ChatStreamEvent::TurnCompleted {
            status,
            conversation_id,
            seq,
            message_id,
            user_seq,
            user_message_id,
            operation_id: Some(operation_id),
            session_id,
        } => {
            if *status != orchestrator_core::ChatOperationStatus::Completed {
                bail!("application completed receipt has a non-completed status");
            }
            for (label, value) in [
                ("conversation_id", conversation_id.as_str()),
                ("message_id", message_id.as_str()),
                ("user_message_id", user_message_id.as_str()),
                ("operation_id", operation_id.as_str()),
            ] {
                validate_application_protocol_string(label, value)?;
            }
            if let Some(session_id) = session_id {
                validate_application_protocol_string("session_id", session_id)?;
            }
            validate_seq("seq", *seq)?;
            validate_seq("user_seq", *user_seq)?;
        }
        ChatStreamEvent::TurnFailed {
            status,
            conversation_id,
            user_seq,
            user_message_id,
            operation_id: Some(operation_id),
            error_code,
            error_message,
        } => {
            if !matches!(
                status,
                orchestrator_core::ChatOperationStatus::AssistantFailed
                    | orchestrator_core::ChatOperationStatus::AssistantInterrupted
            ) {
                bail!("application failed receipt has a non-failure status");
            }
            for (label, value) in [
                ("conversation_id", conversation_id.as_str()),
                ("user_message_id", user_message_id.as_str()),
                ("operation_id", operation_id.as_str()),
                ("error_code", error_code.as_str()),
            ] {
                validate_application_protocol_string(label, value)?;
            }
            if error_message.is_empty() || error_message.len() > MAX_APPLICATION_CHAT_ERROR_BYTES {
                bail!("error_message violates the application receipt error contract");
            }
            validate_seq("user_seq", *user_seq)?;
        }
        _ => {}
    }
    Ok(())
}

impl ChatStreamSink for JsonlStdoutSink {
    fn emit(&mut self, event: &ChatStreamEvent) -> Result<()> {
        validate_application_receipt_event(event)?;
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

    #[test]
    fn application_receipts_match_vendored_shared_limits_and_shapes() {
        let limits: serde_json::Value =
            serde_json::from_str(include_str!("../../../../contracts/animus-application-protocol/_limits.json"))
                .unwrap();
        assert_eq!(limits["application_protocol_string_max_utf8_bytes"], MAX_APPLICATION_PROTOCOL_STRING_BYTES);
        assert_eq!(limits["application_chat_error_max_utf8_bytes"], MAX_APPLICATION_CHAT_ERROR_BYTES);
        assert_eq!(limits["application_chat_sequence_max"], MAX_APPLICATION_CHAT_SEQUENCE);

        let contract: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../contracts/animus-application-protocol/ApplicationChatReceiptFrame.json"
        ))
        .unwrap();
        let types = contract["oneOf"]
            .as_array()
            .unwrap()
            .iter()
            .map(|schema| schema["properties"]["type"]["const"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(types, ["user_message_accepted", "turn_completed", "turn_failed"]);

        let accepted = ChatStreamEvent::UserMessageAccepted {
            status: orchestrator_core::ChatOperationStatus::UserAccepted,
            conversation_id: "chat-1".into(),
            seq: MAX_APPLICATION_CHAT_SEQUENCE,
            message_id: "é".repeat(256),
            operation_id: Some("operation-1".into()),
        };
        validate_application_receipt_event(&accepted).unwrap();

        for rejected in [
            ChatStreamEvent::UserMessageAccepted {
                status: orchestrator_core::ChatOperationStatus::UserAccepted,
                conversation_id: "chat-1".into(),
                seq: MAX_APPLICATION_CHAT_SEQUENCE + 1,
                message_id: "message-user".into(),
                operation_id: Some("operation-1".into()),
            },
            ChatStreamEvent::UserMessageAccepted {
                status: orchestrator_core::ChatOperationStatus::UserAccepted,
                conversation_id: "chat-1".into(),
                seq: 1,
                message_id: format!("{}x", "é".repeat(256)),
                operation_id: Some("operation-1".into()),
            },
        ] {
            assert!(validate_application_receipt_event(&rejected).is_err());
        }

        let oversized_error = ChatStreamEvent::TurnFailed {
            status: orchestrator_core::ChatOperationStatus::AssistantFailed,
            conversation_id: "chat-1".into(),
            user_seq: 1,
            user_message_id: "message-user".into(),
            operation_id: Some("operation-1".into()),
            error_code: "provider_failed".into(),
            error_message: format!("{}x", "é".repeat(512)),
        };
        assert!(validate_application_receipt_event(&oversized_error).is_err());
    }
}
