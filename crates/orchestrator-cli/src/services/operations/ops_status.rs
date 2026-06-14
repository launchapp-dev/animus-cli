use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command as ProcessCommand, Stdio};
use std::sync::OnceLock;

use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Utc};
use orchestrator_core::{
    load_active_workflow_summaries, load_daemon_health_snapshot, load_recent_failed_workflow_summaries,
    open_project_db, DaemonHealth, DaemonStatus, WorkflowActivitySummary,
};
#[cfg(test)]
use orchestrator_core::{OrchestratorTask, TaskStatus};
use orchestrator_daemon_runtime::resolve_subject_dispatch;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::print_value;

const STATUS_SCHEMA: &str = "animus.status.v1";
const RECENT_COMPLETIONS_LIMIT: usize = 5;
const CI_PROVIDER_GITHUB: &str = "github";
const GH_RUN_LIST_FIELDS: &str =
    "databaseId,displayTitle,name,workflowName,status,conclusion,event,headBranch,headSha,createdAt,updatedAt,url";

const CI_CACHE_SCHEMA: &str = "animus.cache.ci.v1";
const CI_CACHE_DEFAULT_TTL_SECS: u64 = 60;

#[derive(Debug, Clone, Serialize)]
struct StatusDashboard {
    schema: &'static str,
    project_root: String,
    generated_at: DateTime<Utc>,
    /// v0.5: active flavor id (typically `"default"`). Sourced from
    /// `flavors/<name>.toml` discovery; `None` when no flavor manifest
    /// is found. Exposed for `animus status --json | jq '.flavor'`.
    #[serde(skip_serializing_if = "Option::is_none")]
    flavor: Option<String>,
    daemon: DaemonStatusSlice,
    /// Wave-2: proactive degraded-state + agent-silence rollup. Empty
    /// `reasons` and zero `silent_agents` means nothing to flag.
    warnings: WarningsSlice,
    active_agents: ActiveAgentsSlice,
    task_summary: TaskSummarySlice,
    blocked_subjects: BlockedSubjectsSlice,
    needs_you: NeedsYouSlice,
    recent_completions: RecentCompletionsSlice,
    recent_failures: RecentFailuresSlice,
    ci: CiStatusSlice,
    budget: BudgetSlice,
}

/// Active budget-breach rollup for the dashboard. A breach is "active" when
/// its `on_exceed` is `pause` and the breaching workflow is still paused —
/// see `crate::services::cost::breach_summary` for the heuristic.
#[derive(Debug, Clone, Serialize)]
struct BudgetSlice {
    available: bool,
    enforcement_enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_sweep_at: Option<String>,
    breaches: crate::services::cost::BudgetBreachSummary,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct DaemonStatusSlice {
    available: bool,
    status: String,
    running: bool,
    runtime_paused: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    paused_at: Option<String>,
    /// v0.5.3 replacement for the deprecated `runner_connected` field: true
    /// when the daemon's provider plugins are all healthy.
    provider_plugins_healthy: bool,
    /// Deprecated wire fields kept for `--json` envelope back-compat. The
    /// human dashboard renders `provider_plugins_healthy` instead. Always
    /// `false` / `None` on the wire.
    runner_connected: bool,
    runner_pid: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

/// Wave-2: surfaces conditions that keep a live daemon out of a clean
/// "healthy" state. `degraded_reasons` carries actionable subject-router /
/// plugin-cap text from `DaemonHealth`; `silent_agents` counts active
/// agents that have crossed the silence threshold.
#[derive(Debug, Clone, Serialize)]
struct WarningsSlice {
    degraded: bool,
    degraded_reasons: Vec<String>,
    silent_agents: usize,
}

#[derive(Debug, Clone, Serialize)]
struct ActiveAgentsSlice {
    available: bool,
    count: usize,
    assignments: Vec<ActiveAgentAssignment>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct ActiveAgentAssignment {
    task_id: String,
    task_title: String,
    workflow_id: String,
    phase_id: String,
    attributed: bool,
    /// RFC3339 timestamp of the agent's most recent output event, derived
    /// from the newest output JSONL in the workflow's latest run dir.
    /// `None` when no run output is on disk yet (e.g. a just-started phase
    /// or an unattributed placeholder slot).
    #[serde(skip_serializing_if = "Option::is_none")]
    last_output_at: Option<DateTime<Utc>>,
    /// Whole seconds since `last_output_at`. `None` when `last_output_at`
    /// is unknown.
    #[serde(skip_serializing_if = "Option::is_none")]
    silent_for_secs: Option<u64>,
    /// `true` when `silent_for_secs` has crossed the configured
    /// `silent_threshold_mins` — the agent is marked SILENT in the
    /// dashboard. Always `false` when the threshold is `0` (disabled) or
    /// `last_output_at` is unknown.
    silent: bool,
}

#[derive(Debug, Clone, Serialize)]
struct TaskSummarySlice {
    available: bool,
    total: usize,
    done: usize,
    in_progress: usize,
    ready: usize,
    blocked: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct BlockedSubjectsSlice {
    available: bool,
    count: usize,
    entries: Vec<BlockedSubjectEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct BlockedSubjectEntry {
    id: String,
    /// `"blocked"` or `"paused"` — which stuck state put it here.
    state: &'static str,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    blocked_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    blocked_by: Option<String>,
    /// Human age string (e.g. `"2d"`) derived from `blocked_at`/`updated_at`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    age: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct NeedsYouSlice {
    available: bool,
    count: usize,
    entries: Vec<NeedsYouEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct NeedsYouEntry {
    id: String,
    /// `"question"` or `"approval"`.
    kind: &'static str,
    agent: String,
    summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    age: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    timeout_remaining: Option<String>,
    answer_command: String,
}

#[derive(Debug, Clone, Serialize)]
struct RecentCompletionsSlice {
    available: bool,
    entries: Vec<RecentCompletionEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct RecentCompletionEntry {
    task_id: String,
    title: String,
    completed_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize)]
struct RecentFailuresSlice {
    available: bool,
    entries: Vec<RecentFailureEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct RecentFailureEntry {
    workflow_id: String,
    task_id: String,
    phase_id: String,
    failed_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    failure_reason: Option<String>,
}

#[derive(Debug)]
struct WorkflowStatusSnapshot {
    active_workflows: Vec<WorkflowActivitySummary>,
    recent_failures: Vec<RecentFailureEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CiStatusSlice {
    #[serde(skip_deserializing, default = "default_ci_provider")]
    provider: &'static str,
    available: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_run: Option<CiRunSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    /// `true` when this slice was served from the on-disk CI cache rather
    /// than a fresh `gh run list` shell-out. Lets the UI say "as of 30s
    /// ago". Skipped from JSON when `false` to keep the wire compact.
    #[serde(default, skip_serializing_if = "is_false")]
    cached: bool,
}

fn default_ci_provider() -> &'static str {
    CI_PROVIDER_GITHUB
}

fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Debug, Serialize, Deserialize)]
struct CiCacheFile {
    schema: String,
    fetched_at: DateTime<Utc>,
    ttl_seconds: u64,
    payload: CiStatusSlice,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CiRunSummary {
    id: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    workflow_name: Option<String>,
    status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    conclusion: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    event: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    head_branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    head_sha: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    created_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    updated_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    url: Option<String>,
}

#[derive(Debug, Clone)]
#[allow(clippy::large_enum_variant)]
enum CiLookupOutcome {
    Unavailable(String),
    Success(Option<CiRunSummary>),
    Failure(String),
}

#[derive(Debug, Clone, Deserialize)]
struct GhRunListEntry {
    #[serde(default, rename = "databaseId")]
    database_id: Option<u64>,
    #[serde(default, rename = "displayTitle")]
    display_title: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default, rename = "workflowName")]
    workflow_name: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    conclusion: Option<String>,
    #[serde(default)]
    event: Option<String>,
    #[serde(default, rename = "headBranch")]
    head_branch: Option<String>,
    #[serde(default, rename = "headSha")]
    head_sha: Option<String>,
    #[serde(default, rename = "createdAt")]
    created_at: Option<DateTime<Utc>>,
    #[serde(default, rename = "updatedAt")]
    updated_at: Option<DateTime<Utc>>,
    #[serde(default)]
    url: Option<String>,
}

pub(crate) async fn handle_status(project_root: &str, failures: usize, json: bool) -> Result<()> {
    let (daemon_result, subjects_result, workflow_snapshot_result, recent_completions_result, ci_slice) = tokio::join!(
        load_daemon_health_snapshot(Path::new(project_root)),
        collect_subjects_via_router(project_root),
        collect_workflow_status_snapshot(project_root, failures),
        collect_recent_completions(project_root),
        collect_ci_status(project_root),
    );

    let (daemon_health, daemon_error) = split_result(daemon_result);
    let (subjects, subjects_error) = split_result(subjects_result);
    let (workflow_snapshot, workflows_error) = split_result(workflow_snapshot_result);
    let (recent_completions, recent_completions_error) = split_result(recent_completions_result);
    let (task_titles, task_titles_error) = match workflow_snapshot
        .as_ref()
        .map(|snapshot| snapshot.active_workflows.iter().map(|workflow| workflow.task_id.clone()).collect::<Vec<_>>())
    {
        Some(task_ids) => split_result(load_task_titles(project_root, &task_ids)),
        None => (None, None),
    };

    let silence = build_silence_context(
        project_root,
        workflow_snapshot.as_ref().map(|snapshot| snapshot.active_workflows.as_slice()).unwrap_or_default(),
    );

    let mut dashboard = StatusDashboard {
        schema: STATUS_SCHEMA,
        project_root: project_root.to_string(),
        generated_at: Utc::now(),
        flavor: orchestrator_core::flavor::load_flavor_in(
            std::path::Path::new(project_root),
            orchestrator_core::flavor::DEFAULT_FLAVOR_ID,
        )
        .ok()
        .flatten()
        .map(|m| m.id),
        daemon: build_daemon_slice(daemon_health.as_ref(), daemon_error.clone()),
        warnings: WarningsSlice { degraded: false, degraded_reasons: Vec::new(), silent_agents: 0 },
        active_agents: build_active_agents_slice(
            daemon_health.as_ref(),
            workflow_snapshot.as_ref().map(|snapshot| snapshot.active_workflows.as_slice()),
            task_titles.as_ref(),
            &silence,
            combine_errors([daemon_error.as_deref(), workflows_error.as_deref(), task_titles_error.as_deref()]),
        ),
        task_summary: build_task_summary_slice_from_router(subjects.as_deref(), subjects_error.clone()),
        blocked_subjects: build_blocked_subjects_slice(subjects.as_deref(), subjects_error.clone()),
        needs_you: build_needs_you_slice(project_root),
        recent_completions: build_recent_completions_entries_slice(
            recent_completions.as_deref(),
            recent_completions_error,
        ),
        recent_failures: build_recent_failures_slice(
            workflow_snapshot.as_ref().map(|snapshot| snapshot.recent_failures.as_slice()),
            workflows_error,
        ),
        ci: ci_slice,
        budget: build_budget_slice(
            project_root,
            workflow_snapshot.as_ref().map(|snapshot| snapshot.active_workflows.as_slice()),
        ),
    };

    dashboard.warnings = build_warnings_slice(daemon_health.as_ref(), &dashboard.active_agents);

    if json {
        return print_value(dashboard, true);
    }

    println!("{}", render_status_dashboard(&dashboard));
    Ok(())
}

/// Fetch the `kind=task` subject list through the same `SubjectRouter` path
/// `animus subject list` uses, so the dashboard's Task Summary reflects what
/// the installed subject_backend plugin actually holds (not the legacy
/// in-tree store, which is empty when subjects live in a plugin). Returns the
/// raw subject objects; aggregation happens in the slice builders.
async fn collect_subjects_via_router(project_root: &str) -> Result<Vec<Value>> {
    let resolution = resolve_subject_dispatch(Path::new(project_root)).await?;
    if resolution.selected.plugin_count() == 0 {
        return Err(anyhow!(
            "no subject_backend plugin is mounted for kind 'task'; install one with \
             `animus plugin install-defaults --include-subjects`"
        ));
    }
    let mut subjects = Vec::new();
    let mut cursor: Option<Value> = None;
    // Page through every `task/list` response so the dashboard counts the
    // whole store, not just the first page, for cursor-paginating backends.
    // Bounded to stop a misbehaving backend that never clears `next_cursor`
    // from looping forever.
    for _ in 0..MAX_SUBJECT_LIST_PAGES {
        let mut params = serde_json::Map::new();
        params.insert("kind".to_string(), serde_json::json!(["task"]));
        if let Some(cursor) = cursor.take() {
            params.insert("cursor".to_string(), cursor);
        }
        let result = resolution
            .selected
            .route_call("task/list", Some(Value::Object(params)))
            .await
            .map_err(|error| anyhow!("subject call 'task/list' failed ({}): {}", error.code, error.message))?;
        subjects.extend(extract_subject_list(&result));
        match extract_next_cursor(&result) {
            Some(next) => cursor = Some(next),
            None => break,
        }
    }
    Ok(subjects)
}

/// Upper bound on `task/list` pages followed by [`collect_subjects_via_router`].
/// At the default backend page size this covers very large stores while
/// guaranteeing the dashboard never hangs on a backend that returns a
/// non-empty `next_cursor` indefinitely.
const MAX_SUBJECT_LIST_PAGES: usize = 1_000;

/// Pull a non-empty pagination cursor out of a `task/list` response. Returns
/// `None` when the backend omits `next_cursor`, sets it to null, or returns an
/// empty string — any of which signals the final page.
fn extract_next_cursor(result: &Value) -> Option<Value> {
    let cursor = result.as_object()?.get("next_cursor")?;
    match cursor {
        Value::Null => None,
        Value::String(s) if s.trim().is_empty() => None,
        other => Some(other.clone()),
    }
}

/// Pull the array of subject objects out of a `task/list` response. Backends
/// vary in envelope shape, so probe the common wrappers (`items`, `subjects`,
/// `tasks`, `results`) before falling back to a bare top-level array.
fn extract_subject_list(result: &Value) -> Vec<Value> {
    if let Value::Array(items) = result {
        return items.clone();
    }
    if let Value::Object(map) = result {
        for key in ["items", "subjects", "tasks", "results"] {
            if let Some(Value::Array(items)) = map.get(key) {
                return items.clone();
            }
        }
    }
    Vec::new()
}

/// Normalize a backend status string to the dashboard's canonical buckets.
/// Backends differ on casing/spelling (`in_progress` vs `in-progress`), so
/// fold everything to lowercase with `-`/`_` collapsed.
fn normalize_status(raw: &str) -> String {
    raw.trim().to_ascii_lowercase().replace('_', "-")
}

fn subject_status(subject: &Value) -> Option<String> {
    subject.get("status").and_then(Value::as_str).map(normalize_status)
}

fn subject_id(subject: &Value) -> String {
    for key in ["id", "subject_id", "task_id"] {
        if let Some(id) = subject.get(key).and_then(Value::as_str) {
            if !id.trim().is_empty() {
                return id.to_string();
            }
        }
    }
    "unknown".to_string()
}

fn subject_str(subject: &Value, key: &str) -> Option<String> {
    subject.get(key).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(ToOwned::to_owned)
}

/// Human age string from an RFC3339 timestamp relative to now (`"2d"`,
/// `"3h"`, `"5m"`, `"just now"`). `None` when the timestamp is absent or
/// unparseable.
fn age_from_timestamp(raw: Option<&str>) -> Option<String> {
    let raw = raw?.trim();
    if raw.is_empty() {
        return None;
    }
    let parsed = DateTime::parse_from_rfc3339(raw).ok()?.with_timezone(&Utc);
    let delta = Utc::now() - parsed;
    let secs = delta.num_seconds();
    if secs < 0 {
        return None;
    }
    Some(if secs >= 86_400 {
        format!("{}d", secs / 86_400)
    } else if secs >= 3_600 {
        format!("{}h", secs / 3_600)
    } else if secs >= 60 {
        format!("{}m", secs / 60)
    } else {
        "just now".to_string()
    })
}

fn split_result<T>(result: Result<T>) -> (Option<T>, Option<String>) {
    match result {
        Ok(value) => (Some(value), None),
        Err(error) => (None, Some(error.to_string())),
    }
}

fn combine_errors<'a>(errors: impl IntoIterator<Item = Option<&'a str>>) -> Option<String> {
    let messages: Vec<&str> =
        errors.into_iter().flatten().map(str::trim).filter(|message| !message.is_empty()).collect();
    if messages.is_empty() {
        return None;
    }
    Some(messages.join("; "))
}

fn build_daemon_slice(health: Option<&DaemonHealth>, error: Option<String>) -> DaemonStatusSlice {
    match health {
        Some(health) => DaemonStatusSlice {
            available: true,
            status: daemon_status_label(health.status).to_string(),
            running: daemon_running(health.status),
            runtime_paused: health.runtime_paused,
            paused_at: health.paused_at.clone(),
            provider_plugins_healthy: health.provider_plugins_healthy,
            // Deprecated wire fields: always `false` / `None`.
            runner_connected: false,
            runner_pid: None,
            error,
        },
        None => DaemonStatusSlice {
            available: false,
            status: "unknown".to_string(),
            running: false,
            runtime_paused: false,
            paused_at: None,
            provider_plugins_healthy: false,
            runner_connected: false,
            runner_pid: None,
            error,
        },
    }
}

/// Build the budget slice from scoped state. When the active-workflow list
/// is available, a breach counts as "active" only while its workflow is
/// still paused (the resolution heuristic); otherwise the rollup falls back
/// to a 24h recency window.
fn build_budget_slice(project_root: &str, active_workflows: Option<&[WorkflowActivitySummary]>) -> BudgetSlice {
    let path = Path::new(project_root);
    let enforcement = crate::services::cost::load_budget_enforcement_status(path);
    let (enforcement_enabled, last_sweep_at) = match enforcement {
        Some(status) => (status.enabled, Some(status.last_sweep_at.to_rfc3339())),
        None => (crate::services::cost::budget_enforcement_enabled(), None),
    };
    match crate::services::cost::read_decision_records(path) {
        Ok(records) => {
            let paused: Option<std::collections::HashSet<String>> = active_workflows.map(|workflows| {
                workflows.iter().filter(|w| w.status == "paused").map(|w| w.workflow_id.clone()).collect()
            });
            let breaches = crate::services::cost::summarize_breaches(&records, paused.as_ref());
            BudgetSlice { available: true, enforcement_enabled, last_sweep_at, breaches, error: None }
        }
        Err(error) => BudgetSlice {
            available: false,
            enforcement_enabled,
            last_sweep_at,
            breaches: crate::services::cost::summarize_breaches(&[], None),
            error: Some(error.to_string()),
        },
    }
}

fn daemon_running(status: DaemonStatus) -> bool {
    matches!(status, DaemonStatus::Running | DaemonStatus::Paused)
}

fn daemon_status_label(status: DaemonStatus) -> &'static str {
    match status {
        DaemonStatus::Starting => "starting",
        DaemonStatus::Running => "running",
        DaemonStatus::Paused => "paused",
        DaemonStatus::Stopping => "stopping",
        DaemonStatus::Stopped => "stopped",
        DaemonStatus::Crashed => "crashed",
    }
}

/// Wave-2: per-workflow last-output timestamps plus the resolved silence
/// threshold, used to decorate active-agent assignments. `now` is captured
/// once so all silence durations in a single dashboard render share a clock.
struct SilenceContext {
    last_output_at: HashMap<String, DateTime<Utc>>,
    threshold_secs: u64,
    now: DateTime<Utc>,
}

impl SilenceContext {
    /// Empty context (no on-disk output known, detection disabled). Used by
    /// unit tests and as the fallback when the silence scan is skipped.
    #[cfg(test)]
    fn empty() -> Self {
        SilenceContext { last_output_at: HashMap::new(), threshold_secs: 0, now: Utc::now() }
    }

    /// Resolve `(last_output_at, silent_for_secs, silent)` for a workflow.
    fn for_workflow(&self, workflow_id: &str) -> (Option<DateTime<Utc>>, Option<u64>, bool) {
        let Some(last_output_at) = self.last_output_at.get(workflow_id).copied() else {
            return (None, None, false);
        };
        let silent_for_secs = (self.now - last_output_at).num_seconds().max(0) as u64;
        let silent = self.threshold_secs > 0 && silent_for_secs >= self.threshold_secs;
        (Some(last_output_at), Some(silent_for_secs), silent)
    }
}

/// Build the [`SilenceContext`] for the dashboard by resolving each running
/// workflow's most recent output JSONL mtime. The threshold comes from
/// `pm-config.json` (`silent_threshold_mins`, defaulting to
/// [`orchestrator_core::DEFAULT_SILENT_THRESHOLD_MINS`]).
fn build_silence_context(project_root: &str, workflows: &[WorkflowActivitySummary]) -> SilenceContext {
    let threshold_mins = orchestrator_core::resolve_silent_threshold_mins(Path::new(project_root));
    let threshold_secs = threshold_mins.saturating_mul(60);
    let mut last_output_at = HashMap::new();
    for workflow in workflows {
        if let Some(at) = latest_output_at_for_workflow(project_root, &workflow.workflow_id) {
            last_output_at.insert(workflow.workflow_id.clone(), at);
        }
    }
    SilenceContext { last_output_at, threshold_secs, now: Utc::now() }
}

/// Resolve the most recent output timestamp for a running workflow: the
/// newest mtime across the known output JSONL files in the workflow's latest
/// run dir. Returns `None` when no run dir or output file exists yet.
fn latest_output_at_for_workflow(project_root: &str, workflow_id: &str) -> Option<DateTime<Utc>> {
    let (_run_id, run_dir) =
        super::ops_mcp::output_tail_resolution::resolve_latest_workflow_run_dir(project_root, workflow_id).ok()??;
    const OUTPUT_FILES: [&str; 6] =
        ["json-output.jsonl", "stdout.jsonl", "events.jsonl", "system.jsonl", "stderr.jsonl", "signals.jsonl"];
    let mut newest: Option<std::time::SystemTime> = None;
    for file_name in OUTPUT_FILES {
        let path = run_dir.join(file_name);
        if let Ok(modified) = std::fs::metadata(&path).and_then(|meta| meta.modified()) {
            newest = Some(newest.map_or(modified, |current| current.max(modified)));
        }
    }
    newest.map(DateTime::<Utc>::from)
}

fn build_active_agents_slice(
    daemon_health: Option<&DaemonHealth>,
    workflows: Option<&[WorkflowActivitySummary]>,
    task_titles: Option<&HashMap<String, String>>,
    silence: &SilenceContext,
    error: Option<String>,
) -> ActiveAgentsSlice {
    let count = daemon_health.map(|health| health.active_agents).unwrap_or(0);
    let empty_titles = HashMap::new();
    let assignments =
        active_agent_assignments(count, workflows.unwrap_or_default(), task_titles.unwrap_or(&empty_titles), silence);
    ActiveAgentsSlice { available: daemon_health.is_some(), count, assignments, error }
}

fn active_agent_assignments(
    active_count: usize,
    workflows: &[WorkflowActivitySummary],
    task_titles: &HashMap<String, String>,
    silence: &SilenceContext,
) -> Vec<ActiveAgentAssignment> {
    let mut running: Vec<&WorkflowActivitySummary> = workflows.iter().collect();
    running
        .sort_by(|left, right| left.workflow_id.cmp(&right.workflow_id).then_with(|| left.task_id.cmp(&right.task_id)));

    let attributed_count = active_count.min(running.len());
    let mut assignments: Vec<ActiveAgentAssignment> = running
        .into_iter()
        .take(attributed_count)
        .map(|workflow| {
            let (last_output_at, silent_for_secs, silent) = silence.for_workflow(&workflow.workflow_id);
            ActiveAgentAssignment {
                task_id: workflow.task_id.clone(),
                task_title: task_titles
                    .get(workflow.task_id.as_str())
                    .cloned()
                    .unwrap_or_else(|| "Unknown task".to_string()),
                workflow_id: workflow.workflow_id.clone(),
                phase_id: workflow.phase_id.clone(),
                attributed: true,
                last_output_at,
                silent_for_secs,
                silent,
            }
        })
        .collect();

    let missing = active_count.saturating_sub(assignments.len());
    for placeholder_index in 0..missing {
        assignments.push(ActiveAgentAssignment {
            task_id: "unknown".to_string(),
            task_title: "Unknown task assignment".to_string(),
            workflow_id: format!("unknown-{}", placeholder_index + 1),
            phase_id: "unknown".to_string(),
            attributed: false,
            last_output_at: None,
            silent_for_secs: None,
            silent: false,
        });
    }

    assignments
}

/// Aggregate the router-sourced subject list into the Task Summary buckets.
/// When the router is unreachable (`subjects == None`) the section renders as
/// unavailable carrying the error string — never zeros reported as truth.
fn build_task_summary_slice_from_router(subjects: Option<&[Value]>, error: Option<String>) -> TaskSummarySlice {
    let Some(subjects) = subjects else {
        return TaskSummarySlice { available: false, total: 0, done: 0, in_progress: 0, ready: 0, blocked: 0, error };
    };
    let mut done = 0;
    let mut in_progress = 0;
    let mut ready = 0;
    let mut blocked = 0;
    for subject in subjects {
        let status = subject_status(subject);
        match status.as_deref() {
            Some("done" | "completed") => done += 1,
            Some("in-progress" | "inprogress" | "active") => in_progress += 1,
            Some("ready") => ready += 1,
            // `on-hold`/`on_hold` is a blocked state (`TaskStatus::is_blocked`),
            // so it counts in the blocked bucket alongside `blocked`.
            Some("blocked" | "on-hold" | "onhold") => blocked += 1,
            _ => {}
        }
        if !status_is_blocked(status.as_deref()) && subject_is_blocked_or_paused(subject) {
            blocked += 1;
        }
    }
    TaskSummarySlice { available: true, total: subjects.len(), done, in_progress, ready, blocked, error }
}

/// Build the Warnings slice from the daemon's degraded reasons and the
/// computed active-agent assignments.
fn build_warnings_slice(daemon_health: Option<&DaemonHealth>, agents: &ActiveAgentsSlice) -> WarningsSlice {
    let degraded_reasons = daemon_health.map(|health| health.degraded_reasons.clone()).unwrap_or_default();
    let silent_agents = agents.assignments.iter().filter(|assignment| assignment.silent).count();
    WarningsSlice { degraded: !degraded_reasons.is_empty() || silent_agents > 0, degraded_reasons, silent_agents }
}

fn subject_is_paused(subject: &Value) -> bool {
    subject.get("paused").and_then(Value::as_bool).unwrap_or(false)
}

/// `blocked` and `on-hold`/`onhold` are the blocked task statuses
/// (`TaskStatus::is_blocked`). `normalize_status` already folds `on_hold` to
/// `on-hold`, so only the two dash/no-dash spellings need matching here.
fn status_is_blocked(status: Option<&str>) -> bool {
    matches!(status, Some("blocked" | "on-hold" | "onhold"))
}

/// A subject belongs in the Blocked / Paused section when it is in a blocked
/// status, has its `paused` flag set, or carries a non-empty pause/block
/// annotation (`blocked_reason` / `blocked_by`). The workflow-pause path stamps
/// those annotations while intentionally leaving `status` and `paused`
/// untouched, so a status/paused-only predicate would silently drop them.
fn subject_is_blocked_or_paused(subject: &Value) -> bool {
    status_is_blocked(subject_status(subject).as_deref())
        || subject_is_paused(subject)
        || subject_str(subject, "blocked_reason").is_some()
        || subject_str(subject, "blocked_by").is_some()
}

/// A subject is "blocked-or-paused" when its status is blocked/on-hold, its
/// `paused` flag is set, or it carries a pause/block annotation. Such subjects
/// are surfaced in the Blocked / Paused section with their reason and age.
fn build_blocked_subjects_slice(subjects: Option<&[Value]>, error: Option<String>) -> BlockedSubjectsSlice {
    let Some(subjects) = subjects else {
        return BlockedSubjectsSlice { available: false, count: 0, entries: Vec::new(), error };
    };
    let mut entries = Vec::new();
    for subject in subjects {
        if !subject_is_blocked_or_paused(subject) {
            continue;
        }
        let is_blocked = status_is_blocked(subject_status(subject).as_deref());
        let age = age_from_timestamp(
            subject_str(subject, "blocked_at")
                .or_else(|| subject_str(subject, "status_changed_at"))
                .or_else(|| subject_str(subject, "updated_at"))
                .as_deref(),
        );
        entries.push(BlockedSubjectEntry {
            id: subject_id(subject),
            state: if is_blocked { "blocked" } else { "paused" },
            blocked_reason: subject_str(subject, "blocked_reason"),
            blocked_by: subject_str(subject, "blocked_by"),
            age,
        });
    }
    entries.sort_by(|left, right| left.id.cmp(&right.id));
    BlockedSubjectsSlice { available: true, count: entries.len(), entries, error }
}

/// Pending agent interactions (questions + tool approvals) needing a human
/// decision, read from the scoped runtime root the same way
/// `animus agent interactions list` reads them.
fn build_needs_you_slice(project_root: &str) -> NeedsYouSlice {
    match animus_runtime_shared::list_interactions(project_root, false, None) {
        Ok(records) => {
            let entries: Vec<NeedsYouEntry> = records
                .iter()
                .map(|record| {
                    let kind = match record.kind {
                        animus_runtime_shared::InteractionKind::Question => "question",
                        animus_runtime_shared::InteractionKind::Approval => "approval",
                    };
                    let age = age_from_timestamp(Some(record.created_at.as_str()));
                    let timeout_remaining = timeout_remaining(record);
                    NeedsYouEntry {
                        id: record.id.clone(),
                        kind,
                        agent: record.agent_id.clone(),
                        summary: crate::services::runtime::runtime_agent::interactions::interaction_summary(record),
                        age,
                        timeout_remaining,
                        answer_command:
                            crate::services::runtime::runtime_agent::interactions::interaction_answer_command(record),
                    }
                })
                .collect();
            NeedsYouSlice { available: true, count: entries.len(), entries, error: None }
        }
        Err(error) => NeedsYouSlice { available: false, count: 0, entries: Vec::new(), error: Some(error.to_string()) },
    }
}

/// Remaining time before a pending interaction's `timeout_secs` lapses,
/// measured from `created_at`. `None` when the record has no timeout or the
/// created-at timestamp is unparseable.
fn timeout_remaining(record: &animus_runtime_shared::InteractionRecord) -> Option<String> {
    let timeout = record.timeout_secs?;
    let created = DateTime::parse_from_rfc3339(record.created_at.trim()).ok()?.with_timezone(&Utc);
    let elapsed = (Utc::now() - created).num_seconds();
    if elapsed < 0 {
        return None;
    }
    let remaining = i64::try_from(timeout).unwrap_or(i64::MAX).saturating_sub(elapsed);
    if remaining <= 0 {
        return Some("expired".to_string());
    }
    Some(if remaining >= 3_600 {
        format!("{}h", remaining / 3_600)
    } else if remaining >= 60 {
        format!("{}m", remaining / 60)
    } else {
        format!("{remaining}s")
    })
}

fn build_recent_completions_entries_slice(
    entries: Option<&[RecentCompletionEntry]>,
    error: Option<String>,
) -> RecentCompletionsSlice {
    RecentCompletionsSlice {
        available: entries.is_some(),
        entries: entries.map(|entries| entries.to_vec()).unwrap_or_default(),
        error,
    }
}

#[cfg(test)]
fn recent_completions(tasks: &[OrchestratorTask]) -> Vec<RecentCompletionEntry> {
    let mut entries: Vec<RecentCompletionEntry> = tasks
        .iter()
        .filter(|task| task.status == TaskStatus::Done)
        .filter_map(|task| {
            task.metadata.completed_at.as_ref().map(|completed_at| RecentCompletionEntry {
                task_id: task.id.clone(),
                title: task.title.clone(),
                completed_at: completed_at.with_timezone(&Utc),
            })
        })
        .collect();
    entries.sort_by(|left, right| {
        right.completed_at.cmp(&left.completed_at).then_with(|| left.task_id.cmp(&right.task_id))
    });
    entries.truncate(RECENT_COMPLETIONS_LIMIT);
    entries
}

fn build_recent_failures_slice(failures: Option<&[RecentFailureEntry]>, error: Option<String>) -> RecentFailuresSlice {
    RecentFailuresSlice {
        available: failures.is_some(),
        entries: failures.map(|entries| entries.to_vec()).unwrap_or_default(),
        error,
    }
}

async fn collect_workflow_status_snapshot(project_root: &str, failures: usize) -> Result<WorkflowStatusSnapshot> {
    let project_root = project_root.to_string();
    tokio::task::spawn_blocking(move || load_workflow_status_snapshot(project_root.as_str(), failures))
        .await
        .map_err(|error| anyhow!("failed to collect workflow status snapshot: {error}"))?
}

async fn collect_recent_completions(project_root: &str) -> Result<Vec<RecentCompletionEntry>> {
    let project_root = project_root.to_string();
    tokio::task::spawn_blocking(move || load_recent_completions(project_root.as_str(), RECENT_COMPLETIONS_LIMIT))
        .await
        .map_err(|error| anyhow!("failed to collect recent completions: {error}"))?
}

fn load_recent_completions(project_root: &str, limit: usize) -> Result<Vec<RecentCompletionEntry>> {
    let conn = open_project_db(Path::new(project_root))?;
    let limit = i64::try_from(limit).context("recent completions limit overflow")?;
    let mut stmt = conn.prepare(
        "SELECT id, title, completed_at
         FROM tasks
         WHERE status = 'done'
           AND completed_at IS NOT NULL
         ORDER BY completed_at DESC, id ASC
         LIMIT ?1",
    )?;
    let rows = stmt.query_map([limit], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?, row.get::<_, String>(2)?))
    })?;

    let mut entries = Vec::new();
    for row in rows {
        let (task_id, title, completed_at) = row?;
        let completed_at = DateTime::parse_from_rfc3339(&completed_at)
            .with_context(|| format!("invalid task completed_at timestamp for {task_id}"))?
            .with_timezone(&Utc);
        entries.push(RecentCompletionEntry {
            task_id,
            title: title.unwrap_or_else(|| "Unknown task".to_string()),
            completed_at,
        });
    }
    Ok(entries)
}

fn load_task_titles(project_root: &str, task_ids: &[String]) -> Result<HashMap<String, String>> {
    if task_ids.is_empty() {
        return Ok(HashMap::new());
    }

    let conn = open_project_db(Path::new(project_root))?;
    let mut stmt = conn.prepare("SELECT title FROM tasks WHERE id = ?1")?;
    let mut titles = HashMap::new();
    for task_id in task_ids {
        let title = stmt.query_row([task_id], |row| row.get::<_, Option<String>>(0));
        if let Ok(Some(title)) = title {
            titles.insert(task_id.clone(), title);
        }
    }
    Ok(titles)
}

fn load_workflow_status_snapshot(project_root: &str, failures_limit: usize) -> Result<WorkflowStatusSnapshot> {
    Ok(WorkflowStatusSnapshot {
        active_workflows: load_active_workflow_summaries(Path::new(project_root))?,
        recent_failures: load_recent_failed_workflow_summaries(Path::new(project_root), failures_limit)?
            .into_iter()
            .map(|entry| RecentFailureEntry {
                workflow_id: entry.workflow_id,
                task_id: entry.task_id,
                phase_id: entry.phase_id,
                failed_at: entry.failed_at,
                failure_reason: entry.failure_reason,
            })
            .collect(),
    })
}

async fn collect_ci_status(project_root: &str) -> CiStatusSlice {
    let project_root = project_root.to_string();
    match tokio::task::spawn_blocking(move || collect_ci_status_blocking(project_root.as_str())).await {
        Ok(status) => status,
        Err(error) => CiStatusSlice {
            provider: CI_PROVIDER_GITHUB,
            available: false,
            last_run: None,
            reason: None,
            error: Some(format!("failed to collect CI status: {error}")),
            cached: false,
        },
    }
}

fn collect_ci_status_blocking(project_root: &str) -> CiStatusSlice {
    if cache_enabled() {
        if let Some(cached) = read_ci_cache(project_root) {
            return cached;
        }
    }
    let fresh = ci_status_from_lookup(lookup_ci_status(project_root));
    if cache_enabled() && fresh.error.is_none() {
        let _ = write_ci_cache(project_root, &fresh);
    }
    fresh
}

fn cache_enabled() -> bool {
    if no_cache_runtime_flag() {
        return false;
    }
    match std::env::var("ANIMUS_DISABLE_CI_CACHE") {
        Ok(value) => !is_truthy(&value),
        Err(_) => true,
    }
}

fn is_truthy(value: &str) -> bool {
    matches!(value.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on")
}

fn ci_cache_ttl_secs() -> u64 {
    std::env::var("ANIMUS_CI_CACHE_TTL_SECS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(CI_CACHE_DEFAULT_TTL_SECS)
}

/// Per-project CI cache path. Each repo gets its own file under
/// `~/.animus/<repo-scope>/cache/ci-status.json` so two different
/// projects polled within the same TTL window never serve each other's
/// `gh run list` output. Falls back to a global path when scope
/// resolution fails (rare; only when home_dir is unset).
fn ci_cache_path(project_root: &str) -> Option<PathBuf> {
    let path = Path::new(project_root);
    let base = protocol::scoped_state_root(path)
        .unwrap_or_else(|| protocol::Config::global_config_dir().join("cache-fallback"));
    Some(base.join("cache").join("ci-status.json"))
}

fn read_ci_cache(project_root: &str) -> Option<CiStatusSlice> {
    let path = ci_cache_path(project_root)?;
    let bytes = std::fs::read(&path).ok()?;
    let file: CiCacheFile = serde_json::from_slice(&bytes).ok()?;
    if file.schema != CI_CACHE_SCHEMA {
        return None;
    }
    // Always honor the *current* TTL on read. The serialized
    // `ttl_seconds` is informational only; using it would let one stale
    // `ANIMUS_CI_CACHE_TTL_SECS=3600` invocation pin every subsequent
    // call to an hour-long cache even after the user lowers the override.
    let ttl = ci_cache_ttl_secs();
    let age = (Utc::now() - file.fetched_at).num_seconds();
    if age < 0 || age as u64 >= ttl {
        return None;
    }
    let mut payload = file.payload;
    payload.cached = true;
    Some(payload)
}

fn write_ci_cache(project_root: &str, payload: &CiStatusSlice) -> Result<()> {
    let path = match ci_cache_path(project_root) {
        Some(p) => p,
        None => return Ok(()),
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("failed to create cache dir {}", parent.display()))?;
    }
    let mut fresh = payload.clone();
    fresh.cached = false;
    let file = CiCacheFile {
        schema: CI_CACHE_SCHEMA.to_string(),
        fetched_at: Utc::now(),
        ttl_seconds: ci_cache_ttl_secs(),
        payload: fresh,
    };
    let bytes = serde_json::to_vec_pretty(&file).context("failed to serialize ci cache")?;
    write_atomic(&path, &bytes)
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().ok_or_else(|| anyhow!("cache path has no parent: {}", path.display()))?;
    std::fs::create_dir_all(parent).with_context(|| format!("failed to create dir {}", parent.display()))?;
    let pid = std::process::id();
    let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let tmp = parent.join(format!(
        ".{}.tmp.{}.{}",
        path.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "cache".to_string()),
        pid,
        nonce
    ));
    std::fs::write(&tmp, bytes).with_context(|| format!("failed to write {}", tmp.display()))?;
    if let Err(err) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(anyhow!("failed to rename {} -> {}: {}", tmp.display(), path.display(), err));
    }
    Ok(())
}

/// Process-global "skip all caches" flag. Set by `--no-cache` at CLI
/// parse time so library code does not have to thread the flag through
/// every call site.
static NO_CACHE_FLAG: OnceLock<bool> = OnceLock::new();

pub fn set_no_cache_flag(value: bool) {
    let _ = NO_CACHE_FLAG.set(value);
}

fn no_cache_runtime_flag() -> bool {
    *NO_CACHE_FLAG.get().unwrap_or(&false)
}

fn lookup_ci_status(project_root: &str) -> CiLookupOutcome {
    if !gh_available() {
        return CiLookupOutcome::Unavailable("gh CLI is not installed".to_string());
    }

    match query_latest_gh_run(project_root) {
        Ok(run) => CiLookupOutcome::Success(run),
        Err(error) => CiLookupOutcome::Failure(error.to_string()),
    }
}

fn ci_status_from_lookup(outcome: CiLookupOutcome) -> CiStatusSlice {
    match outcome {
        CiLookupOutcome::Unavailable(reason) => CiStatusSlice {
            provider: CI_PROVIDER_GITHUB,
            available: false,
            last_run: None,
            reason: Some(reason),
            error: None,
            cached: false,
        },
        CiLookupOutcome::Success(run) => CiStatusSlice {
            provider: CI_PROVIDER_GITHUB,
            available: true,
            reason: if run.is_none() { Some("no workflow runs found".to_string()) } else { None },
            last_run: run,
            error: None,
            cached: false,
        },
        CiLookupOutcome::Failure(error) => CiStatusSlice {
            provider: CI_PROVIDER_GITHUB,
            available: true,
            last_run: None,
            reason: None,
            error: Some(error),
            cached: false,
        },
    }
}

fn gh_available() -> bool {
    // Memoize per-process: `gh --version` spawns a subprocess (~50-100ms on
    // first call). Subsequent `animus status` invocations from the same
    // process (Tauri loops, MCP servers, dashboard refreshes) reuse the
    // cached answer.
    static AVAILABLE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *AVAILABLE.get_or_init(|| {
        ProcessCommand::new("gh")
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    })
}

fn query_latest_gh_run(project_root: &str) -> Result<Option<CiRunSummary>> {
    let output = ProcessCommand::new("gh")
        .current_dir(project_root)
        .args(["run", "list", "--limit", "1", "--json", GH_RUN_LIST_FIELDS])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .with_context(|| format!("failed to run gh run list in {project_root}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let message =
            if stderr.is_empty() { format!("gh run list exited with status {}", output.status) } else { stderr };
        return Err(anyhow!(message));
    }

    let payload = String::from_utf8(output.stdout).context("gh run list emitted non-UTF8 output")?;
    parse_gh_run_list(payload.as_str())
}

fn parse_gh_run_list(payload: &str) -> Result<Option<CiRunSummary>> {
    let entries: Vec<GhRunListEntry> =
        serde_json::from_str(payload).context("failed to parse gh run list JSON payload")?;
    let Some(entry) = entries.into_iter().next() else {
        return Ok(None);
    };
    Ok(Some(CiRunSummary {
        id: entry.database_id,
        title: entry.display_title,
        name: entry.name,
        workflow_name: entry.workflow_name,
        status: entry.status.unwrap_or_else(|| "unknown".to_string()),
        conclusion: entry.conclusion,
        event: entry.event,
        head_branch: entry.head_branch,
        head_sha: entry.head_sha,
        created_at: entry.created_at,
        updated_at: entry.updated_at,
        url: entry.url,
    }))
}

/// Compact `Nh Nm Ns` rendering of a duration in seconds, dropping
/// zero-valued leading units (e.g. `90` -> `1m 30s`, `3661` -> `1h 1m 1s`).
fn format_duration_secs(secs: u64) -> String {
    let hours = secs / 3600;
    let minutes = (secs % 3600) / 60;
    let seconds = secs % 60;
    if hours > 0 {
        format!("{hours}h {minutes}m {seconds}s")
    } else if minutes > 0 {
        format!("{minutes}m {seconds}s")
    } else {
        format!("{seconds}s")
    }
}

fn render_status_dashboard(dashboard: &StatusDashboard) -> String {
    let mut output = String::new();
    let _ = writeln!(&mut output, "Animus Status Dashboard");
    let _ = writeln!(&mut output, "Project Root: {}", dashboard.project_root);
    let _ = writeln!(&mut output, "Generated At: {}", dashboard.generated_at.to_rfc3339());
    let _ = writeln!(&mut output);

    let _ = writeln!(&mut output, "Daemon");
    let _ = writeln!(&mut output, "  status: {}", dashboard.daemon.status);
    let _ = writeln!(&mut output, "  running: {}", dashboard.daemon.running);
    let _ = writeln!(&mut output, "  runtime_paused: {}", dashboard.daemon.runtime_paused);
    if let Some(paused_at) = dashboard.daemon.paused_at.as_deref() {
        let _ = writeln!(&mut output, "  paused_at: {paused_at}");
    }
    let _ = writeln!(&mut output, "  provider_plugins_healthy: {}", dashboard.daemon.provider_plugins_healthy);
    if let Some(error) = dashboard.daemon.error.as_deref() {
        let _ = writeln!(&mut output, "  error: {error}");
    }
    let _ = writeln!(&mut output);

    let _ = writeln!(&mut output, "Warnings");
    if !dashboard.warnings.degraded {
        let _ = writeln!(&mut output, "  none");
    } else {
        let _ = writeln!(&mut output, "  degraded: true");
        for reason in &dashboard.warnings.degraded_reasons {
            let _ = writeln!(&mut output, "  - {reason}");
        }
        if dashboard.warnings.silent_agents > 0 {
            let _ = writeln!(
                &mut output,
                "  - {} agent(s) SILENT (no output past the configured threshold)",
                dashboard.warnings.silent_agents
            );
        }
    }
    let _ = writeln!(&mut output);

    let _ = writeln!(&mut output, "Active Agents");
    let _ = writeln!(&mut output, "  count: {}", dashboard.active_agents.count);
    if dashboard.active_agents.assignments.is_empty() {
        let _ = writeln!(&mut output, "  entries: none");
    } else {
        for entry in &dashboard.active_agents.assignments {
            let silence = match (entry.silent, entry.silent_for_secs) {
                (true, Some(secs)) => format!(" SILENT silent_for={}", format_duration_secs(secs)),
                (false, Some(secs)) => format!(" silent_for={}", format_duration_secs(secs)),
                _ => String::new(),
            };
            let _ = writeln!(
                &mut output,
                "  - task_id={} task_title={} workflow_id={} phase_id={} attributed={}{}",
                entry.task_id, entry.task_title, entry.workflow_id, entry.phase_id, entry.attributed, silence
            );
        }
    }
    if let Some(error) = dashboard.active_agents.error.as_deref() {
        let _ = writeln!(&mut output, "  error: {error}");
    }
    let _ = writeln!(&mut output);

    let _ = writeln!(&mut output, "Task Summary");
    let _ = writeln!(&mut output, "  total: {}", dashboard.task_summary.total);
    let _ = writeln!(&mut output, "  done: {}", dashboard.task_summary.done);
    let _ = writeln!(&mut output, "  in_progress: {}", dashboard.task_summary.in_progress);
    let _ = writeln!(&mut output, "  ready: {}", dashboard.task_summary.ready);
    let _ = writeln!(&mut output, "  blocked: {}", dashboard.task_summary.blocked);
    if let Some(error) = dashboard.task_summary.error.as_deref() {
        let _ = writeln!(&mut output, "  error: {error}");
    }
    if !dashboard.task_summary.available && dashboard.task_summary.error.is_none() {
        let _ = writeln!(&mut output, "  (unavailable)");
    }
    let _ = writeln!(&mut output);

    let _ = writeln!(&mut output, "Blocked / Paused");
    let blocked = &dashboard.blocked_subjects;
    if !blocked.available {
        let _ = writeln!(&mut output, "  (unavailable)");
    } else if blocked.entries.is_empty() {
        let _ = writeln!(&mut output, "  entries: none");
    } else {
        for entry in &blocked.entries {
            let age = entry.age.as_deref().map(|age| format!(" {age}")).unwrap_or_default();
            let reason = entry.blocked_reason.as_deref().map(|r| format!("  reason: {r}")).unwrap_or_default();
            let by = entry.blocked_by.as_deref().map(|b| format!("  by: {b}")).unwrap_or_default();
            let _ = writeln!(&mut output, "  - {}  {}{}{}{}", entry.id, entry.state, age, reason, by);
        }
    }
    if let Some(error) = blocked.error.as_deref() {
        let _ = writeln!(&mut output, "  error: {error}");
    }
    let _ = writeln!(&mut output);

    let _ = writeln!(&mut output, "Needs You");
    let needs_you = &dashboard.needs_you;
    if !needs_you.available {
        let _ = writeln!(&mut output, "  (unavailable)");
    } else if needs_you.entries.is_empty() {
        let _ = writeln!(&mut output, "  entries: none");
    } else {
        let _ = writeln!(&mut output, "  count: {}", needs_you.count);
        for entry in &needs_you.entries {
            let age = entry.age.as_deref().map(|age| format!(" ({age} ago)")).unwrap_or_default();
            let timeout = entry.timeout_remaining.as_deref().map(|t| format!(" [timeout: {t}]")).unwrap_or_default();
            let _ = writeln!(&mut output, "  - {} {}: {}{}{}", entry.id, entry.kind, entry.summary, age, timeout);
            let _ = writeln!(&mut output, "    {}", entry.answer_command);
        }
    }
    if let Some(error) = needs_you.error.as_deref() {
        let _ = writeln!(&mut output, "  error: {error}");
    }
    let _ = writeln!(&mut output);

    let _ = writeln!(&mut output, "Recent Completions");
    if dashboard.recent_completions.entries.is_empty() {
        let _ = writeln!(&mut output, "  entries: none");
    } else {
        for entry in &dashboard.recent_completions.entries {
            let _ = writeln!(
                &mut output,
                "  - task_id={} completed_at={} title={}",
                entry.task_id,
                entry.completed_at.to_rfc3339(),
                entry.title
            );
        }
    }
    if let Some(error) = dashboard.recent_completions.error.as_deref() {
        let _ = writeln!(&mut output, "  error: {error}");
    }
    let _ = writeln!(&mut output);

    let _ = writeln!(&mut output, "Recent Failures");
    if dashboard.recent_failures.entries.is_empty() {
        let _ = writeln!(&mut output, "  entries: none");
    } else {
        for entry in &dashboard.recent_failures.entries {
            let _ = writeln!(
                &mut output,
                "  - workflow_id={} task_id={} phase_id={} failed_at={} failure_reason={}",
                entry.workflow_id,
                entry.task_id,
                entry.phase_id,
                entry.failed_at.to_rfc3339(),
                entry.failure_reason.as_deref().unwrap_or("n/a")
            );
        }
    }
    if let Some(error) = dashboard.recent_failures.error.as_deref() {
        let _ = writeln!(&mut output, "  error: {error}");
    }
    let _ = writeln!(&mut output);

    let _ = writeln!(&mut output, "CI Status");
    let _ = writeln!(&mut output, "  provider: {}", dashboard.ci.provider);
    let _ = writeln!(&mut output, "  available: {}", dashboard.ci.available);
    if dashboard.ci.cached {
        let _ = writeln!(&mut output, "  cached: true");
    }
    if let Some(run) = dashboard.ci.last_run.as_ref() {
        let _ = writeln!(
            &mut output,
            "  last_run: id={} workflow_name={} status={} conclusion={} url={}",
            run.id.map(|id| id.to_string()).unwrap_or_else(|| "n/a".to_string()),
            run.workflow_name.as_deref().unwrap_or("n/a"),
            run.status,
            run.conclusion.as_deref().unwrap_or("n/a"),
            run.url.as_deref().unwrap_or("n/a")
        );
    } else {
        let _ = writeln!(&mut output, "  last_run: none");
    }
    if let Some(reason) = dashboard.ci.reason.as_deref() {
        let _ = writeln!(&mut output, "  reason: {reason}");
    }
    if let Some(error) = dashboard.ci.error.as_deref() {
        let _ = writeln!(&mut output, "  error: {error}");
    }
    let _ = writeln!(&mut output);

    let budget = &dashboard.budget;
    let _ = writeln!(&mut output, "Budget");
    let _ = writeln!(&mut output, "  enforcement_enabled: {}", budget.enforcement_enabled);
    if let Some(last) = budget.last_sweep_at.as_deref() {
        let _ = writeln!(&mut output, "  last_sweep_at: {last}");
    }
    match budget.breaches.active {
        Some(active) => {
            let _ = writeln!(&mut output, "  active_breaches: {active}");
        }
        None => {
            let _ = writeln!(&mut output, "  breaches_last_24h: {}", budget.breaches.recent_24h);
        }
    }
    if let Some(offender) = budget.breaches.worst_offender.as_ref() {
        let _ = writeln!(&mut output, "  worst_offender: {} — {}", offender.workflow_run_id, offender.summary);
        let _ = writeln!(&mut output, "  (see `animus cost decisions`)");
    }
    if let Some(error) = budget.error.as_deref() {
        let _ = writeln!(&mut output, "  error: {error}");
    }

    output
}

#[cfg(test)]
mod tests;
