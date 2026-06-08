//! `animus events ...` — workflow event observability surface (v0.5.8).
//!
//! The daemon broadcasts workflow lifecycle events on the
//! `workflow/events` control RPC (per animus-protocol v0.1.10+). This module
//! exposes a CLI consumer (`animus events tail`) that subscribes, filters
//! by workflow id / kind / since-window, and renders either a human-readable
//! tail or one JSON-per-line stream.

use std::path::Path;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Utc};

use crate::cli_types::{EventsCommand, EventsTailArgs};
use crate::shared::print_value;

pub(crate) async fn handle_events(command: EventsCommand, project_root: &str, json: bool) -> Result<()> {
    match command {
        EventsCommand::Tail(mut args) => {
            args.json = args.json || json;
            handle_events_tail(args, project_root).await
        }
    }
}

async fn handle_events_tail(args: EventsTailArgs, project_root: &str) -> Result<()> {
    use orchestrator_daemon_runtime::control::ControlClient;

    let project_root_path = Path::new(project_root);
    let client = match ControlClient::try_connect(project_root_path).await? {
        Some(client) => client,
        None => {
            return Err(anyhow!(
                "animus events tail requires a running daemon (control socket not found). Start one with: animus daemon start"
            ));
        }
    };

    let since_threshold = match args.since.as_deref() {
        Some(value) => Some(Utc::now() - parse_duration_arg(value)?),
        None => None,
    };

    let request =
        animus_control_protocol::types::WorkflowEventsRequest { workflow_id: args.workflow_id.clone(), kinds: None };

    if args.json {
        client
            .workflow_events(request, |event| {
                if !passes_since(&event, since_threshold) {
                    return true;
                }
                if let Err(error) = print_value(&event, true) {
                    tracing::warn!(error = %error, "failed to print workflow event");
                }
                true
            })
            .await
    } else {
        if since_threshold.is_some() {
            eprintln!(
                "note: daemon does not buffer historical workflow events; --since filters only events that arrive after the subscription is live."
            );
        }
        eprintln!("tailing workflow events (Ctrl-C to stop)...");
        client
            .workflow_events(request, |event| {
                if !passes_since(&event, since_threshold) {
                    return true;
                }
                println!("{}", format_event_line(&event));
                true
            })
            .await
    }
}

fn passes_since(event: &animus_control_protocol::types::WorkflowEvent, since: Option<DateTime<Utc>>) -> bool {
    match since {
        None => true,
        Some(threshold) => event.occurred_at >= threshold,
    }
}

pub(crate) fn format_event_line(event: &animus_control_protocol::types::WorkflowEvent) -> String {
    let timestamp = event.occurred_at.format("%Y-%m-%dT%H:%M:%SZ");
    let phase =
        event.payload.get("phase_id").and_then(|v| v.as_str()).map(|s| format!(" phase={s}")).unwrap_or_default();
    let attempt =
        event.payload.get("attempt").and_then(|v| v.as_u64()).map(|n| format!(" attempt={n}")).unwrap_or_default();
    let status =
        event.payload.get("status").and_then(|v| v.as_str()).map(|s| format!(" status={s}")).unwrap_or_default();
    format!("{timestamp} wf:{wf} {kind:<19}{phase}{attempt}{status}", wf = event.workflow_id, kind = event.kind)
}

/// Parse durations like `90s`, `5m`, `2h`, `1d`. Returns chrono Duration so
/// the threshold subtraction stays UTC-safe.
pub(crate) fn parse_duration_arg(value: &str) -> Result<chrono::Duration> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(anyhow!("--since must be non-empty"));
    }
    let (digits, suffix) = trimmed.split_at(trimmed.find(|c: char| !c.is_ascii_digit()).unwrap_or(trimmed.len()));
    if digits.is_empty() {
        return Err(anyhow!("--since '{value}' must start with a number (e.g. 5m, 2h)"));
    }
    let amount: i64 = digits.parse().with_context(|| format!("--since '{value}' is not a valid number"))?;
    let std_duration = match suffix {
        "" | "s" => Duration::from_secs(amount as u64),
        "m" => Duration::from_secs((amount as u64).saturating_mul(60)),
        "h" => Duration::from_secs((amount as u64).saturating_mul(3_600)),
        "d" => Duration::from_secs((amount as u64).saturating_mul(86_400)),
        other => {
            return Err(anyhow!("--since '{value}' has unknown unit '{other}'; supported: s, m, h, d"));
        }
    };
    chrono::Duration::from_std(std_duration)
        .with_context(|| format!("--since '{value}' exceeds the representable duration range"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use serde_json::json;

    fn evt(workflow_id: &str, kind: &str, payload: serde_json::Value) -> animus_control_protocol::types::WorkflowEvent {
        animus_control_protocol::types::WorkflowEvent {
            workflow_id: workflow_id.to_string(),
            kind: kind.to_string(),
            payload,
            occurred_at: Utc.with_ymd_and_hms(2026, 6, 8, 5, 12, 1).unwrap(),
        }
    }

    #[test]
    fn parse_duration_arg_supports_units() {
        assert_eq!(parse_duration_arg("30s").unwrap().num_seconds(), 30);
        assert_eq!(parse_duration_arg("5m").unwrap().num_seconds(), 300);
        assert_eq!(parse_duration_arg("2h").unwrap().num_seconds(), 7_200);
        assert_eq!(parse_duration_arg("1d").unwrap().num_seconds(), 86_400);
    }

    #[test]
    fn parse_duration_arg_rejects_unknown_unit() {
        let err = parse_duration_arg("5y").unwrap_err();
        assert!(err.to_string().contains("unknown unit"), "got: {err}");
    }

    #[test]
    fn parse_duration_arg_rejects_empty_and_non_numeric() {
        assert!(parse_duration_arg("").is_err());
        assert!(parse_duration_arg("abc").is_err());
    }

    #[test]
    fn format_event_line_renders_phase_attempt_and_status() {
        let event = evt("wf-abc", "phase_started", json!({"phase_id": "impl", "attempt": 1}));
        let line = format_event_line(&event);
        assert!(line.contains("wf:wf-abc"), "got: {line}");
        assert!(line.contains("phase_started"), "got: {line}");
        assert!(line.contains("phase=impl"), "got: {line}");
        assert!(line.contains("attempt=1"), "got: {line}");
    }

    #[test]
    fn format_event_line_omits_missing_payload_fields() {
        let event = evt("wf-1", "workflow_completed", json!({"status": "success"}));
        let line = format_event_line(&event);
        assert!(line.contains("status=success"));
        assert!(!line.contains("phase="));
        assert!(!line.contains("attempt="));
    }

    #[test]
    fn passes_since_filters_events_before_threshold() {
        let event = evt("wf-1", "phase_started", json!({}));
        let future = event.occurred_at + chrono::Duration::seconds(60);
        let past = event.occurred_at - chrono::Duration::seconds(60);
        assert!(!passes_since(&event, Some(future)), "event before threshold must be filtered out");
        assert!(passes_since(&event, Some(past)));
        assert!(passes_since(&event, None));
    }
}
