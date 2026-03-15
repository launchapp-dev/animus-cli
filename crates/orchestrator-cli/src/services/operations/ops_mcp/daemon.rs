use super::{
    push_bool_flag, push_bool_set, push_opt, push_opt_num, push_opt_usize, DaemonConfigSetInput, DaemonEventsInput,
    DaemonLogsInput, DaemonStartInput, DEFAULT_DAEMON_EVENTS_LIMIT, MAX_DAEMON_EVENTS_LIMIT,
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
    push_opt_num(&mut args, "--idle-timeout-secs", input.idle_timeout_secs);
    push_bool_flag(&mut args, "--skip-runner", input.skip_runner);
    push_bool_flag(&mut args, "--autonomous", input.autonomous);
    push_bool_set(&mut args, "--auto-run-ready", input.auto_run_ready);
    push_bool_set(&mut args, "--auto-merge", input.auto_merge);
    push_bool_set(&mut args, "--auto-pr", input.auto_pr);
    push_bool_set(&mut args, "--auto-commit-before-merge", input.auto_commit_before_merge);
    push_bool_set(&mut args, "--auto-prune-worktrees-after-merge", input.auto_prune_worktrees_after_merge);
    push_bool_set(&mut args, "--startup-cleanup", input.startup_cleanup);
    push_bool_set(&mut args, "--resume-interrupted", input.resume_interrupted);
    push_bool_set(&mut args, "--reconcile-stale", input.reconcile_stale);
    push_opt(&mut args, "--runner-scope", input.runner_scope.clone());
    args
}

pub(super) fn build_daemon_config_set_args(input: &DaemonConfigSetInput) -> Vec<String> {
    let mut args = vec!["daemon".to_string(), "config".to_string()];
    push_bool_set(&mut args, "--auto-merge", input.auto_merge);
    push_bool_set(&mut args, "--auto-pr", input.auto_pr);
    push_bool_set(&mut args, "--auto-commit-before-merge", input.auto_commit_before_merge);
    push_bool_set(&mut args, "--auto-prune-worktrees-after-merge", input.auto_prune_worktrees_after_merge);
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
    let project_root = resolve_daemon_events_project_root(default_project_root, input.project_root);
    let limit = input.limit.unwrap_or(DEFAULT_DAEMON_LOGS_LIMIT).max(1);
    let log_path = crate::services::runtime::autonomous_daemon_log_path(&project_root);

    let content = match std::fs::read_to_string(&log_path) {
        Ok(c) => c,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Ok(json!({
                "log_path": log_path.display().to_string(),
                "line_count": 0,
                "lines": [],
                "has_more": false,
            }));
        }
        Err(err) => {
            anyhow::bail!("failed to read daemon log at {}: {}", log_path.display(), err);
        }
    };

    let mut lines: Vec<&str> = content.lines().collect();

    if let Some(ref needle) = input.search {
        lines.retain(|line| line.contains(needle.as_str()));
    }

    let total = lines.len();
    let has_more = total > limit;
    if total > limit {
        lines = lines.split_off(total - limit);
    }

    Ok(json!({
        "log_path": log_path.display().to_string(),
        "line_count": total,
        "lines": lines,
        "has_more": has_more,
    }))
}
