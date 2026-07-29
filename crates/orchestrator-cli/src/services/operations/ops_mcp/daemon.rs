use super::{
    push_bool_set, push_opt, push_opt_num, push_opt_usize, DaemonEventsInput, DaemonLogsInput, DaemonObserveInput,
    DaemonStartInput, DEFAULT_DAEMON_EVENTS_LIMIT, MAX_DAEMON_EVENTS_LIMIT,
};
use anyhow::Result;
use serde_json::{json, Value};

const DEFAULT_DAEMON_LOGS_LIMIT: usize = 100;

pub(super) fn build_daemon_start_args(input: &DaemonStartInput) -> Vec<String> {
    let mut args = vec!["daemon".to_string(), "start".to_string()];
    push_opt_usize(&mut args, "--pool-size", input.pool_size);
    push_opt_num(&mut args, "--interval-secs", input.interval_secs);
    push_opt_num(&mut args, "--stale-threshold-hours", input.stale_threshold_hours);
    push_opt_usize(&mut args, "--max-tasks-per-tick", input.max_tasks_per_tick);
    push_opt_num(&mut args, "--phase-timeout-secs", input.phase_timeout_secs);
    push_bool_set(&mut args, "--startup-cleanup", input.startup_cleanup);
    push_bool_set(&mut args, "--reconcile-stale", input.reconcile_stale);
    args
}

/// Builds args for `daemon observe`. The MCP surface deliberately never sets
/// `--follow`: it returns the merged window the CLI's non-streaming path
/// produces, so the tool always terminates.
pub(super) fn build_daemon_observe_args(input: &DaemonObserveInput) -> Vec<String> {
    let mut args = vec!["daemon".to_string(), "observe".to_string()];
    push_opt(&mut args, "--since", input.since.clone());
    push_opt(&mut args, "--source", input.source.clone());
    push_opt(&mut args, "--workflow", input.workflow_id.clone());
    push_opt_usize(&mut args, "--limit", input.limit);
    args
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_daemon_observe_args_defaults_minimal() {
        let args = build_daemon_observe_args(&DaemonObserveInput::default());
        assert_eq!(args, vec!["daemon".to_string(), "observe".to_string()]);
        // Never streams: --follow must never appear.
        assert!(!args.contains(&"--follow".to_string()));
    }

    #[test]
    fn build_daemon_observe_args_wires_all_params_without_follow() {
        let input = DaemonObserveInput {
            since: Some("2h".to_string()),
            source: Some("events".to_string()),
            workflow_id: Some("wf-abc123".to_string()),
            limit: Some(50),
            project_root: Some("/repo".to_string()),
        };

        let args = build_daemon_observe_args(&input);

        assert_eq!(
            args,
            vec![
                "daemon".to_string(),
                "observe".to_string(),
                "--since".to_string(),
                "2h".to_string(),
                "--source".to_string(),
                "events".to_string(),
                "--workflow".to_string(),
                "wf-abc123".to_string(),
                "--limit".to_string(),
                "50".to_string(),
            ]
        );
        assert!(!args.contains(&"--follow".to_string()));
    }
}
