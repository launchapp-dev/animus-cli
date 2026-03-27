use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::path::Path;
use std::process::{Command as ProcessCommand, Stdio};
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Utc};
use orchestrator_core::{
    open_project_db, DaemonHealth, DaemonStatus, OrchestratorTask, OrchestratorWorkflow, RequirementItem, ServiceHub,
    TaskStatistics, TaskStatus, WorkflowPhaseStatus, WorkflowStateManager, WorkflowStatus,
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
    next_actions: NextActionsSlice,
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
struct NextActionsSlice {
    available: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    entry: Option<NextActionEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct NextActionEntry {
    task_id: String,
    task_title: String,
    priority: String,
    #[serde(default)]
    linked_requirements: Vec<RequirementRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    active_workflow_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct RequirementRef {
    id: String,
    title: String,
}

#[derive(Debug)]
struct WorkflowStatusSnapshot {
    active_workflows: Vec<OrchestratorWorkflow>,
    recent_failures: Vec<RecentFailureEntry>,
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

pub(crate) async fn handle_status(hub: Arc<dyn ServiceHub>, project_root: &str, json: bool) -> Result<()> {
    let daemon_service = hub.daemon();
    let tasks_service = hub.tasks();
    let task_stats_service = tasks_service.clone();
    let prioritized_service = hub.tasks();
    let requirements_service = hub.planning();

    let (daemon_result, tasks_result, task_stats_result, workflow_snapshot_result, prioritized_result, requirements_result, ci_slice) = tokio::join!(
        daemon_service.health(),
        tasks_service.list(),
        task_stats_service.statistics(),
        collect_workflow_status_snapshot(project_root),
        prioritized_service.list_prioritized(),
        requirements_service.list_requirements(),
        collect_ci_status(project_root),
    );

    let (daemon_health, daemon_error) = split_result(daemon_result);
    let (tasks, tasks_error) = split_result(tasks_result);
    let (task_stats, task_stats_error) = split_result(task_stats_result);
    let (workflow_snapshot, workflows_error) = split_result(workflow_snapshot_result);
    let (prioritized_tasks, prioritized_error) = split_result(prioritized_result);
    let (requirements, requirements_error) = split_result(requirements_result);

    let dashboard = StatusDashboard {
        schema: STATUS_SCHEMA,
        project_root: project_root.to_string(),
        generated_at: Utc::now(),
        daemon: build_daemon_slice(daemon_health.as_ref(), daemon_error.clone()),
        active_agents: build_active_agents_slice(
            daemon_health.as_ref(),
            workflow_snapshot.as_ref().map(|snapshot| snapshot.active_workflows.as_slice()),
            tasks.as_deref(),
            combine_errors([daemon_error.as_deref(), workflows_error.as_deref(), tasks_error.as_deref()]),
        ),
        task_summary: build_task_summary_slice(
            task_stats.as_ref(),
            tasks.as_deref(),
            combine_errors([task_stats_error.as_deref(), tasks_error.as_deref()]),
        ),
        recent_completions: build_recent_completions_slice(tasks.as_deref(), tasks_error),
        recent_failures: build_recent_failures_slice(
            workflow_snapshot.as_ref().map(|snapshot| snapshot.recent_failures.as_slice()),
            workflows_error,
        ),
        next_actions: build_next_actions_slice(
            prioritized_tasks.as_deref(),
            requirements.as_deref(),
            workflow_snapshot.as_ref().map(|snapshot| snapshot.active_workflows.as_slice()),
            combine_errors([prioritized_error.as_deref(), requirements_error.as_deref()]),
        ),
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
    let count = daemon_health.map(|health| health.active_agents).unwrap_or(0);
    let assignments = active_agent_assignments(count, workflows.unwrap_or_default(), tasks.unwrap_or_default());
    ActiveAgentsSlice { available: daemon_health.is_some(), count, assignments, error }
}

fn active_agent_assignments(
    active_count: usize,
    workflows: &[OrchestratorWorkflow],
    tasks: &[OrchestratorTask],
) -> Vec<ActiveAgentAssignment> {
    let task_titles: HashMap<&str, &str> = tasks.iter().map(|task| (task.id.as_str(), task.title.as_str())).collect();

    let mut running: Vec<&OrchestratorWorkflow> =
        workflows.iter().filter(|workflow| workflow.status == WorkflowStatus::Running).collect();
    running.sort_by(|left, right| left.id.cmp(&right.id).then_with(|| left.task_id.cmp(&right.task_id)));

    let attributed_count = active_count.min(running.len());
    let mut assignments: Vec<ActiveAgentAssignment> = running
        .into_iter()
        .take(attributed_count)
        .map(|workflow| ActiveAgentAssignment {
            task_id: workflow.task_id.clone(),
            task_title: task_titles.get(workflow.task_id.as_str()).copied().unwrap_or("Unknown task").to_string(),
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
            done: tasks.iter().filter(|task| task.status == TaskStatus::Done).count(),
            in_progress: tasks.iter().filter(|task| task.status == TaskStatus::InProgress).count(),
            ready: tasks.iter().filter(|task| task.status == TaskStatus::Ready).count(),
            blocked: tasks.iter().filter(|task| task.status.is_blocked()).count(),
            error,
        };
    }

    TaskSummarySlice { available: false, total: 0, done: 0, in_progress: 0, ready: 0, blocked: 0, error }
}

fn build_recent_completions_slice(tasks: Option<&[OrchestratorTask]>, error: Option<String>) -> RecentCompletionsSlice {
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

fn build_next_actions_slice(
    prioritized_tasks: Option<&[OrchestratorTask]>,
    requirements: Option<&[RequirementItem]>,
    workflows: Option<&[OrchestratorWorkflow]>,
    error: Option<String>,
) -> NextActionsSlice {
    let requirements_by_id: HashMap<&str, &str> = requirements
        .map(|reqs| reqs.iter().map(|req| (req.id.as_str(), req.title.as_str())).collect())
        .unwrap_or_default();

    let active_workflows_by_task: HashMap<&str, &str> = workflows
        .map(|wfs| {
            wfs.iter()
                .filter(|wf| wf.status == WorkflowStatus::Running)
                .map(|wf| (wf.task_id.as_str(), wf.id.as_str()))
                .collect()
        })
        .unwrap_or_default();

    if let Some(tasks) = prioritized_tasks {
        for task in tasks {
            if task.status == TaskStatus::Ready {
                let linked_requirements = task
                    .linked_requirements
                    .iter()
                    .filter_map(|req_id| {
                        requirements_by_id.get(req_id.as_str()).map(|&title| RequirementRef {
                            id: req_id.clone(),
                            title: title.to_string(),
                        })
                    })
                    .collect();

                let active_workflow_id =
                    active_workflows_by_task.get(task.id.as_str()).map(|&id| id.to_string());

                return NextActionsSlice {
                    available: true,
                    entry: Some(NextActionEntry {
                        task_id: task.id.clone(),
                        task_title: task.title.clone(),
                        priority: task.priority.as_str().to_string(),
                        linked_requirements,
                        active_workflow_id,
                    }),
                    reason: None,
                    error,
                };
            }
        }

        let reason = if tasks.is_empty() {
            "no tasks available"
        } else {
            let has_blocked = tasks.iter().any(|task| task.status.is_blocked());
            let has_in_progress = tasks.iter().any(|task| task.status == TaskStatus::InProgress);
            let has_done = tasks.iter().any(|task| task.status == TaskStatus::Done);
            let has_backlog = tasks.iter().any(|task| task.status == TaskStatus::Backlog);

            if has_blocked {
                "all tasks blocked"
            } else if !has_in_progress && !has_done && !has_backlog {
                "no tasks available"
            } else if tasks.iter().all(|task| task.status == TaskStatus::InProgress) {
                "all tasks in progress"
            } else if tasks.iter().all(|task| task.status == TaskStatus::Done) {
                "all tasks done"
            } else {
                "no ready tasks"
            }
        };

        return NextActionsSlice {
            available: true,
            entry: None,
            reason: Some(reason.to_string()),
            error,
        };
    }

    NextActionsSlice { available: false, entry: None, reason: None, error }
}

fn latest_failed_phase(workflow: &OrchestratorWorkflow) -> (String, DateTime<Utc>, Option<String>) {
    let failed_phase = workflow
        .phases
        .iter()
        .enumerate()
        .filter(|(_, phase)| phase.status == WorkflowPhaseStatus::Failed)
        .max_by(|left, right| left.1.completed_at.cmp(&right.1.completed_at).then_with(|| left.0.cmp(&right.0)))
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

async fn collect_workflow_status_snapshot(project_root: &str) -> Result<WorkflowStatusSnapshot> {
    let project_root = project_root.to_string();
    tokio::task::spawn_blocking(move || load_workflow_status_snapshot(project_root.as_str()))
        .await
        .map_err(|error| anyhow!("failed to collect workflow status snapshot: {error}"))?
}

fn load_workflow_status_snapshot(project_root: &str) -> Result<WorkflowStatusSnapshot> {
    let manager = WorkflowStateManager::new(project_root);
    Ok(WorkflowStatusSnapshot {
        active_workflows: manager.list()?,
        recent_failures: load_recent_failures(project_root, RECENT_FAILURES_LIMIT)?,
    })
}

fn load_recent_failures(project_root: &str, limit: usize) -> Result<Vec<RecentFailureEntry>> {
    let candidate_limit = i64::try_from(limit.saturating_mul(32)).context("recent failure limit overflow")?;
    let conn = open_project_db(Path::new(project_root))?;
    let mut stmt = conn.prepare(
        "SELECT workflow_id, MAX(timestamp) AS failed_at
         FROM checkpoints
         WHERE status = 'failed'
         GROUP BY workflow_id
         ORDER BY failed_at DESC
         LIMIT ?1",
    )?;
    let candidate_ids: Vec<String> =
        stmt.query_map([candidate_limit], |row| row.get::<_, String>(0))?.filter_map(|row| row.ok()).collect();
    drop(stmt);
    drop(conn);

    let manager = WorkflowStateManager::new(project_root);
    let mut seen = HashSet::new();
    let mut entries = Vec::new();
    for workflow_id in candidate_ids {
        if !seen.insert(workflow_id.clone()) {
            continue;
        }
        let Ok(workflow) = manager.load(&workflow_id) else {
            continue;
        };
        if workflow.status != WorkflowStatus::Failed {
            continue;
        }

        let (phase_id, failed_at, phase_error) = latest_failed_phase(&workflow);
        entries.push(RecentFailureEntry {
            workflow_id: workflow.id.clone(),
            task_id: workflow.task_id.clone(),
            phase_id,
            failed_at,
            failure_reason: workflow.failure_reason.clone().or(phase_error),
        });
    }

    entries.sort_by(|left, right| {
        right.failed_at.cmp(&left.failed_at).then_with(|| left.workflow_id.cmp(&right.workflow_id))
    });
    entries.truncate(limit);
    Ok(entries)
}

async fn collect_ci_status(project_root: &str) -> CiStatusSlice {
    let project_root = project_root.to_string();
    match tokio::task::spawn_blocking(move || ci_status_from_lookup(lookup_ci_status(project_root.as_str()))).await {
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
            reason: if run.is_none() { Some("no workflow runs found".to_string()) } else { None },
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

fn render_status_dashboard(dashboard: &StatusDashboard) -> String {
    let mut output = String::new();
    let _ = writeln!(&mut output, "AO Status Dashboard");
    let _ = writeln!(&mut output, "Project Root: {}", dashboard.project_root);
    let _ = writeln!(&mut output, "Generated At: {}", dashboard.generated_at.to_rfc3339());
    let _ = writeln!(&mut output);

    let _ = writeln!(&mut output, "Daemon");
    let _ = writeln!(&mut output, "  status: {}", dashboard.daemon.status);
    let _ = writeln!(&mut output, "  running: {}", dashboard.daemon.running);
    let _ = writeln!(&mut output, "  runner_connected: {}", dashboard.daemon.runner_connected);
    let _ = writeln!(
        &mut output,
        "  runner_pid: {}",
        dashboard.daemon.runner_pid.map(|pid| pid.to_string()).unwrap_or_else(|| "n/a".to_string())
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
                entry.task_id, entry.task_title, entry.workflow_id, entry.phase_id, entry.attributed
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

    let _ = writeln!(&mut output, "Next Actions");
    if let Some(entry) = dashboard.next_actions.entry.as_ref() {
        let _ = writeln!(
            &mut output,
            "  - task_id={} task_title={} priority={}",
            entry.task_id, entry.task_title, entry.priority
        );
        if entry.linked_requirements.is_empty() {
            let _ = writeln!(&mut output, "    linked_requirements: none");
        } else {
            for req in &entry.linked_requirements {
                let _ = writeln!(&mut output, "      - id={} title={}", req.id, req.title);
            }
        }
        if let Some(workflow_id) = entry.active_workflow_id.as_deref() {
            let _ = writeln!(&mut output, "    active_workflow_id: {workflow_id}");
        }
    } else if let Some(reason) = dashboard.next_actions.reason.as_deref() {
        let _ = writeln!(&mut output, "  reason: {reason}");
    }
    if let Some(error) = dashboard.next_actions.error.as_deref() {
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

    output
}

#[cfg(test)]
mod tests;
