use std::path::Path;

use anyhow::{anyhow, Result};
use chrono::{Duration as ChronoDuration, Utc};
use orchestrator_daemon_runtime::{resolve_log_storage_dispatch, LogStorageDispatch};
use orchestrator_logging::{Level, LogEntry, Logger};
use serde::Serialize;

use crate::{print_value, LogsCommand, LogsTailArgs};

/// JSON shape for one entry served by the daemon control wire.
///
/// We round-trip through this struct rather than reuse the in-tree
/// [`LogEntry`] because the wire schema flattens the structured fields
/// payload onto `fields`. Operators inspecting the JSON envelope expect
/// the same field names regardless of which transport produced them, so
/// the wire branch projects back into the same response shape the
/// in-tree branch emits.
#[derive(Debug, Serialize)]
pub(crate) struct WireLogEntryView {
    ts: String,
    level: String,
    cat: String,
    msg: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    source: Option<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct LogsTailResponse {
    backend: &'static str,
    plugin_name: Option<String>,
    project_root: String,
    /// Transport that actually answered. `"local"` when the daemon socket
    /// was missing and the CLI read events.jsonl directly; `"wire"` when
    /// the request was routed through `ControlClient::daemon_logs`.
    transport: &'static str,
    entries: Vec<LogEntry>,
}

#[derive(Debug, Serialize)]
pub(crate) struct WireLogsTailResponse {
    backend: &'static str,
    plugin_name: Option<String>,
    project_root: String,
    transport: &'static str,
    entries: Vec<WireLogEntryView>,
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
pub(crate) enum LogsTailApplicationResponse {
    Local(LogsTailResponse),
    Wire(WireLogsTailResponse),
}

impl LogsTailApplicationResponse {
    fn entries_are_empty(&self) -> bool {
        match self {
            Self::Local(response) => response.entries.is_empty(),
            Self::Wire(response) => response.entries.is_empty(),
        }
    }

    fn local_plugin_fallback(&self) -> Option<&str> {
        match self {
            Self::Local(response) if response.backend == "plugin" && response.transport == "local" => {
                response.plugin_name.as_deref()
            }
            _ => None,
        }
    }

    fn render_human(&self) {
        match self {
            Self::Local(response) => {
                for entry in &response.entries {
                    let plugin_suffix =
                        entry.provider.as_deref().map(|provider| format!(" [{provider}]")).unwrap_or_default();
                    println!("{} {:>5} {}{} :: {}", entry.ts, entry.level, entry.cat, plugin_suffix, entry.msg);
                }
            }
            Self::Wire(response) => {
                for entry in &response.entries {
                    let plugin_suffix =
                        entry.provider.as_deref().map(|provider| format!(" [{provider}]")).unwrap_or_default();
                    println!("{} {:>5} {}{} :: {}", entry.ts, entry.level, entry.cat, plugin_suffix, entry.msg);
                }
            }
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct LogsTailApplicationRequest {
    pub(crate) plugin: Option<String>,
    pub(crate) level: String,
    pub(crate) since: String,
    pub(crate) limit: usize,
}

impl From<LogsTailArgs> for LogsTailApplicationRequest {
    fn from(args: LogsTailArgs) -> Self {
        Self { plugin: args.plugin, level: args.level, since: args.since, limit: args.limit }
    }
}

pub(crate) async fn handle_logs(command: LogsCommand, project_root: &str, json: bool) -> Result<()> {
    match command {
        LogsCommand::Tail(args) => {
            let request = LogsTailApplicationRequest::from(args);
            let response = logs_tail_application(request.clone(), project_root).await?;
            if json {
                return print_value(response, true);
            }
            if let Some(plugin) = response.local_plugin_fallback() {
                eprintln!(
                    "note: active backend is plugin '{}' but daemon is not running; \
                     reading in-tree events.jsonl. Start the daemon (`animus daemon start`) \
                     to route through the plugin.",
                    plugin
                );
            }
            if response.entries_are_empty() {
                print_empty_logs_hint(&request.since, &request.level);
            }
            response.render_human();
            Ok(())
        }
    }
}

pub(crate) async fn logs_tail_application(
    request: LogsTailApplicationRequest,
    project_root: &str,
) -> Result<LogsTailApplicationResponse> {
    let level = parse_level(&request.level).map_err(|error| crate::invalid_input_error(error.to_string()))?;
    let since_duration =
        parse_duration(&request.since).map_err(|error| crate::invalid_input_error(error.to_string()))?;
    let since_dt = Utc::now() - since_duration;
    let since_ts = since_dt.to_rfc3339();
    let limit = request.limit.max(1);

    let resolution = resolve_log_storage_dispatch(Path::new(project_root))?;
    let backend_label: &'static str = match resolution.selected.as_ref() {
        LogStorageDispatch::InTree { .. } => "in_tree",
        LogStorageDispatch::Plugin { .. } => "plugin",
    };
    let plugin_name = resolution.selected.plugin_name().map(|s| s.to_string());
    let project_root_for_response = resolution.selected.project_root().display().to_string();

    // Daemon-up: route through ControlClient::daemon_logs so a live
    // log_storage_backend plugin can intercept (today the daemon still
    // reads in-tree under the hood; the wire is the contract). Daemon-
    // down: fall back to direct events.jsonl read so `animus logs tail`
    // keeps working without a daemon process.
    if let Some(entries) =
        try_daemon_logs_via_control(project_root, level, &since_dt, request.plugin.as_deref(), limit).await?
    {
        return Ok(LogsTailApplicationResponse::Wire(WireLogsTailResponse {
            backend: backend_label,
            plugin_name,
            project_root: project_root_for_response,
            transport: "wire",
            entries,
        }));
    }

    // Daemon-down fallback: open events.jsonl directly through the
    // resolved dispatch root.
    match resolution.selected.as_ref() {
        LogStorageDispatch::InTree { project_root: pr } => {
            let logger = Logger::for_project(pr);
            let entries = read_in_tree_entries(&logger, limit, level, since_ts.as_str(), request.plugin.as_deref());
            Ok(LogsTailApplicationResponse::Local(LogsTailResponse {
                backend: "in_tree",
                plugin_name: None,
                project_root: pr.display().to_string(),
                transport: "local",
                entries,
            }))
        }
        LogStorageDispatch::Plugin { project_root: pr, plugin } => {
            // Daemon down: we can't reach the plugin, so degrade to the
            // in-tree fallback file. The CLI renderer tells the operator what
            // happened; typed application callers receive transport="local".
            let logger = Logger::for_project(pr);
            let entries = read_in_tree_entries(&logger, limit, level, since_ts.as_str(), request.plugin.as_deref());
            Ok(LogsTailApplicationResponse::Local(LogsTailResponse {
                backend: "plugin",
                plugin_name: Some(plugin.name.clone()),
                project_root: pr.display().to_string(),
                transport: "local",
                entries,
            }))
        }
    }
}

/// Try the daemon control wire for `daemon/logs`. Returns `Ok(None)`
/// when the daemon socket is absent (so the caller falls back to the
/// local file reader) or when the wire reports the method unavailable.
async fn try_daemon_logs_via_control(
    project_root: &str,
    level: Level,
    since: &chrono::DateTime<Utc>,
    plugin_filter: Option<&str>,
    limit: usize,
) -> Result<Option<Vec<WireLogEntryView>>> {
    use animus_control_protocol::types::DaemonLogsRequest;
    use animus_log_storage_protocol::LogLevel as WireLogLevel;
    use orchestrator_daemon_runtime::control::{is_method_unavailable, ControlClient};

    let project_root_path = Path::new(project_root);
    let Some(client) = ControlClient::try_connect(project_root_path).await? else {
        return Ok(None);
    };
    let wire_level = match level {
        Level::Debug => WireLogLevel::Debug,
        Level::Info => WireLogLevel::Info,
        Level::Warn => WireLogLevel::Warn,
        Level::Error => WireLogLevel::Error,
    };
    let request = DaemonLogsRequest {
        since: Some(*since),
        level: Some(wire_level),
        plugin: plugin_filter.map(|s| s.to_string()),
        follow: false,
    };

    match client.daemon_logs(request, limit).await {
        Ok(entries) => {
            let view: Vec<WireLogEntryView> = entries
                .into_iter()
                .map(|e| {
                    let provider = match e.source {
                        animus_log_storage_protocol::LogSource::Plugin => e.source_name.clone(),
                        _ => None,
                    };
                    WireLogEntryView {
                        ts: e.ts.to_rfc3339(),
                        level: format!("{:?}", e.level).to_lowercase(),
                        cat: e.target,
                        msg: e.message,
                        provider,
                        source: Some(format!("{:?}", e.source).to_lowercase()),
                    }
                })
                .collect();
            Ok(Some(view))
        }
        Err(err) if is_method_unavailable(&err) => {
            tracing::debug!(error = %err, "daemon/logs wire returned unavailable; falling back to local");
            Ok(None)
        }
        Err(err) => Err(err),
    }
}

/// Print a stderr hint when a `logs tail` window returns no entries, so an
/// empty result reads as "nothing matched your filters" rather than a silent
/// success.
fn print_empty_logs_hint(since: &str, level: &str) {
    eprintln!(
        "no log entries in the last {since} (default --since 1h, --level {level}); \
         try --since 24h or --level debug"
    );
}

fn read_in_tree_entries(
    logger: &Logger,
    limit: usize,
    level: Level,
    since_ts: &str,
    plugin_filter: Option<&str>,
) -> Vec<LogEntry> {
    // Overscan and then trim from the most-recent side. `read_entries_since`
    // returns entries in chronological order (oldest first); we want the
    // `limit` MOST RECENT matches, so we filter, keep the tail, and let
    // the CLI render them oldest-first for human-readability.
    let candidate_pool = limit.saturating_mul(4).max(limit * 4);
    let entries = logger.read_entries_since(candidate_pool, None, Some(level), Some(since_ts));
    let mut filtered: Vec<LogEntry> = entries
        .into_iter()
        .filter(|e| {
            plugin_filter.map(|name| e.provider.as_deref() == Some(name) || e.meta_plugin_matches(name)).unwrap_or(true)
        })
        .collect();
    if filtered.len() > limit {
        let drop = filtered.len() - limit;
        filtered.drain(0..drop);
    }
    filtered
}

fn parse_level(raw: &str) -> Result<Level> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "debug" => Ok(Level::Debug),
        "info" => Ok(Level::Info),
        "warn" | "warning" => Ok(Level::Warn),
        "error" | "err" => Ok(Level::Error),
        other => Err(anyhow!("invalid --level value '{other}'; expected one of debug|info|warn|error")),
    }
}

fn parse_duration(raw: &str) -> Result<ChronoDuration> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(anyhow!("--since cannot be empty"));
    }
    if trimmed.starts_with('-') {
        return Err(anyhow!("--since '{}' is negative; duration must be positive (e.g. 30s, 5m, 2h, 1d)", trimmed));
    }
    let (num_part, unit) = trimmed.split_at(
        trimmed
            .find(|c: char| !c.is_ascii_digit() && c != '.')
            .ok_or_else(|| anyhow!("--since '{}' missing unit (use s/m/h/d)", trimmed))?,
    );
    let value: i64 = num_part
        .parse::<f64>()
        .map(|v| v as i64)
        .map_err(|_| anyhow!("--since '{}' has invalid numeric component", trimmed))?;
    match unit.trim() {
        "s" | "sec" | "secs" | "seconds" => Ok(ChronoDuration::seconds(value.max(0))),
        "m" | "min" | "mins" | "minutes" => Ok(ChronoDuration::minutes(value.max(0))),
        "h" | "hr" | "hrs" | "hour" | "hours" => Ok(ChronoDuration::hours(value.max(0))),
        "d" | "day" | "days" => Ok(ChronoDuration::days(value.max(0))),
        other => Err(anyhow!(
            "--since '{}' has unknown unit '{}'; expected a single number with one unit suffix (s/m/h/d), e.g. 30s, 5m, 2h, 1d — compound durations like 1h30m are not supported",
            trimmed,
            other
        )),
    }
}

/// Extension trait: search the structured meta payload for a plugin
/// reference. Provider plugin fan-out writes a `meta.plugin` field, so
/// `--plugin` should match against it too.
trait LogEntryMetaExt {
    fn meta_plugin_matches(&self, name: &str) -> bool;
}

impl LogEntryMetaExt for LogEntry {
    fn meta_plugin_matches(&self, name: &str) -> bool {
        match self.meta.as_ref().and_then(|v| v.get("plugin")).and_then(|v| v.as_str()) {
            Some(plugin) => plugin == name,
            None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn parse_level_accepts_canonical_names() {
        assert!(matches!(parse_level("debug").unwrap(), Level::Debug));
        assert!(matches!(parse_level("INFO").unwrap(), Level::Info));
        assert!(matches!(parse_level("warning").unwrap(), Level::Warn));
        assert!(matches!(parse_level("err").unwrap(), Level::Error));
        assert!(parse_level("trace").is_err(), "trace is not a supported level");
    }

    #[test]
    fn parse_duration_accepts_common_shortcuts() {
        assert_eq!(parse_duration("1h").unwrap(), ChronoDuration::hours(1));
        assert_eq!(parse_duration("30m").unwrap(), ChronoDuration::minutes(30));
        assert_eq!(parse_duration("15s").unwrap(), ChronoDuration::seconds(15));
        assert_eq!(parse_duration("2d").unwrap(), ChronoDuration::days(2));
        assert!(parse_duration("forever").is_err(), "non-numeric prefix must fail");
        assert!(parse_duration("10").is_err(), "missing unit must fail");
    }

    #[test]
    fn parse_duration_explains_negative_and_compound_inputs() {
        let negative = parse_duration("-5h").expect_err("negative duration must fail");
        assert!(negative.to_string().contains("duration must be positive"), "{negative}");

        let compound = parse_duration("1h30m").expect_err("compound duration must fail");
        let message = compound.to_string();
        assert!(message.contains("unknown unit 'h30m'"), "{message}");
        assert!(message.contains("compound durations like 1h30m are not supported"), "{message}");
    }

    #[test]
    fn logs_tail_in_tree_reads_events_jsonl() {
        // Write a fixture events.jsonl with mixed levels and read it back
        // through the same reader logs_tail uses, with --level=warn and
        // --limit=3.
        let temp = tempfile::tempdir().expect("tempdir");
        let logs_dir = temp.path().join(".animus/logs");
        fs::create_dir_all(&logs_dir).expect("mkdir");

        let logger = Logger::open(&logs_dir, "events.jsonl", Level::Debug);
        logger.debug("test", "debug-noise").emit();
        logger.info("test", "info-baseline").emit();
        logger.warn("test", "warn-one").emit();
        logger.warn("test", "warn-two").provider("kimi-code").emit();
        logger.error("test", "error-one").emit();
        logger.info("test", "info-trailing").emit();

        let since_ts = (Utc::now() - ChronoDuration::hours(1)).to_rfc3339();
        let entries = read_in_tree_entries(&logger, 3, Level::Warn, since_ts.as_str(), None);

        assert_eq!(entries.len(), 3, "expected 3 entries at warn+ level, got {:?}", entries.len());
        let messages: Vec<&str> = entries.iter().map(|e| e.msg.as_str()).collect();
        assert!(messages.contains(&"warn-one"), "warn-one missing: {messages:?}");
        assert!(messages.contains(&"warn-two"), "warn-two missing: {messages:?}");
        assert!(messages.contains(&"error-one"), "error-one missing: {messages:?}");
        assert!(!messages.iter().any(|m| m == &"debug-noise"), "debug entries must be filtered out");
        assert!(!messages.iter().any(|m| m == &"info-baseline"), "info entries must be filtered out");

        // Plugin filter narrows the set further.
        let plugin_filtered = read_in_tree_entries(&logger, 10, Level::Warn, since_ts.as_str(), Some("kimi-code"));
        assert_eq!(plugin_filtered.len(), 1, "expected exactly one warn-and-above entry tagged kimi-code");
        assert_eq!(plugin_filtered[0].msg, "warn-two");
    }
}
