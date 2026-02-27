use std::collections::HashMap;
use std::fmt::Write as _;
use std::process::{Command as ProcessCommand, Stdio};
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Utc};
use orchestrator_core::{
    DaemonHealth, DaemonStatus, OrchestratorTask, OrchestratorWorkflow, ServiceHub, TaskStatistics,
    TaskStatus, WorkflowPhaseStatus, WorkflowStatus,
};
use serde::{Deserialize, Serialize};

use crate::print_value;

const STATUS_SCHEMA: &str = "ao.status.v1";
const RECENT_COMPLETIONS_LIMIT: usize = 5;
const RECENT_FAILURES_LIMIT: usize = 3;
const CI_PROVIDER_GITHUB: &str = "github";
const GH_RUN_LIST_FIELDS: &str =
    "databaseId,displayTitle,name,workflowName,status,conclusion,event,headBranch,headSha,createdAt,updatedAt,url";

#[derive(Debug, Clone, Serialize)]
struct StatusDashboard {
    schema: &'static str,
    project_root: String,
    generated_at: DateTime<Utc>,
    daemon: DaemonStatusSlice,
    active_agents: ActiveAgentsSlice,
    task_summary: TaskSummarySlice,
    recent_completions: RecentCompletionsSlice,
    recent_failures: RecentFailuresSlice,
    ci: CiStatusSlice,
}

#[derive(Debug, Clone, Serialize)]
struct DaemonStatusSlice {
    available: bool,
    status: String,
    running: bool,
    runner_connected: bool,
    runner_pid: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    error: Option<String>,
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

#[derive(Debug, Clone, Serialize)]
struct CiStatusSlice {
    provider: &'static str,
    available: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_run: Option<CiRunSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
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

pub(crate) async fn handle_status(
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
    json: bool,
) -> Result<()> {
    let daemon_service = hub.daemon();
    let tasks_service = hub.tasks();
    let task_stats_service = tasks_service.clone();
    let workflows_service = hub.workflows();

    let (daemon_result, tasks_result, task_stats_result, workflows_result, ci_slice) = tokio::join!(
        daemon_service.health(),
        tasks_service.list(),
        task_stats_service.statistics(),
        workflows_service.list(),
        collect_ci_status(project_root),
    );

    let (daemon_health, daemon_error) = split_result(daemon_result);
    let (tasks, tasks_error) = split_result(tasks_result);
    let (task_stats, task_stats_error) = split_result(task_stats_result);
    let (workflows, workflows_error) = split_result(workflows_result);

    let dashboard = StatusDashboard {
        schema: STATUS_SCHEMA,
        project_root: project_root.to_string(),
        generated_at: Utc::now(),
        daemon: build_daemon_slice(daemon_health.as_ref(), daemon_error.clone()),
        active_agents: build_active_agents_slice(
            daemon_health.as_ref(),
            workflows.as_deref(),
            tasks.as_deref(),
            combine_errors([
                daemon_error.as_deref(),
                workflows_error.as_deref(),
                tasks_error.as_deref(),
            ]),
        ),
        task_summary: build_task_summary_slice(
            task_stats.as_ref(),
            tasks.as_deref(),
            combine_errors([task_stats_error.as_deref(), tasks_error.as_deref()]),
        ),
        recent_completions: build_recent_completions_slice(tasks.as_deref(), tasks_error),
        recent_failures: build_recent_failures_slice(workflows.as_deref(), workflows_error),
        ci: ci_slice,
    };

    if json {
        return print_value(dashboard, true);
    }

    println!("{}", render_status_dashboard(&dashboard));
    Ok(())
}

fn split_result<T>(result: Result<T>) -> (Option<T>, Option<String>) {
    match result {
        Ok(value) => (Some(value), None),
        Err(error) => (None, Some(error.to_string())),
    }
}

fn combine_errors<'a>(errors: impl IntoIterator<Item = Option<&'a str>>) -> Option<String> {
    let messages: Vec<&str> = errors
        .into_iter()
        .flatten()
        .map(str::trim)
        .filter(|message| !message.is_empty())
        .collect();
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
            runner_connected: health.runner_connected,
            runner_pid: health.runner_pid,
            error,
        },
        None => DaemonStatusSlice {
            available: false,
            status: "unknown".to_string(),
            running: false,
            runner_connected: false,
            runner_pid: None,
            error,
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

fn build_active_agents_slice(
    daemon_health: Option<&DaemonHealth>,
    workflows: Option<&[OrchestratorWorkflow]>,
    tasks: Option<&[OrchestratorTask]>,
    error: Option<String>,
) -> ActiveAgentsSlice {
    let count = daemon_health
        .map(|health| health.active_agents)
        .unwrap_or(0);
    let assignments = active_agent_assignments(
        count,
        workflows.unwrap_or_default(),
        tasks.unwrap_or_default(),
    );
    ActiveAgentsSlice {
        available: daemon_health.is_some(),
        count,
        assignments,
        error,
    }
}

fn active_agent_assignments(
    active_count: usize,
    workflows: &[OrchestratorWorkflow],
    tasks: &[OrchestratorTask],
) -> Vec<ActiveAgentAssignment> {
    let task_titles: HashMap<&str, &str> = tasks
        .iter()
        .map(|task| (task.id.as_str(), task.title.as_str()))
        .collect();

    let mut running: Vec<&OrchestratorWorkflow> = workflows
        .iter()
        .filter(|workflow| workflow.status == WorkflowStatus::Running)
        .collect();
    running.sort_by(|left, right| {
        left.id
            .cmp(&right.id)
            .then_with(|| left.task_id.cmp(&right.task_id))
    });

    let attributed_count = active_count.min(running.len());
    let mut assignments: Vec<ActiveAgentAssignment> = running
        .into_iter()
        .take(attributed_count)
        .map(|workflow| ActiveAgentAssignment {
            task_id: workflow.task_id.clone(),
            task_title: task_titles
                .get(workflow.task_id.as_str())
                .copied()
                .unwrap_or("Unknown task")
                .to_string(),
            workflow_id: workflow.id.clone(),
            phase_id: workflow_active_phase(workflow),
            attributed: true,
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
        });
    }

    assignments
}

fn workflow_active_phase(workflow: &OrchestratorWorkflow) -> String {
    workflow
        .phases
        .iter()
        .find(|phase| phase.status == WorkflowPhaseStatus::Running)
        .map(|phase| phase.phase_id.clone())
        .or_else(|| workflow.current_phase.clone())
        .unwrap_or_else(|| "unknown".to_string())
}

fn build_task_summary_slice(
    statistics: Option<&TaskStatistics>,
    tasks: Option<&[OrchestratorTask]>,
    error: Option<String>,
) -> TaskSummarySlice {
    if let Some(statistics) = statistics {
        return TaskSummarySlice {
            available: true,
            total: statistics.total,
            done: statistics.by_status.get("done").copied().unwrap_or(0),
            in_progress: statistics.in_progress,
            ready: statistics.by_status.get("ready").copied().unwrap_or(0),
            blocked: statistics.blocked,
            error,
        };
    }

    if let Some(tasks) = tasks {
        return TaskSummarySlice {
            available: true,
            total: tasks.len(),
            done: tasks
                .iter()
                .filter(|task| task.status == TaskStatus::Done)
                .count(),
            in_progress: tasks
                .iter()
                .filter(|task| task.status == TaskStatus::InProgress)
                .count(),
            ready: tasks
                .iter()
                .filter(|task| task.status == TaskStatus::Ready)
                .count(),
            blocked: tasks.iter().filter(|task| task.status.is_blocked()).count(),
            error,
        };
    }

    TaskSummarySlice {
        available: false,
        total: 0,
        done: 0,
        in_progress: 0,
        ready: 0,
        blocked: 0,
        error,
    }
}

fn build_recent_completions_slice(
    tasks: Option<&[OrchestratorTask]>,
    error: Option<String>,
) -> RecentCompletionsSlice {
    RecentCompletionsSlice {
        available: tasks.is_some(),
        entries: tasks.map(recent_completions).unwrap_or_default(),
        error,
    }
}

fn recent_completions(tasks: &[OrchestratorTask]) -> Vec<RecentCompletionEntry> {
    let mut entries: Vec<RecentCompletionEntry> = tasks
        .iter()
        .filter(|task| task.status == TaskStatus::Done)
        .filter_map(|task| {
            task.metadata
                .completed_at
                .as_ref()
                .map(|completed_at| RecentCompletionEntry {
                    task_id: task.id.clone(),
                    title: task.title.clone(),
                    completed_at: completed_at.with_timezone(&Utc),
                })
        })
        .collect();
    entries.sort_by(|left, right| {
        right
            .completed_at
            .cmp(&left.completed_at)
            .then_with(|| left.task_id.cmp(&right.task_id))
    });
    entries.truncate(RECENT_COMPLETIONS_LIMIT);
    entries
}

fn build_recent_failures_slice(
    workflows: Option<&[OrchestratorWorkflow]>,
    error: Option<String>,
) -> RecentFailuresSlice {
    RecentFailuresSlice {
        available: workflows.is_some(),
        entries: workflows.map(recent_failures).unwrap_or_default(),
        error,
    }
}

fn recent_failures(workflows: &[OrchestratorWorkflow]) -> Vec<RecentFailureEntry> {
    let mut entries: Vec<RecentFailureEntry> = workflows
        .iter()
        .filter(|workflow| workflow.status == WorkflowStatus::Failed)
        .map(|workflow| {
            let (phase_id, failed_at, phase_error) = latest_failed_phase(workflow);
            RecentFailureEntry {
                workflow_id: workflow.id.clone(),
                task_id: workflow.task_id.clone(),
                phase_id,
                failed_at,
                failure_reason: workflow.failure_reason.clone().or(phase_error),
            }
        })
        .collect();
    entries.sort_by(|left, right| {
        right
            .failed_at
            .cmp(&left.failed_at)
            .then_with(|| left.workflow_id.cmp(&right.workflow_id))
    });
    entries.truncate(RECENT_FAILURES_LIMIT);
    entries
}

fn latest_failed_phase(workflow: &OrchestratorWorkflow) -> (String, DateTime<Utc>, Option<String>) {
    let failed_phase = workflow
        .phases
        .iter()
        .enumerate()
        .filter(|(_, phase)| phase.status == WorkflowPhaseStatus::Failed)
        .max_by(|left, right| {
            left.1
                .completed_at
                .cmp(&right.1.completed_at)
                .then_with(|| left.0.cmp(&right.0))
        })
        .map(|(_, phase)| phase);

    let phase_id = failed_phase
        .map(|phase| phase.phase_id.clone())
        .or_else(|| workflow.current_phase.clone())
        .unwrap_or_else(|| "unknown".to_string());
    let failed_at = failed_phase
        .and_then(|phase| phase.completed_at.as_ref().cloned())
        .or_else(|| workflow.completed_at.as_ref().cloned())
        .unwrap_or_else(|| workflow.started_at.with_timezone(&Utc));
    let phase_error = failed_phase.and_then(|phase| phase.error_message.clone());

    (phase_id, failed_at, phase_error)
}

async fn collect_ci_status(project_root: &str) -> CiStatusSlice {
    let project_root = project_root.to_string();
    match tokio::task::spawn_blocking(move || {
        ci_status_from_lookup(lookup_ci_status(project_root.as_str()))
    })
    .await
    {
        Ok(status) => status,
        Err(error) => CiStatusSlice {
            provider: CI_PROVIDER_GITHUB,
            available: false,
            last_run: None,
            reason: None,
            error: Some(format!("failed to collect CI status: {error}")),
        },
    }
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
        },
        CiLookupOutcome::Success(run) => CiStatusSlice {
            provider: CI_PROVIDER_GITHUB,
            available: true,
            reason: if run.is_none() {
                Some("no workflow runs found".to_string())
            } else {
                None
            },
            last_run: run,
            error: None,
        },
        CiLookupOutcome::Failure(error) => CiStatusSlice {
            provider: CI_PROVIDER_GITHUB,
            available: true,
            last_run: None,
            reason: None,
            error: Some(error),
        },
    }
}

fn gh_available() -> bool {
    ProcessCommand::new("gh")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
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
        let message = if stderr.is_empty() {
            format!("gh run list exited with status {}", output.status)
        } else {
            stderr
        };
        return Err(anyhow!(message));
    }

    let payload =
        String::from_utf8(output.stdout).context("gh run list emitted non-UTF8 output")?;
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

fn render_status_dashboard(dashboard: &StatusDashboard) -> String {
    let mut output = String::new();
    let _ = writeln!(&mut output, "AO Status Dashboard");
    let _ = writeln!(&mut output, "Project Root: {}", dashboard.project_root);
    let _ = writeln!(
        &mut output,
        "Generated At: {}",
        dashboard.generated_at.to_rfc3339()
    );
    let _ = writeln!(&mut output);

    let _ = writeln!(&mut output, "Daemon");
    let _ = writeln!(&mut output, "  status: {}", dashboard.daemon.status);
    let _ = writeln!(&mut output, "  running: {}", dashboard.daemon.running);
    let _ = writeln!(
        &mut output,
        "  runner_connected: {}",
        dashboard.daemon.runner_connected
    );
    let _ = writeln!(
        &mut output,
        "  runner_pid: {}",
        dashboard
            .daemon
            .runner_pid
            .map(|pid| pid.to_string())
            .unwrap_or_else(|| "n/a".to_string())
    );
    if let Some(error) = dashboard.daemon.error.as_deref() {
        let _ = writeln!(&mut output, "  error: {error}");
    }
    let _ = writeln!(&mut output);

    let _ = writeln!(&mut output, "Active Agents");
    let _ = writeln!(&mut output, "  count: {}", dashboard.active_agents.count);
    if dashboard.active_agents.assignments.is_empty() {
        let _ = writeln!(&mut output, "  entries: none");
    } else {
        for entry in &dashboard.active_agents.assignments {
            let _ = writeln!(
                &mut output,
                "  - task_id={} task_title={} workflow_id={} phase_id={} attributed={}",
                entry.task_id,
                entry.task_title,
                entry.workflow_id,
                entry.phase_id,
                entry.attributed
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
    let _ = writeln!(
        &mut output,
        "  in_progress: {}",
        dashboard.task_summary.in_progress
    );
    let _ = writeln!(&mut output, "  ready: {}", dashboard.task_summary.ready);
    let _ = writeln!(&mut output, "  blocked: {}", dashboard.task_summary.blocked);
    if let Some(error) = dashboard.task_summary.error.as_deref() {
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
    if let Some(run) = dashboard.ci.last_run.as_ref() {
        let _ = writeln!(
            &mut output,
            "  last_run: id={} workflow_name={} status={} conclusion={} url={}",
            run.id
                .map(|id| id.to_string())
                .unwrap_or_else(|| "n/a".to_string()),
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

    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use orchestrator_core::{
        Assignee, ChecklistItem, Complexity, ImpactArea, Priority, ResourceRequirements, RiskLevel,
        Scope, TaskDependency, TaskMetadata, TaskType, WorkflowCheckpointMetadata,
        WorkflowDecisionRecord, WorkflowMachineState, WorkflowMetadata, WorkflowPhaseExecution,
    };
    use std::collections::HashMap;

    fn parse_time(value: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(value)
            .expect("timestamp should be valid RFC3339")
            .with_timezone(&Utc)
    }

    fn make_task(
        id: &str,
        title: &str,
        status: TaskStatus,
        completed_at: Option<DateTime<Utc>>,
    ) -> OrchestratorTask {
        let now = parse_time("2026-02-01T00:00:00Z");
        OrchestratorTask {
            id: id.to_string(),
            title: title.to_string(),
            description: String::new(),
            task_type: TaskType::Feature,
            status,
            blocked_reason: None,
            blocked_at: None,
            blocked_phase: None,
            blocked_by: None,
            priority: Priority::Medium,
            risk: RiskLevel::Medium,
            scope: Scope::Medium,
            complexity: Complexity::Medium,
            impact_area: Vec::<ImpactArea>::new(),
            assignee: Assignee::Unassigned,
            estimated_effort: None,
            linked_requirements: Vec::new(),
            linked_architecture_entities: Vec::new(),
            dependencies: Vec::<TaskDependency>::new(),
            checklist: Vec::<ChecklistItem>::new(),
            tags: Vec::new(),
            workflow_metadata: WorkflowMetadata::default(),
            worktree_path: None,
            branch_name: None,
            metadata: TaskMetadata {
                created_at: now,
                updated_at: now,
                created_by: "test".to_string(),
                updated_by: "test".to_string(),
                started_at: None,
                completed_at,
                version: 1,
            },
            deadline: None,
            paused: false,
            cancelled: false,
            resource_requirements: ResourceRequirements::default(),
        }
    }

    fn make_phase(
        phase_id: &str,
        status: WorkflowPhaseStatus,
        completed_at: Option<DateTime<Utc>>,
        error_message: Option<&str>,
    ) -> WorkflowPhaseExecution {
        WorkflowPhaseExecution {
            phase_id: phase_id.to_string(),
            status,
            started_at: None,
            completed_at,
            attempt: 1,
            error_message: error_message.map(str::to_string),
        }
    }

    fn make_workflow(
        id: &str,
        task_id: &str,
        status: WorkflowStatus,
        current_phase: Option<&str>,
        started_at: DateTime<Utc>,
        completed_at: Option<DateTime<Utc>>,
        phases: Vec<WorkflowPhaseExecution>,
        failure_reason: Option<&str>,
    ) -> OrchestratorWorkflow {
        OrchestratorWorkflow {
            id: id.to_string(),
            task_id: task_id.to_string(),
            pipeline_id: None,
            status,
            current_phase_index: 0,
            phases,
            machine_state: WorkflowMachineState::Idle,
            current_phase: current_phase.map(str::to_string),
            started_at,
            completed_at,
            failure_reason: failure_reason.map(str::to_string),
            checkpoint_metadata: WorkflowCheckpointMetadata::default(),
            rework_counts: HashMap::<String, u32>::new(),
            total_reworks: 0,
            decision_history: Vec::<WorkflowDecisionRecord>::new(),
        }
    }

    #[test]
    fn recent_completions_are_sorted_and_limited() {
        let tasks = vec![
            make_task(
                "TASK-003",
                "third",
                TaskStatus::Done,
                Some(parse_time("2026-02-21T12:00:00Z")),
            ),
            make_task(
                "TASK-001",
                "first",
                TaskStatus::Done,
                Some(parse_time("2026-02-20T10:00:00Z")),
            ),
            make_task(
                "TASK-002",
                "second",
                TaskStatus::Done,
                Some(parse_time("2026-02-20T10:00:00Z")),
            ),
            make_task(
                "TASK-004",
                "fourth",
                TaskStatus::Done,
                Some(parse_time("2026-02-19T10:00:00Z")),
            ),
            make_task(
                "TASK-005",
                "fifth",
                TaskStatus::Done,
                Some(parse_time("2026-02-18T10:00:00Z")),
            ),
            make_task(
                "TASK-006",
                "sixth",
                TaskStatus::Done,
                Some(parse_time("2026-02-17T10:00:00Z")),
            ),
            make_task("TASK-007", "skip-no-completed-at", TaskStatus::Done, None),
            make_task(
                "TASK-008",
                "skip-cancelled",
                TaskStatus::Cancelled,
                Some(parse_time("2026-02-22T10:00:00Z")),
            ),
        ];

        let entries = recent_completions(&tasks);
        assert_eq!(entries.len(), 5, "entries should be capped at 5");
        let ids: Vec<&str> = entries.iter().map(|entry| entry.task_id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["TASK-003", "TASK-001", "TASK-002", "TASK-004", "TASK-005"]
        );
    }

    #[test]
    fn recent_failures_are_sorted_limited_and_fallback_current_phase() {
        let workflows = vec![
            make_workflow(
                "WF-002",
                "TASK-2",
                WorkflowStatus::Failed,
                Some("implementation"),
                parse_time("2026-02-20T00:00:00Z"),
                Some(parse_time("2026-02-26T10:00:00Z")),
                Vec::new(),
                Some("runner timeout"),
            ),
            make_workflow(
                "WF-001",
                "TASK-1",
                WorkflowStatus::Failed,
                Some("qa"),
                parse_time("2026-02-20T00:00:00Z"),
                Some(parse_time("2026-02-25T11:00:00Z")),
                vec![make_phase(
                    "qa",
                    WorkflowPhaseStatus::Failed,
                    Some(parse_time("2026-02-25T11:00:00Z")),
                    Some("qa gate failed"),
                )],
                None,
            ),
            make_workflow(
                "WF-003",
                "TASK-3",
                WorkflowStatus::Failed,
                Some("merge"),
                parse_time("2026-02-20T00:00:00Z"),
                Some(parse_time("2026-02-24T11:00:00Z")),
                vec![
                    make_phase(
                        "implementation",
                        WorkflowPhaseStatus::Failed,
                        Some(parse_time("2026-02-24T10:00:00Z")),
                        Some("compile failed"),
                    ),
                    make_phase(
                        "qa",
                        WorkflowPhaseStatus::Failed,
                        Some(parse_time("2026-02-24T11:00:00Z")),
                        Some("tests failed"),
                    ),
                ],
                None,
            ),
            make_workflow(
                "WF-004",
                "TASK-4",
                WorkflowStatus::Running,
                Some("implementation"),
                parse_time("2026-02-20T00:00:00Z"),
                None,
                vec![make_phase(
                    "implementation",
                    WorkflowPhaseStatus::Running,
                    None,
                    None,
                )],
                None,
            ),
            make_workflow(
                "WF-005",
                "TASK-5",
                WorkflowStatus::Failed,
                None,
                parse_time("2026-02-20T00:00:00Z"),
                Some(parse_time("2026-02-27T09:00:00Z")),
                Vec::new(),
                Some("unknown failure"),
            ),
        ];

        let entries = recent_failures(&workflows);
        assert_eq!(entries.len(), 3, "entries should be capped at 3");
        assert_eq!(entries[0].workflow_id, "WF-005");
        assert_eq!(entries[1].workflow_id, "WF-002");
        assert_eq!(
            entries[1].phase_id, "implementation",
            "current_phase should be used when no failed phase exists"
        );
        assert_eq!(
            entries[2].phase_id, "qa",
            "latest failed phase should be selected"
        );
    }

    #[test]
    fn latest_failed_phase_uses_phase_order_when_timestamps_are_missing() {
        let workflow = make_workflow(
            "WF-100",
            "TASK-100",
            WorkflowStatus::Failed,
            Some("implementation"),
            parse_time("2026-02-20T00:00:00Z"),
            Some(parse_time("2026-02-27T09:00:00Z")),
            vec![
                make_phase(
                    "implementation",
                    WorkflowPhaseStatus::Failed,
                    None,
                    Some("compile failed"),
                ),
                make_phase(
                    "qa",
                    WorkflowPhaseStatus::Failed,
                    None,
                    Some("tests failed"),
                ),
            ],
            None,
        );

        let (phase_id, failed_at, failure_reason) = latest_failed_phase(&workflow);
        assert_eq!(phase_id, "qa");
        assert_eq!(failed_at, parse_time("2026-02-27T09:00:00Z"));
        assert_eq!(failure_reason.as_deref(), Some("tests failed"));
    }

    #[test]
    fn active_agent_assignments_fill_unknown_slots() {
        let workflows = vec![make_workflow(
            "WF-001",
            "TASK-001",
            WorkflowStatus::Running,
            Some("implementation"),
            parse_time("2026-02-20T00:00:00Z"),
            None,
            vec![make_phase(
                "implementation",
                WorkflowPhaseStatus::Running,
                None,
                None,
            )],
            None,
        )];
        let tasks = vec![make_task(
            "TASK-001",
            "Implement status",
            TaskStatus::InProgress,
            None,
        )];

        let assignments = active_agent_assignments(3, &workflows, &tasks);
        assert_eq!(assignments.len(), 3);
        assert!(assignments[0].attributed);
        assert_eq!(assignments[0].task_id, "TASK-001");
        assert_eq!(assignments[1].workflow_id, "unknown-1");
        assert!(!assignments[1].attributed);
    }

    #[test]
    fn active_agent_assignments_are_limited_to_daemon_count() {
        let workflows = vec![
            make_workflow(
                "WF-001",
                "TASK-001",
                WorkflowStatus::Running,
                Some("implementation"),
                parse_time("2026-02-20T00:00:00Z"),
                None,
                vec![make_phase(
                    "implementation",
                    WorkflowPhaseStatus::Running,
                    None,
                    None,
                )],
                None,
            ),
            make_workflow(
                "WF-002",
                "TASK-002",
                WorkflowStatus::Running,
                Some("qa"),
                parse_time("2026-02-20T00:00:00Z"),
                None,
                vec![make_phase("qa", WorkflowPhaseStatus::Running, None, None)],
                None,
            ),
        ];
        let tasks = vec![
            make_task("TASK-001", "One", TaskStatus::InProgress, None),
            make_task("TASK-002", "Two", TaskStatus::InProgress, None),
        ];

        let assignments = active_agent_assignments(1, &workflows, &tasks);
        assert_eq!(assignments.len(), 1);
        assert_eq!(assignments[0].workflow_id, "WF-001");
    }

    #[test]
    fn active_agent_assignment_uses_unknown_task_title_when_task_is_missing() {
        let workflows = vec![make_workflow(
            "WF-001",
            "TASK-404",
            WorkflowStatus::Running,
            Some("implementation"),
            parse_time("2026-02-20T00:00:00Z"),
            None,
            vec![make_phase(
                "implementation",
                WorkflowPhaseStatus::Running,
                None,
                None,
            )],
            None,
        )];

        let assignments = active_agent_assignments(1, &workflows, &[]);
        assert_eq!(assignments.len(), 1);
        assert_eq!(assignments[0].task_id, "TASK-404");
        assert_eq!(assignments[0].task_title, "Unknown task");
        assert!(assignments[0].attributed);
    }

    #[test]
    fn task_summary_uses_done_status_from_by_status() {
        let mut by_status = HashMap::new();
        by_status.insert("done".to_string(), 2);
        by_status.insert("cancelled".to_string(), 4);
        let summary = build_task_summary_slice(
            Some(&TaskStatistics {
                total: 10,
                by_status,
                by_priority: HashMap::new(),
                by_type: HashMap::new(),
                in_progress: 3,
                blocked: 1,
                completed: 6,
            }),
            None,
            None,
        );
        assert_eq!(summary.done, 2);
        assert_eq!(summary.in_progress, 3);
        assert_eq!(summary.blocked, 1);
    }

    #[test]
    fn task_summary_falls_back_to_task_scan_when_statistics_unavailable() {
        let tasks = vec![
            make_task("TASK-001", "Done", TaskStatus::Done, None),
            make_task("TASK-002", "In Progress", TaskStatus::InProgress, None),
            make_task("TASK-003", "Ready", TaskStatus::Ready, None),
            make_task("TASK-004", "Blocked", TaskStatus::Blocked, None),
            make_task("TASK-005", "On Hold", TaskStatus::OnHold, None),
            make_task("TASK-006", "Backlog", TaskStatus::Backlog, None),
        ];

        let summary = build_task_summary_slice(None, Some(&tasks), None);
        assert!(summary.available);
        assert_eq!(summary.total, 6);
        assert_eq!(summary.done, 1);
        assert_eq!(summary.in_progress, 1);
        assert_eq!(summary.ready, 1);
        assert_eq!(summary.blocked, 2);
    }

    #[test]
    fn ci_status_marks_gh_unavailable_without_failing() {
        let status = ci_status_from_lookup(CiLookupOutcome::Unavailable(
            "gh CLI is not installed".to_string(),
        ));
        assert!(!status.available);
        assert!(status.error.is_none());
        assert_eq!(status.reason.as_deref(), Some("gh CLI is not installed"));
    }

    #[test]
    fn ci_status_reports_when_no_workflow_runs_exist() {
        let status = ci_status_from_lookup(CiLookupOutcome::Success(None));
        assert!(status.available);
        assert!(status.last_run.is_none());
        assert_eq!(status.reason.as_deref(), Some("no workflow runs found"));
        assert!(status.error.is_none());
    }

    #[test]
    fn parse_gh_run_list_extracts_latest_run() {
        let payload = r#"
[
  {
    "databaseId": 42,
    "displayTitle": "CI",
    "name": "CI / test",
    "workflowName": "ci",
    "status": "completed",
    "conclusion": "success",
    "event": "push",
    "headBranch": "main",
    "headSha": "abc123",
    "createdAt": "2026-02-26T10:00:00Z",
    "updatedAt": "2026-02-26T10:10:00Z",
    "url": "https://example.test/run/42"
  }
]
"#;
        let run = parse_gh_run_list(payload)
            .expect("payload should parse")
            .expect("payload should include one run");
        assert_eq!(run.id, Some(42));
        assert_eq!(run.status, "completed");
        assert_eq!(run.conclusion.as_deref(), Some("success"));
    }

    #[test]
    fn parse_gh_run_list_defaults_missing_status_to_unknown() {
        let payload = r#"
[
  {
    "databaseId": 43,
    "displayTitle": "CI",
    "workflowName": "ci"
  }
]
"#;
        let run = parse_gh_run_list(payload)
            .expect("payload should parse")
            .expect("payload should include one run");
        assert_eq!(run.id, Some(43));
        assert_eq!(run.status, "unknown");
    }

    #[test]
    fn parse_gh_run_list_rejects_invalid_payload() {
        let error = parse_gh_run_list("{invalid json").expect_err("invalid JSON should fail");
        assert!(error
            .to_string()
            .contains("failed to parse gh run list JSON payload"));
    }

    #[test]
    fn ci_status_reports_lookup_errors_non_fatally() {
        let status = ci_status_from_lookup(CiLookupOutcome::Failure("lookup failed".to_string()));
        assert!(status.available);
        assert!(status.last_run.is_none());
        assert_eq!(status.error.as_deref(), Some("lookup failed"));
    }

    #[test]
    fn render_status_dashboard_uses_required_section_order() {
        let dashboard = StatusDashboard {
            schema: STATUS_SCHEMA,
            project_root: "/tmp/project".to_string(),
            generated_at: parse_time("2026-02-27T00:00:00Z"),
            daemon: build_daemon_slice(
                Some(&DaemonHealth {
                    healthy: true,
                    status: DaemonStatus::Running,
                    runner_connected: true,
                    runner_pid: Some(123),
                    active_agents: 1,
                    max_agents: Some(5),
                    project_root: Some("/tmp/project".to_string()),
                }),
                None,
            ),
            active_agents: ActiveAgentsSlice {
                available: true,
                count: 0,
                assignments: Vec::new(),
                error: None,
            },
            task_summary: TaskSummarySlice {
                available: true,
                total: 0,
                done: 0,
                in_progress: 0,
                ready: 0,
                blocked: 0,
                error: None,
            },
            recent_completions: RecentCompletionsSlice {
                available: true,
                entries: Vec::new(),
                error: None,
            },
            recent_failures: RecentFailuresSlice {
                available: true,
                entries: Vec::new(),
                error: None,
            },
            ci: CiStatusSlice {
                provider: CI_PROVIDER_GITHUB,
                available: false,
                last_run: None,
                reason: Some("gh CLI is not installed".to_string()),
                error: None,
            },
        };

        let output = render_status_dashboard(&dashboard);
        let daemon_idx = output.find("Daemon").expect("daemon section should exist");
        let agents_idx = output
            .find("Active Agents")
            .expect("active agents section should exist");
        let summary_idx = output
            .find("Task Summary")
            .expect("task summary section should exist");
        let completions_idx = output
            .find("Recent Completions")
            .expect("recent completions section should exist");
        let failures_idx = output
            .find("Recent Failures")
            .expect("recent failures section should exist");
        let ci_idx = output.find("CI Status").expect("ci section should exist");

        assert!(daemon_idx < agents_idx);
        assert!(agents_idx < summary_idx);
        assert!(summary_idx < completions_idx);
        assert!(completions_idx < failures_idx);
        assert!(failures_idx < ci_idx);
    }
}
