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
            status_changed_at: None,
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

    let assignments = active_agent_assignments(3, &workflows, &titles, &SilenceContext::empty());
    assert_eq!(assignments.len(), 3);
    assert!(assignments[0].attributed);
    assert_eq!(assignments[0].task_id, "TASK-001");
    assert_eq!(assignments[1].workflow_id, "unknown-1");
    assert!(!assignments[1].attributed);
    assert!(!assignments[0].silent);
    assert!(assignments[0].last_output_at.is_none());
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

    let assignments = active_agent_assignments(1, &workflows, &titles, &SilenceContext::empty());
    assert_eq!(assignments.len(), 1);
    assert_eq!(assignments[0].workflow_id, "WF-001");
}

fn silence_context_with(workflow_id: &str, last_output_at: DateTime<Utc>, threshold_secs: u64, now: DateTime<Utc>)
-> SilenceContext {
    let mut last = HashMap::new();
    last.insert(workflow_id.to_string(), last_output_at);
    SilenceContext { last_output_at: last, threshold_secs, now }
}

#[test]
fn silent_agent_is_flagged_past_threshold() {
    let workflows = vec![make_activity_summary("WF-001", "TASK-001", "implementation")];
    let now = parse_time("2026-06-12T01:00:00Z");
    // last output 40 minutes ago, threshold 20 minutes -> silent
    let silence = silence_context_with("WF-001", parse_time("2026-06-12T00:20:00Z"), 20 * 60, now);

    let assignments = active_agent_assignments(1, &workflows, &HashMap::new(), &silence);
    assert_eq!(assignments.len(), 1);
    assert!(assignments[0].silent);
    assert_eq!(assignments[0].silent_for_secs, Some(40 * 60));
    assert!(assignments[0].last_output_at.is_some());
}

#[test]
fn recent_output_agent_is_not_silent() {
    let workflows = vec![make_activity_summary("WF-001", "TASK-001", "implementation")];
    let now = parse_time("2026-06-12T01:00:00Z");
    // last output 2 minutes ago, threshold 20 minutes -> not silent
    let silence = silence_context_with("WF-001", parse_time("2026-06-12T00:58:00Z"), 20 * 60, now);

    let assignments = active_agent_assignments(1, &workflows, &HashMap::new(), &silence);
    assert!(!assignments[0].silent);
    assert_eq!(assignments[0].silent_for_secs, Some(120));
}

#[test]
fn zero_threshold_disables_silence_detection() {
    let workflows = vec![make_activity_summary("WF-001", "TASK-001", "implementation")];
    let now = parse_time("2026-06-12T05:00:00Z");
    let silence = silence_context_with("WF-001", parse_time("2026-06-12T00:00:00Z"), 0, now);

    let assignments = active_agent_assignments(1, &workflows, &HashMap::new(), &silence);
    assert!(!assignments[0].silent, "threshold 0 disables silence flagging");
    assert!(assignments[0].silent_for_secs.is_some());
}

#[test]
fn warnings_slice_aggregates_degraded_reasons_and_silent_agents() {
    let agents = ActiveAgentsSlice {
        available: true,
        count: 1,
        assignments: vec![ActiveAgentAssignment {
            task_id: "TASK-001".to_string(),
            task_title: "t".to_string(),
            workflow_id: "WF-001".to_string(),
            phase_id: "impl".to_string(),
            attributed: true,
            last_output_at: Some(parse_time("2026-06-12T00:00:00Z")),
            silent_for_secs: Some(3600),
            silent: true,
        }],
        error: None,
    };
    let health = DaemonHealth {
        healthy: true,
        status: DaemonStatus::Running,
        runner_connected: false,
        runner_pid: None,
        provider_plugins_healthy: true,
        active_agents: 1,
        pool_size: Some(5),
        project_root: None,
        daemon_pid: None,
        process_alive: None,
        pool_utilization_percent: None,
        queued_tasks: None,
        total_agents_spawned: None,
        total_agents_completed: None,
        total_agents_failed: None,
        flavor: None,
        runtime_paused: false,
        paused_at: None,
        degraded_reasons: vec!["subject_backend unroutable: ...".to_string()],
    };

    let warnings = build_warnings_slice(Some(&health), &agents);
    assert!(warnings.degraded);
    assert_eq!(warnings.degraded_reasons.len(), 1);
    assert_eq!(warnings.silent_agents, 1);
}

#[test]
fn warnings_slice_clean_when_healthy_and_quiet() {
    let agents = ActiveAgentsSlice { available: true, count: 0, assignments: Vec::new(), error: None };
    let warnings = build_warnings_slice(None, &agents);
    assert!(!warnings.degraded);
    assert!(warnings.degraded_reasons.is_empty());
    assert_eq!(warnings.silent_agents, 0);
}

#[test]
fn format_duration_secs_renders_compact_units() {
    assert_eq!(format_duration_secs(45), "45s");
    assert_eq!(format_duration_secs(90), "1m 30s");
    assert_eq!(format_duration_secs(3661), "1h 1m 1s");
}

#[test]
fn active_agent_assignment_uses_unknown_task_title_when_task_is_missing() {
    let workflows = vec![make_activity_summary("WF-001", "TASK-404", "implementation")];

    let assignments = active_agent_assignments(1, &workflows, &HashMap::new(), &SilenceContext::empty());
    assert_eq!(assignments.len(), 1);
    assert_eq!(assignments[0].task_id, "TASK-404");
    assert_eq!(assignments[0].task_title, "Unknown task");
    assert!(assignments[0].attributed);
}

#[test]
fn task_summary_from_router_aggregates_normalized_statuses() {
    let subjects = vec![
        serde_json::json!({ "id": "task:T-1", "status": "ready" }),
        serde_json::json!({ "id": "task:T-2", "status": "in_progress" }),
        serde_json::json!({ "id": "task:T-3", "status": "in-progress" }),
        serde_json::json!({ "id": "task:T-4", "status": "done" }),
        serde_json::json!({ "id": "task:T-5", "status": "completed" }),
        serde_json::json!({ "id": "task:T-6", "status": "blocked" }),
        serde_json::json!({ "id": "task:T-7", "status": "ready", "paused": true }),
        serde_json::json!({ "id": "task:T-8", "status": "on_hold" }),
        serde_json::json!({ "id": "task:T-9", "status": "ready", "blocked_reason": "workflow paused" }),
    ];
    let summary = build_task_summary_slice_from_router(Some(&subjects), None);
    assert!(summary.available);
    assert_eq!(summary.total, 9);
    assert_eq!(summary.ready, 3);
    assert_eq!(summary.in_progress, 2);
    assert_eq!(summary.done, 2);
    // T-6 (blocked) + T-7 (paused flag) + T-8 (on-hold status) + T-9
    // (workflow-pause annotation on a non-blocked status) all count blocked.
    assert_eq!(summary.blocked, 4);
}

#[test]
fn task_summary_unavailable_when_router_unreachable_never_reports_zero_as_truth() {
    let summary = build_task_summary_slice_from_router(None, Some("router down".to_string()));
    assert!(!summary.available);
    assert_eq!(summary.total, 0);
    assert_eq!(summary.error.as_deref(), Some("router down"));
}

#[test]
fn blocked_subjects_slice_lists_blocked_and_paused() {
    let subjects = vec![
        serde_json::json!({ "id": "task:T-1", "status": "ready" }),
        serde_json::json!({
            "id": "task:T-2",
            "status": "blocked",
            "blocked_reason": "dep gate",
            "blocked_by": "wf-9"
        }),
        serde_json::json!({ "id": "task:T-3", "status": "ready", "paused": true }),
        serde_json::json!({ "id": "task:T-4", "status": "on-hold" }),
        serde_json::json!({
            "id": "task:T-5",
            "status": "ready",
            "blocked_reason": "workflow paused",
            "blocked_by": "wf-77"
        }),
    ];
    let slice = build_blocked_subjects_slice(Some(&subjects), None);
    assert!(slice.available);
    assert_eq!(slice.count, 4);
    let blocked = slice.entries.iter().find(|e| e.id == "task:T-2").expect("blocked entry");
    assert_eq!(blocked.state, "blocked");
    assert_eq!(blocked.blocked_reason.as_deref(), Some("dep gate"));
    assert_eq!(blocked.blocked_by.as_deref(), Some("wf-9"));
    let paused = slice.entries.iter().find(|e| e.id == "task:T-3").expect("paused entry");
    assert_eq!(paused.state, "paused");
    let on_hold = slice.entries.iter().find(|e| e.id == "task:T-4").expect("on-hold entry");
    assert_eq!(on_hold.state, "blocked");
    // Workflow-pause annotation on a non-blocked, non-paused subject still
    // surfaces (state falls back to "paused" since status isn't blocked).
    let annotated = slice.entries.iter().find(|e| e.id == "task:T-5").expect("annotated entry");
    assert_eq!(annotated.state, "paused");
    assert_eq!(annotated.blocked_by.as_deref(), Some("wf-77"));
}

#[test]
fn extract_subject_list_handles_envelope_shapes() {
    let bare = serde_json::json!([{ "id": "a" }, { "id": "b" }]);
    assert_eq!(extract_subject_list(&bare).len(), 2);
    let wrapped = serde_json::json!({ "items": [{ "id": "a" }] });
    assert_eq!(extract_subject_list(&wrapped).len(), 1);
    let tasks = serde_json::json!({ "tasks": [{ "id": "a" }, { "id": "b" }, { "id": "c" }] });
    assert_eq!(extract_subject_list(&tasks).len(), 3);
    let empty = serde_json::json!({ "unrelated": 1 });
    assert!(extract_subject_list(&empty).is_empty());
}

#[test]
fn extract_next_cursor_treats_empty_and_null_as_final_page() {
    assert_eq!(extract_next_cursor(&serde_json::json!({ "next_cursor": "abc" })), Some(serde_json::json!("abc")));
    assert!(extract_next_cursor(&serde_json::json!({ "next_cursor": null })).is_none());
    assert!(extract_next_cursor(&serde_json::json!({ "next_cursor": "   " })).is_none());
    assert!(extract_next_cursor(&serde_json::json!({ "subjects": [] })).is_none());
    assert!(extract_next_cursor(&serde_json::json!([{ "id": "a" }])).is_none());
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
        flavor: None,
        daemon: build_daemon_slice(
            Some(&DaemonHealth {
                healthy: true,
                status: DaemonStatus::Running,
                runner_connected: false,
                runner_pid: None,
                provider_plugins_healthy: true,
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
                flavor: None,
                runtime_paused: false,
                paused_at: None,
                degraded_reasons: Vec::new(),
            }),
            None,
        ),
        warnings: WarningsSlice { degraded: false, degraded_reasons: Vec::new(), silent_agents: 0 },
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
        blocked_subjects: BlockedSubjectsSlice { available: true, count: 0, entries: Vec::new(), error: None },
        needs_you: NeedsYouSlice { available: true, count: 0, entries: Vec::new(), error: None },
        recent_completions: RecentCompletionsSlice { available: true, entries: Vec::new(), error: None },
        recent_failures: RecentFailuresSlice { available: true, entries: Vec::new(), error: None },
        ci: CiStatusSlice {
            provider: CI_PROVIDER_GITHUB,
            available: false,
            last_run: None,
            reason: Some("gh CLI is not installed".to_string()),
            error: None,
            cached: false,
        },
        budget: BudgetSlice {
            available: true,
            enforcement_enabled: true,
            last_sweep_at: None,
            breaches: crate::services::cost::summarize_breaches(&[], None),
            error: None,
        },
    };

    let output = render_status_dashboard(&dashboard);
    let daemon_idx = output.find("Daemon").expect("daemon section should exist");
    let warnings_idx = output.find("Warnings").expect("warnings section should exist");
    let agents_idx = output.find("Active Agents").expect("active agents section should exist");
    let summary_idx = output.find("Task Summary").expect("task summary section should exist");
    let blocked_idx = output.find("Blocked / Paused").expect("blocked/paused section should exist");
    let needs_you_idx = output.find("Needs You").expect("needs you section should exist");
    let completions_idx = output.find("Recent Completions").expect("recent completions section should exist");
    let failures_idx = output.find("Recent Failures").expect("recent failures section should exist");
    let ci_idx = output.find("CI Status").expect("ci section should exist");

    assert!(daemon_idx < warnings_idx);
    assert!(warnings_idx < agents_idx);
    assert!(agents_idx < summary_idx);
    assert!(summary_idx < blocked_idx);
    assert!(blocked_idx < needs_you_idx);
    assert!(needs_you_idx < completions_idx);
    assert!(completions_idx < failures_idx);
    assert!(failures_idx < ci_idx);

    // The deprecated runner fields must not appear in the human dashboard.
    assert!(!output.contains("runner_connected"), "human dashboard must not render runner_connected");
    assert!(!output.contains("runner_pid"), "human dashboard must not render runner_pid");
    assert!(output.contains("provider_plugins_healthy"), "human dashboard renders provider_plugins_healthy");
}

mod cache_tests {
    use super::*;

    // HOME goes through `EnvVarGuard` so the swap holds the process-wide
    // env lock for the closure's duration — raw `set_var` here used to race
    // every test that resolves `scoped_state_root` (memory tools, daemon_run
    // resume tests) and could delete the temp HOME out from under them. The
    // raw mutations inside test closures stay safe under the held lock and
    // are restored by the guards on drop.
    fn with_cache_env<F: FnOnce(&std::path::Path)>(f: F) {
        let _guard = crate::test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let temp = tempfile::tempdir().expect("tempdir");
        let _home = protocol::test_utils::EnvVarGuard::set("HOME", Some(temp.path().to_string_lossy().as_ref()));
        let _disable = protocol::test_utils::EnvVarGuard::set("ANIMUS_DISABLE_CI_CACHE", None);
        let _ttl = protocol::test_utils::EnvVarGuard::set("ANIMUS_CI_CACHE_TTL_SECS", None);
        let project = temp.path().join("project");
        std::fs::create_dir_all(&project).expect("project dir");
        f(&project);
    }

    fn sample_slice() -> CiStatusSlice {
        CiStatusSlice {
            provider: CI_PROVIDER_GITHUB,
            available: true,
            last_run: Some(CiRunSummary {
                id: Some(99),
                title: None,
                name: None,
                workflow_name: Some("ci".to_string()),
                status: "completed".to_string(),
                conclusion: Some("success".to_string()),
                event: None,
                head_branch: None,
                head_sha: None,
                created_at: None,
                updated_at: None,
                url: None,
            }),
            reason: None,
            error: None,
            cached: false,
        }
    }

    #[test]
    fn ci_cache_hit_within_ttl_marks_cached_true() {
        with_cache_env(|project_root| {
            let slice = sample_slice();
            let pr = project_root.to_string_lossy();
            write_ci_cache(&pr, &slice).expect("write cache");
            let read = read_ci_cache(&pr).expect("should hit");
            assert!(read.cached, "served-from-cache slice should set cached=true");
            assert_eq!(read.last_run.as_ref().and_then(|r| r.id), Some(99));
        });
    }

    #[test]
    fn ci_cache_disabled_by_env_returns_none() {
        with_cache_env(|project_root| {
            let pr = project_root.to_string_lossy();
            write_ci_cache(&pr, &sample_slice()).expect("write cache");
            std::env::set_var("ANIMUS_DISABLE_CI_CACHE", "1");
            assert!(!cache_enabled());
        });
    }

    #[test]
    fn ci_cache_expired_returns_none() {
        with_cache_env(|project_root| {
            std::env::set_var("ANIMUS_CI_CACHE_TTL_SECS", "0");
            let pr = project_root.to_string_lossy();
            write_ci_cache(&pr, &sample_slice()).expect("write cache");
            assert!(read_ci_cache(&pr).is_none(), "ttl=0 should always expire");
        });
    }

    #[test]
    fn ci_cache_corrupt_file_falls_through() {
        with_cache_env(|project_root| {
            let pr = project_root.to_string_lossy();
            let path = ci_cache_path(&pr).expect("path");
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, b"not json").unwrap();
            assert!(read_ci_cache(&pr).is_none(), "corrupt cache must not panic and must fall through");
        });
    }

    #[test]
    fn ci_cache_wrong_schema_returns_none() {
        with_cache_env(|project_root| {
            let pr = project_root.to_string_lossy();
            let path = ci_cache_path(&pr).expect("path");
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, br#"{"schema":"other.v9","fetched_at":"2026-01-01T00:00:00Z","ttl_seconds":60,"payload":{"available":true,"cached":false}}"#).unwrap();
            assert!(read_ci_cache(&pr).is_none());
        });
    }

    #[test]
    fn ci_cache_isolated_per_project_root() {
        with_cache_env(|project_root_a| {
            let pr_a = project_root_a.to_string_lossy().to_string();
            // Place project-b under a sibling directory hierarchy so
            // `scoped_state_root` always hashes it to a different scope.
            let project_b = project_root_a.parent().unwrap().join("sibling-tree").join("project-b");
            std::fs::create_dir_all(&project_b).expect("project-b dir");
            let pr_b = project_b.to_string_lossy().to_string();

            // Confirm the cache paths are physically distinct before we
            // probe behavior — if they collide the assertion below is
            // meaningless and the test is wrong, not the code.
            let path_a = ci_cache_path(&pr_a).expect("a path");
            let path_b = ci_cache_path(&pr_b).expect("b path");
            assert_ne!(path_a, path_b, "scoped paths must differ");

            write_ci_cache(&pr_a, &sample_slice()).expect("write a");
            assert!(read_ci_cache(&pr_b).is_none(), "project-b must not see project-a cache");
            assert!(read_ci_cache(&pr_a).is_some(), "project-a still has its own cache");
        });
    }
}
