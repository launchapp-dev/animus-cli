use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use chrono::{DateTime, Utc};
use protocol::{metrics_env_disabled, MetricsConfig};
use serde::Serialize;
use serde_json::Value;

use super::events::{Event, EventName, EventTags};
use super::recorder::{
    delete_rotated, last_send_timestamp, read_metrics_block_without_creating, restore_rotated, rotate_and_read_pending,
    write_last_send_timestamp,
};
use super::{host_arch, host_os};

const MAX_ATTEMPTS: u32 = 3;
const BASE_BACKOFF: Duration = Duration::from_millis(250);

/// Outcome of a flush attempt. Returned to CLI handlers so they can
/// report what happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FlushOutcome {
    /// Telemetry disabled — nothing to do.
    Disabled,
    /// No buffered events.
    Empty,
    /// Batch sent successfully.
    Sent { events: usize, buckets: usize },
    /// Send failed after exhausting retries; events were preserved in
    /// the pending queue for the next flush.
    Failed { events: usize, last_status: Option<u16> },
}

/// Drain pending events and POST them to the configured endpoint.
/// Honors the kill switch and opt-out state.
pub(crate) async fn flush_pending(project_root: &Path) -> FlushOutcome {
    let Some(metrics) = read_metrics_block_without_creating(project_root) else {
        return FlushOutcome::Disabled;
    };
    if !metrics.is_enabled() {
        return FlushOutcome::Disabled;
    }
    let install_id = metrics.install_id.clone().unwrap_or_default();
    if install_id.is_empty() {
        return FlushOutcome::Disabled;
    }
    // Rotate the pending file BEFORE building the payload so concurrent
    // CLI processes that append during the network round-trip write to
    // a fresh `pending.jsonl` instead of having their events dropped
    // when we delete the post-send snapshot.
    let Some((rotated, events)) = rotate_and_read_pending(project_root) else {
        return FlushOutcome::Empty;
    };
    if events.is_empty() {
        delete_rotated(&rotated);
        return FlushOutcome::Empty;
    }
    let payload = build_payload(&metrics, &install_id, &events);
    let total = events.len();
    let buckets = payload.events.len();
    match post_with_retry(&metrics.endpoint, &payload).await {
        Ok(()) => {
            // Only remove the rotated snapshot AFTER the server
            // acknowledged the batch. Any events appended to
            // `pending.jsonl` during the round-trip survive untouched.
            delete_rotated(&rotated);
            write_last_send_timestamp(project_root, &Utc::now().to_rfc3339());
            FlushOutcome::Sent { events: total, buckets }
        }
        Err(last_status) => {
            // Re-merge the snapshot back into `pending.jsonl` so the
            // next flush retries it (and includes anything that arrived
            // during this attempt).
            restore_rotated(project_root, &rotated);
            FlushOutcome::Failed { events: total, last_status }
        }
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct Payload {
    pub install_id: String,
    pub animus_version: String,
    pub os: &'static str,
    pub arch: &'static str,
    pub events: Vec<Bucket>,
}

#[derive(Debug, Serialize)]
pub(crate) struct Bucket {
    pub name: &'static str,
    pub tags: Value,
    pub count: u64,
    pub first_seen: String,
}

pub(crate) fn build_payload(metrics: &MetricsConfig, install_id: &str, events: &[Event]) -> Payload {
    let _ = metrics;
    let mut buckets: BTreeMap<(EventName, String), Bucket> = BTreeMap::new();
    for event in events {
        let name_enum = event.tags.event_name();
        let name = name_enum.as_str();
        let tags_value = tags_to_value(&event.tags);
        let key_tags = tags_value.to_string();
        buckets
            .entry((name_enum, key_tags))
            .and_modify(|bucket| {
                bucket.count += 1;
                if event.recorded_at < bucket.first_seen {
                    bucket.first_seen = event.recorded_at.clone();
                }
            })
            .or_insert(Bucket { name, tags: tags_value, count: 1, first_seen: event.recorded_at.clone() });
    }
    Payload {
        install_id: install_id.to_string(),
        animus_version: env!("CARGO_PKG_VERSION").to_string(),
        os: host_os(),
        arch: host_arch(),
        events: buckets.into_values().collect(),
    }
}

/// Serializes the event's enum tags to a JSON object keyed by the inner
/// tag name. Falls back to `{}` if serialization somehow fails (which
/// shouldn't happen — every variant is a bounded enum).
fn tags_to_value(tags: &EventTags) -> Value {
    let raw = serde_json::to_value(tags).unwrap_or(Value::Object(Default::default()));
    raw.get("tags").cloned().unwrap_or(Value::Object(Default::default()))
}

/// Best-effort, non-blocking flush attempt: returns immediately without
/// sending unless the configured `batch_interval` has elapsed since the
/// last successful send AND there are pending events. Callers should
/// invoke this near a natural CLI exit (after the command's handler
/// returns) so opted-in users don't accumulate events forever.
///
/// Honors the kill switch.
pub(crate) async fn maybe_flush_if_due(project_root: &Path) -> FlushOutcome {
    if metrics_env_disabled() {
        return FlushOutcome::Disabled;
    }
    let Some(metrics) = read_metrics_block_without_creating(project_root) else {
        return FlushOutcome::Disabled;
    };
    if !metrics.is_enabled() {
        return FlushOutcome::Disabled;
    }
    let interval = parse_iso8601_duration_seconds(&metrics.batch_interval).unwrap_or(DEFAULT_BATCH_SECS);
    let now = Utc::now();
    if let Some(raw) = last_send_timestamp(project_root) {
        if let Ok(parsed) = DateTime::parse_from_rfc3339(&raw) {
            let elapsed = now.signed_duration_since(parsed.with_timezone(&Utc));
            if elapsed.num_seconds() < interval {
                return FlushOutcome::Empty;
            }
        }
    }
    flush_pending(project_root).await
}

/// Minimal ISO 8601 duration parser. Recognizes `PnD`, `PnH`, `PnM`,
/// `PnS`, and combinations like `P1DT2H3M4S`. Unparseable inputs return
/// `None`, letting the caller fall back to the configured default.
fn parse_iso8601_duration_seconds(raw: &str) -> Option<i64> {
    let trimmed = raw.trim();
    let rest = trimmed.strip_prefix('P')?;
    let mut total: i64 = 0;
    let mut current = 0i64;
    let mut in_time = false;
    let mut consumed_any = false;
    for ch in rest.chars() {
        if ch == 'T' {
            in_time = true;
            continue;
        }
        if let Some(digit) = ch.to_digit(10) {
            current = current.checked_mul(10)?.checked_add(i64::from(digit))?;
            continue;
        }
        let multiplier = match (in_time, ch) {
            (false, 'D') => 86_400,
            (true, 'H') => 3_600,
            (true, 'M') => 60,
            (true, 'S') => 1,
            _ => return None,
        };
        total = total.checked_add(current.checked_mul(multiplier)?)?;
        current = 0;
        consumed_any = true;
    }
    if !consumed_any {
        return None;
    }
    Some(total)
}

const DEFAULT_BATCH_SECS: i64 = 86_400;

async fn post_with_retry(endpoint: &str, payload: &Payload) -> Result<(), Option<u16>> {
    let client = match reqwest::Client::builder().timeout(Duration::from_secs(5)).build() {
        Ok(client) => client,
        Err(_) => return Err(None),
    };
    let mut last_status: Option<u16> = None;
    for attempt in 0..MAX_ATTEMPTS {
        if attempt > 0 {
            let delay = BASE_BACKOFF * (1 << (attempt - 1));
            tokio::time::sleep(delay).await;
        }
        match client.post(endpoint).json(payload).send().await {
            Ok(resp) => {
                let status = resp.status();
                if status.is_success() {
                    return Ok(());
                }
                last_status = Some(status.as_u16());
                if status.is_client_error() {
                    // 4xx is non-retriable — server doesn't want this shape.
                    return Err(last_status);
                }
            }
            Err(_) => {
                last_status = None;
            }
        }
    }
    Err(last_status)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::metrics::events::{
        CommandGroup, ErrorClass, Event, EventTags, PluginRole, WorkflowKind, WorkflowOutcome,
    };

    fn ev(tags: EventTags, ts: &str) -> Event {
        Event { recorded_at: ts.to_string(), tags }
    }

    #[test]
    fn build_payload_folds_same_tag_events_into_counter_buckets() {
        let events = vec![
            ev(EventTags::WorkflowStarted { workflow_kind: WorkflowKind::Task }, "2026-06-04T10:00:00Z"),
            ev(EventTags::WorkflowStarted { workflow_kind: WorkflowKind::Task }, "2026-06-04T10:01:00Z"),
            ev(EventTags::WorkflowStarted { workflow_kind: WorkflowKind::Requirement }, "2026-06-04T10:02:00Z"),
            ev(EventTags::WorkflowCompleted { outcome: WorkflowOutcome::Success }, "2026-06-04T10:03:00Z"),
        ];
        let metrics = MetricsConfig::default();
        let payload = build_payload(&metrics, "install-id-abc", &events);
        assert_eq!(payload.install_id, "install-id-abc");
        assert_eq!(payload.events.len(), 3, "two task buckets merge; requirement and completed separate");
        let task_bucket = payload.events.iter().find(|b| b.tags["workflow_kind"] == "task").expect("task bucket");
        assert_eq!(task_bucket.count, 2);
        assert_eq!(task_bucket.first_seen, "2026-06-04T10:00:00Z");
    }

    #[test]
    fn parse_iso8601_handles_common_shapes() {
        assert_eq!(parse_iso8601_duration_seconds("P1D"), Some(86_400));
        assert_eq!(parse_iso8601_duration_seconds("P7D"), Some(7 * 86_400));
        assert_eq!(parse_iso8601_duration_seconds("PT1H"), Some(3_600));
        assert_eq!(parse_iso8601_duration_seconds("PT30M"), Some(1_800));
        assert_eq!(parse_iso8601_duration_seconds("PT45S"), Some(45));
        assert_eq!(parse_iso8601_duration_seconds("P1DT2H3M4S"), Some(86_400 + 7_200 + 180 + 4));
        assert_eq!(parse_iso8601_duration_seconds(""), None);
        assert_eq!(parse_iso8601_duration_seconds("P"), None);
        assert_eq!(parse_iso8601_duration_seconds("garbage"), None);
        assert_eq!(parse_iso8601_duration_seconds("PXY"), None);
    }

    #[test]
    fn payload_payload_shape_is_bounded_for_every_event_variant() {
        // Privacy invariant: every event variant must serialize into a
        // payload whose tag values are all primitive (string/bool/null/
        // number). No nested objects, no arrays — a sentinel against any
        // future drift toward free-form fields.
        let variants = vec![
            EventTags::WorkflowStarted { workflow_kind: WorkflowKind::Task },
            EventTags::WorkflowStarted { workflow_kind: WorkflowKind::Requirement },
            EventTags::WorkflowStarted { workflow_kind: WorkflowKind::Custom },
            EventTags::WorkflowCompleted { outcome: WorkflowOutcome::Success },
            EventTags::WorkflowCompleted { outcome: WorkflowOutcome::Failure },
            EventTags::WorkflowCompleted { outcome: WorkflowOutcome::Cancelled },
            EventTags::PluginInstalled { plugin_kind: PluginRole::Provider },
            EventTags::PluginInstalled { plugin_kind: PluginRole::SubjectBackend },
            EventTags::PluginInstalled { plugin_kind: PluginRole::Transport },
            EventTags::PluginInstalled { plugin_kind: PluginRole::WebUi },
            EventTags::PluginInstalled { plugin_kind: PluginRole::Trigger },
            EventTags::PluginInstalled { plugin_kind: PluginRole::LogStorage },
            EventTags::PluginInstalled { plugin_kind: PluginRole::Queue },
            EventTags::PluginInstalled { plugin_kind: PluginRole::Notifier },
            EventTags::PluginInstalled { plugin_kind: PluginRole::WorkflowRunner },
            EventTags::PluginInstalled { plugin_kind: PluginRole::Other },
            EventTags::DaemonStarted {},
            EventTags::ErrorHit { error_class: ErrorClass::ParseError },
            EventTags::ErrorHit { error_class: ErrorClass::PreflightFailed },
            EventTags::ErrorHit { error_class: ErrorClass::PluginCrash },
            EventTags::ErrorHit { error_class: ErrorClass::NetworkError },
            EventTags::ErrorHit { error_class: ErrorClass::Other },
            EventTags::CliInvoked { command_group: CommandGroup::Daemon },
            EventTags::CliInvoked { command_group: CommandGroup::Subject },
            EventTags::CliInvoked { command_group: CommandGroup::Other },
            EventTags::UpdateApplied {},
        ];
        let events: Vec<Event> =
            variants.into_iter().map(|tags| Event { recorded_at: "2026-06-04T10:00:00Z".to_string(), tags }).collect();
        let metrics = MetricsConfig::default();
        let payload = build_payload(&metrics, "id", &events);
        for bucket in &payload.events {
            let obj = bucket.tags.as_object().expect("tags must be a JSON object");
            for (key, value) in obj {
                assert!(
                    value.is_string() || value.is_boolean() || value.is_number() || value.is_null(),
                    "tag {key} must be a primitive — payload privacy invariant: got {value:?}"
                );
            }
        }
    }
}
