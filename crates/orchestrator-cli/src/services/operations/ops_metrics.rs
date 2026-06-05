use anyhow::Result;
use serde_json::json;

use crate::services::metrics::{self, HttpTransport};
use crate::{print_value, MetricsCommand};

pub(crate) async fn handle_metrics(command: MetricsCommand, json: bool) -> Result<()> {
    match command {
        MetricsCommand::Status => handle_status(json),
        MetricsCommand::Enable => handle_enable(json),
        MetricsCommand::Disable => handle_disable(json),
        MetricsCommand::Flush => handle_flush(json).await,
    }
}

fn handle_status(json: bool) -> Result<()> {
    let snapshot = metrics::status_snapshot();
    let resolved = if snapshot.env_disabled {
        "disabled (ANIMUS_METRICS_DISABLE)"
    } else {
        match snapshot.enabled {
            Some(true) => "enabled",
            Some(false) => "disabled",
            None => "never-asked",
        }
    };
    let value = json!({
        "enabled": snapshot.enabled,
        "env_disabled": snapshot.env_disabled,
        "resolved": resolved,
        "install_id": snapshot.install_id,
        "endpoint": snapshot.endpoint,
        "batch_interval": snapshot.batch_interval,
        "pending_count": snapshot.pending_count,
        "last_send": {
            "last_attempt_at": snapshot.last_send.last_attempt_at,
            "last_success_at": snapshot.last_send.last_success_at,
            "last_event_count": snapshot.last_send.last_event_count,
            "last_error": snapshot.last_send.last_error,
        }
    });
    print_value(value, json)
}

fn handle_enable(json: bool) -> Result<()> {
    metrics::set_enabled(true)?;
    let snapshot = metrics::status_snapshot();
    print_value(
        json!({
            "message": "metrics enabled",
            "install_id": snapshot.install_id,
            "endpoint": snapshot.endpoint,
        }),
        json,
    )
}

fn handle_disable(json: bool) -> Result<()> {
    metrics::set_enabled(false)?;
    print_value(json!({"message": "metrics disabled; pending events dropped"}), json)
}

async fn handle_flush(json: bool) -> Result<()> {
    let transport = HttpTransport::new();
    let report = metrics::flush(&transport).await?;
    print_value(
        json!({
            "skipped": report.skipped,
            "attempted_count": report.attempted_count,
            "sent_events": report.sent_events,
            "error": report.error,
        }),
        json,
    )
}
