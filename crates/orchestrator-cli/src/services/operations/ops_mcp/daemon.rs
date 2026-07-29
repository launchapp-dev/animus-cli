use super::{DaemonEventsInput, DaemonLogsInput, DEFAULT_DAEMON_EVENTS_LIMIT, MAX_DAEMON_EVENTS_LIMIT};
use anyhow::Result;
use serde_json::{json, Value};

const DEFAULT_DAEMON_LOGS_LIMIT: usize = 100;

pub(super) fn daemon_events_poll_limit(limit: Option<usize>) -> usize {
    let normalized = limit.unwrap_or(DEFAULT_DAEMON_EVENTS_LIMIT).max(1);
    normalized.min(MAX_DAEMON_EVENTS_LIMIT)
}

pub(super) fn resolve_daemon_events_project_root(
    default_project_root: &str,
    project_root_override: Option<String>,
) -> String {
    let candidate = project_root_override
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| default_project_root.to_string());
    crate::services::runtime::canonicalize_lossy(candidate.as_str())
}

pub(super) fn build_daemon_events_poll_result(default_project_root: &str, input: DaemonEventsInput) -> Result<Value> {
    let project_root = resolve_daemon_events_project_root(default_project_root, input.project_root);
    let limit = daemon_events_poll_limit(input.limit);
    let response = crate::services::runtime::poll_daemon_events(Some(limit), Some(project_root.as_str()))?;
    Ok(json!({
        "schema": response.schema,
        "events_path": response.events_path,
        "project_root": project_root,
        "limit": limit,
        "count": response.count,
        "events": response.events,
    }))
}

pub(super) fn build_daemon_logs_result(default_project_root: &str, input: DaemonLogsInput) -> Result<Value> {
    use orchestrator_logging::Logger;

    let project_root = resolve_daemon_events_project_root(default_project_root, input.project_root);
    let limit = input.limit.unwrap_or(DEFAULT_DAEMON_LOGS_LIMIT).max(1);
    let logger = Logger::for_project(std::path::Path::new(&project_root));

    let entries = logger.read_entries(limit * 2, None, None);
    let mut lines: Vec<String> = entries.iter().map(|e| serde_json::to_string(e).unwrap_or_default()).collect();

    if let Some(ref needle) = input.search {
        lines.retain(|line| line.contains(needle.as_str()));
    }

    let total = lines.len();
    let has_more = total > limit;
    if total > limit {
        lines = lines.split_off(total - limit);
    }

    Ok(json!({
        "log_path": logger.path().display().to_string(),
        "line_count": lines.len(),
        "lines": lines,
        "has_more": has_more,
    }))
}
