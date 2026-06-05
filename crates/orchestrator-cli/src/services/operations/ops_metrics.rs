use std::path::Path;

use anyhow::Result;
use protocol::{metrics_env_disabled, Config};
use serde::Serialize;
use uuid::Uuid;

use crate::services::metrics::recorder::{
    last_send_timestamp, pending_event_count, read_metrics_block_without_creating,
};
use crate::services::metrics::{flush_pending, FlushOutcome};
use crate::{print_value, MetricsCommand};

#[derive(Debug, Serialize)]
struct MetricsStatus {
    enabled: Option<bool>,
    env_disabled: bool,
    endpoint: String,
    batch_interval: String,
    install_id: Option<String>,
    pending_events: usize,
    last_send: Option<String>,
}

#[derive(Debug, Serialize)]
struct MetricsActionResult {
    action: &'static str,
    enabled: Option<bool>,
    install_id: Option<String>,
    cleared_pending: bool,
    message: String,
}

#[derive(Debug, Serialize)]
struct MetricsFlushResult {
    outcome: &'static str,
    events: usize,
    buckets: usize,
    last_status: Option<u16>,
}

pub(crate) async fn handle_metrics(command: MetricsCommand, project_root: &str, json: bool) -> Result<()> {
    let project_path = Path::new(project_root);
    match command {
        MetricsCommand::Status => handle_status(project_path, json),
        MetricsCommand::Enable => handle_enable(project_path, json),
        MetricsCommand::Disable => handle_disable(project_path, json),
        MetricsCommand::Flush => handle_flush(project_path, json).await,
    }
}

fn handle_status(project_root: &Path, json: bool) -> Result<()> {
    let metrics = read_metrics_block_without_creating(project_root).unwrap_or_default();
    let status = MetricsStatus {
        enabled: metrics.enabled,
        env_disabled: metrics_env_disabled(),
        endpoint: metrics.endpoint,
        batch_interval: metrics.batch_interval,
        install_id: metrics.install_id,
        pending_events: pending_event_count(project_root),
        last_send: last_send_timestamp(project_root),
    };
    print_value(status, json)
}

fn handle_enable(_project_root: &Path, json: bool) -> Result<()> {
    let mut config = load_global_or_fresh_or_bail("enable")?;
    let mut metrics = config.metrics.clone().unwrap_or_default();
    metrics.enabled = Some(true);
    if metrics.install_id.is_none() {
        metrics.install_id = Some(Uuid::new_v4().to_string());
    }
    config.metrics = Some(metrics.clone());
    config.save_global()?;
    let result = MetricsActionResult {
        action: "enable",
        enabled: Some(true),
        install_id: metrics.install_id,
        cleared_pending: false,
        message: if metrics_env_disabled() {
            "opt-in recorded, but ANIMUS_METRICS_DISABLE=1 still suppresses emission".to_string()
        } else {
            "anonymous metrics enabled".to_string()
        },
    };
    print_value(result, json)
}

fn handle_disable(project_root: &Path, json: bool) -> Result<()> {
    // Persist explicit opt-out in the **user-global** config so the
    // first-run prompt does not re-show and any future emission paths
    // short-circuit. Materializing `~/.animus/config.json` on disable is
    // intentional: we want a durable opt-out marker.
    let mut config = load_global_or_fresh_or_bail("disable")?;
    let mut metrics = config.metrics.clone().unwrap_or_default();
    metrics.enabled = Some(false);
    metrics.install_id = None;
    config.metrics = Some(metrics);
    config.save_global()?;
    // Consent is global, so wipe every repo-scoped pending buffer
    // under `~/.animus/<scope>/metrics/`, not just this project's.
    // Otherwise a future re-enable would resurface events the user
    // just opted out of.
    let _ = project_root;
    let mut cleared = false;
    let scopes_root = protocol::Config::global_config_dir();
    if let Ok(read) = std::fs::read_dir(&scopes_root) {
        for entry in read.flatten() {
            let candidate = entry.path().join("metrics").join("pending.jsonl");
            if candidate.exists() {
                let _ = std::fs::remove_file(&candidate);
                cleared = true;
            }
        }
    }
    let result = MetricsActionResult {
        action: "disable",
        enabled: Some(false),
        install_id: None,
        cleared_pending: cleared,
        message: "anonymous metrics disabled; pending events dropped".to_string(),
    };
    print_value(result, json)
}

/// Loads the global config when the file is absent (returning a fresh
/// default) or successfully parses. Refuses to clobber an existing but
/// unreadable global config — the caller would otherwise erase
/// profiles / MCP servers / tokens on disk.
fn load_global_or_fresh_or_bail(action: &str) -> Result<Config> {
    let path = Config::global_config_dir().join("config.json");
    if !path.exists() {
        return Ok(Config {
            agent_runner_token: None,
            mcp_servers: Default::default(),
            claude_profiles: Default::default(),
            default_subject_kind: None,
            auto_update: None,
            metrics: None,
        });
    }
    Config::load_global_if_exists().ok_or_else(|| {
        anyhow::anyhow!(
            "refusing to {action} metrics: global config at {} is unreadable; fix or remove the file by hand",
            path.display()
        )
    })
}

async fn handle_flush(project_root: &Path, json: bool) -> Result<()> {
    let outcome = flush_pending(project_root).await;
    let result = match outcome {
        FlushOutcome::Disabled => MetricsFlushResult { outcome: "disabled", events: 0, buckets: 0, last_status: None },
        FlushOutcome::Empty => MetricsFlushResult { outcome: "empty", events: 0, buckets: 0, last_status: None },
        FlushOutcome::Sent { events, buckets } => {
            MetricsFlushResult { outcome: "sent", events, buckets, last_status: None }
        }
        FlushOutcome::Failed { events, last_status } => {
            MetricsFlushResult { outcome: "failed", events, buckets: 0, last_status }
        }
    };
    print_value(result, json)
}
