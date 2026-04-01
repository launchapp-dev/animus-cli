use super::*;
use orchestrator_core::{
    Assignee, ChecklistItem, Complexity, ImpactArea, Priority, ResourceRequirements, RiskLevel, Scope, TaskDependency,
    TaskMetadata, TaskType, WorkflowActivitySummary, WorkflowMetadata,
};
use std::collections::HashMap;

fn parse_time(value: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(value).expect("timestamp should be valid RFC3339").with_timezone(&Utc)
}

fn make_task(id: &str, title: &str, status: TaskStatus, completed_at: Option<DateTime<Utc>>) -> OrchestratorTask {
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
        resolution: None,
        resource_requirements: ResourceRequirements::default(),
        consecutive_dispatch_failures: None,
        last_dispatch_failure_at: None,
        dispatch_history: Vec::new(),
    }
}

fn make_activity_summary(workflow_id: &str, task_id: &str, phase_id: &str) -> WorkflowActivitySummary {
    WorkflowActivitySummary {
        workflow_id: workflow_id.to_string(),
        task_id: task_id.to_string(),
        status: "running".to_string(),
        phase_id: phase_id.to_string(),
    }
}

#[test]
fn recent_completions_are_sorted_and_limited() {
    let tasks = vec![
        make_task("TASK-003", "third", TaskStatus::Done, Some(parse_time("2026-02-21T12:00:00Z"))),
        make_task("TASK-001", "first", TaskStatus::Done, Some(parse_time("2026-02-20T10:00:00Z"))),
        make_task("TASK-002", "second", TaskStatus::Done, Some(parse_time("2026-02-20T10:00:00Z"))),
        make_task("TASK-004", "fourth", TaskStatus::Done, Some(parse_time("2026-02-19T10:00:00Z"))),
        make_task("TASK-005", "fifth", TaskStatus::Done, Some(parse_time("2026-02-18T10:00:00Z"))),
        make_task("TASK-006", "sixth", TaskStatus::Done, Some(parse_time("2026-02-17T10:00:00Z"))),
        make_task("TASK-007", "skip-no-completed-at", TaskStatus::Done, None),
        make_task("TASK-008", "skip-cancelled", TaskStatus::Cancelled, Some(parse_time("2026-02-22T10:00:00Z"))),
    ];

    let entries = recent_completions(&tasks);
    assert_eq!(entries.len(), 5, "entries should be capped at 5");
    let ids: Vec<&str> = entries.iter().map(|entry| entry.task_id.as_str()).collect();
    assert_eq!(ids, vec!["TASK-003", "TASK-001", "TASK-002", "TASK-004", "TASK-005"]);
}

#[test]
fn active_agent_assignments_fill_unknown_slots() {
    let workflows = vec![make_activity_summary("WF-001", "TASK-001", "implementation")];
    let mut titles = HashMap::new();
    titles.insert("TASK-001".to_string(), "Implement status".to_string());

    let assignments = active_agent_assignments(3, &workflows, &titles);
    assert_eq!(assignments.len(), 3);
    assert!(assignments[0].attributed);
    assert_eq!(assignments[0].task_id, "TASK-001");
    assert_eq!(assignments[1].workflow_id, "unknown-1");
    assert!(!assignments[1].attributed);
}

#[test]
fn active_agent_assignments_are_limited_to_daemon_count() {
    let workflows = vec![
        make_activity_summary("WF-001", "TASK-001", "implementation"),
        make_activity_summary("WF-002", "TASK-002", "qa"),
    ];
    let mut titles = HashMap::new();
    titles.insert("TASK-001".to_string(), "One".to_string());
    titles.insert("TASK-002".to_string(), "Two".to_string());

    let assignments = active_agent_assignments(1, &workflows, &titles);
    assert_eq!(assignments.len(), 1);
    assert_eq!(assignments[0].workflow_id, "WF-001");
}

#[test]
fn active_agent_assignment_uses_unknown_task_title_when_task_is_missing() {
    let workflows = vec![make_activity_summary("WF-001", "TASK-404", "implementation")];

    let assignments = active_agent_assignments(1, &workflows, &HashMap::new());
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
fn ci_status_marks_gh_unavailable_without_failing() {
    let status = ci_status_from_lookup(CiLookupOutcome::Unavailable("gh CLI is not installed".to_string()));
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
    let run = parse_gh_run_list(payload).expect("payload should parse").expect("payload should include one run");
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
    let run = parse_gh_run_list(payload).expect("payload should parse").expect("payload should include one run");
    assert_eq!(run.id, Some(43));
    assert_eq!(run.status, "unknown");
}

#[test]
fn parse_gh_run_list_rejects_invalid_payload() {
    let error = parse_gh_run_list("{invalid json").expect_err("invalid JSON should fail");
    assert!(error.to_string().contains("failed to parse gh run list JSON payload"));
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
                pool_size: Some(5),
                project_root: Some("/tmp/project".to_string()),
                daemon_pid: None,
                process_alive: None,
                pool_utilization_percent: None,
                queued_tasks: None,
                total_agents_spawned: None,
                total_agents_completed: None,
                total_agents_failed: None,
            }),
            None,
        ),
        active_agents: ActiveAgentsSlice { available: true, count: 0, assignments: Vec::new(), error: None },
        task_summary: TaskSummarySlice {
            available: true,
            total: 0,
            done: 0,
            in_progress: 0,
            ready: 0,
            blocked: 0,
            error: None,
        },
        recent_completions: RecentCompletionsSlice { available: true, entries: Vec::new(), error: None },
        recent_failures: RecentFailuresSlice { available: true, entries: Vec::new(), error: None },
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
    let agents_idx = output.find("Active Agents").expect("active agents section should exist");
    let summary_idx = output.find("Task Summary").expect("task summary section should exist");
    let completions_idx = output.find("Recent Completions").expect("recent completions section should exist");
    let failures_idx = output.find("Recent Failures").expect("recent failures section should exist");
    let ci_idx = output.find("CI Status").expect("ci section should exist");

    assert!(daemon_idx < agents_idx);
    assert!(agents_idx < summary_idx);
    assert!(summary_idx < completions_idx);
    assert!(completions_idx < failures_idx);
    assert!(failures_idx < ci_idx);
}
