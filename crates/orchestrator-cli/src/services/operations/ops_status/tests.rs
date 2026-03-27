use super::*;
use orchestrator_core::{
    Assignee, ChecklistItem, Complexity, ImpactArea, Priority, ResourceRequirements, RiskLevel, Scope, SubjectRef,
    TaskDependency, TaskMetadata, TaskType, WorkflowCheckpointMetadata, WorkflowDecisionRecord, WorkflowMachineState,
    WorkflowMetadata, WorkflowPhaseExecution,
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
        workflow_ref: None,
        input: None,
        vars: HashMap::new(),
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
        subject: SubjectRef::task(task_id.to_string()),
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
fn latest_failed_phase_uses_phase_order_when_timestamps_are_missing() {
    let workflow = make_workflow(
        "WF-100",
        "TASK-100",
        WorkflowStatus::Failed,
        Some("implementation"),
        parse_time("2026-02-20T00:00:00Z"),
        Some(parse_time("2026-02-27T09:00:00Z")),
        vec![
            make_phase("implementation", WorkflowPhaseStatus::Failed, None, Some("compile failed")),
            make_phase("qa", WorkflowPhaseStatus::Failed, None, Some("tests failed")),
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
        vec![make_phase("implementation", WorkflowPhaseStatus::Running, None, None)],
        None,
    )];
    let tasks = vec![make_task("TASK-001", "Implement status", TaskStatus::InProgress, None)];

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
            vec![make_phase("implementation", WorkflowPhaseStatus::Running, None, None)],
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
        vec![make_phase("implementation", WorkflowPhaseStatus::Running, None, None)],
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
        next_actions: NextActionsSlice { available: true, entry: None, reason: Some("no ready tasks".to_string()), error: None },
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
    let next_actions_idx = output.find("Next Actions").expect("next actions section should exist");
    let ci_idx = output.find("CI Status").expect("ci section should exist");

    assert!(daemon_idx < agents_idx);
    assert!(agents_idx < summary_idx);
    assert!(summary_idx < completions_idx);
    assert!(completions_idx < failures_idx);
    assert!(failures_idx < next_actions_idx);
    assert!(next_actions_idx < ci_idx);
}

#[test]
fn next_actions_slice_finds_first_ready_task() {
    let mut task = make_task("TASK-001", "First Ready", TaskStatus::Ready, None);
    task.priority = Priority::High;
    task.linked_requirements = vec!["REQ-001".to_string(), "REQ-002".to_string()];

    let tasks = vec![task];
    let requirements = vec![
        orchestrator_core::RequirementItem {
            id: "REQ-001".to_string(),
            title: "First requirement".to_string(),
            description: String::new(),
            body: None,
            legacy_id: None,
            category: None,
            requirement_type: None,
            acceptance_criteria: Vec::new(),
            priority: Default::default(),
            status: Default::default(),
            source: String::new(),
            tags: Vec::new(),
            links: Default::default(),
            comments: Vec::new(),
            relative_path: None,
            linked_task_ids: Vec::new(),
            created_at: parse_time("2026-02-01T00:00:00Z"),
            updated_at: parse_time("2026-02-01T00:00:00Z"),
        },
        orchestrator_core::RequirementItem {
            id: "REQ-002".to_string(),
            title: "Second requirement".to_string(),
            description: String::new(),
            body: None,
            legacy_id: None,
            category: None,
            requirement_type: None,
            acceptance_criteria: Vec::new(),
            priority: Default::default(),
            status: Default::default(),
            source: String::new(),
            tags: Vec::new(),
            links: Default::default(),
            comments: Vec::new(),
            relative_path: None,
            linked_task_ids: Vec::new(),
            created_at: parse_time("2026-02-01T00:00:00Z"),
            updated_at: parse_time("2026-02-01T00:00:00Z"),
        },
    ];

    let slice = build_next_actions_slice(Some(&tasks), Some(&requirements), None, None);
    assert!(slice.available);
    assert!(slice.entry.is_some());
    assert_eq!(slice.entry.as_ref().unwrap().task_id, "TASK-001");
    assert_eq!(slice.entry.as_ref().unwrap().priority, "high");
    assert_eq!(slice.entry.as_ref().unwrap().linked_requirements.len(), 2);
    assert_eq!(slice.entry.as_ref().unwrap().linked_requirements[0].id, "REQ-001");
    assert_eq!(slice.entry.as_ref().unwrap().active_workflow_id, None);
}

#[test]
fn next_actions_slice_includes_active_workflow_id() {
    let task = make_task("TASK-001", "Ready", TaskStatus::Ready, None);
    let tasks = vec![task];

    let workflow = make_workflow(
        "WF-001",
        "TASK-001",
        WorkflowStatus::Running,
        Some("implementation"),
        parse_time("2026-02-20T00:00:00Z"),
        None,
        vec![make_phase("implementation", WorkflowPhaseStatus::Running, None, None)],
        None,
    );
    let workflows = vec![workflow];

    let slice = build_next_actions_slice(Some(&tasks), Some(&[]), Some(&workflows), None);
    assert!(slice.available);
    assert_eq!(slice.entry.as_ref().unwrap().active_workflow_id.as_deref(), Some("WF-001"));
}

#[test]
fn next_actions_slice_skips_non_ready_tasks() {
    let tasks = vec![
        make_task("TASK-001", "Blocked", TaskStatus::Blocked, None),
        make_task("TASK-002", "In Progress", TaskStatus::InProgress, None),
        make_task("TASK-003", "Ready", TaskStatus::Ready, None),
    ];

    let slice = build_next_actions_slice(Some(&tasks), Some(&[]), None, None);
    assert!(slice.available);
    assert_eq!(slice.entry.as_ref().unwrap().task_id, "TASK-003");
}

#[test]
fn next_actions_slice_provides_reason_when_no_ready_tasks() {
    let tasks = vec![
        make_task("TASK-001", "Blocked", TaskStatus::Blocked, None),
        make_task("TASK-002", "In Progress", TaskStatus::InProgress, None),
    ];

    let slice = build_next_actions_slice(Some(&tasks), Some(&[]), None, None);
    assert!(slice.available);
    assert!(slice.entry.is_none());
    assert_eq!(slice.reason.as_deref(), Some("all tasks blocked"));
}

#[test]
fn next_actions_slice_unavailable_when_no_tasks() {
    let slice = build_next_actions_slice(None, None, None, None);
    assert!(!slice.available);
    assert!(slice.entry.is_none());
    assert!(slice.reason.is_none());
}
