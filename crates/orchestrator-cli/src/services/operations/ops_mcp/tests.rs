use super::*;
use crate::services::runtime::daemon_events_log_path;
use crate::services::runtime::DaemonEventRecord;
use chrono::{Duration, Utc};
use protocol::CLI_SCHEMA_ID;
use std::collections::HashMap;
use tempfile::TempDir;

use protocol::test_utils::EnvVarGuard;

fn sample_event(seq: u64, event_type: &str, project_root: &str) -> DaemonEventRecord {
    DaemonEventRecord {
        schema: "ao.daemon.event.v1".to_string(),
        id: format!("evt-{seq}"),
        seq,
        timestamp: "2026-01-01T00:00:00Z".to_string(),
        event_type: event_type.to_string(),
        project_root: Some(project_root.to_string()),
        data: json!({ "seq": seq }),
    }
}

fn write_events(lines: &[String]) {
    let path = daemon_events_log_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("daemon event parent directory should exist");
    }
    let content = lines.iter().map(|line| format!("{line}\n")).collect::<String>();
    std::fs::write(path, content).expect("daemon event log should be written");
}

fn write_run_events(project_root: &str, run_id: &str, lines: &[String]) {
    let run_path = run_dir(project_root, &RunId(run_id.to_string()), None);
    std::fs::create_dir_all(&run_path).expect("run directory should be created");
    let payload = lines.iter().map(|line| format!("{line}\n")).collect::<String>();
    std::fs::write(run_path.join("events.jsonl"), payload).expect("run events should be written");
}

fn output_event(run_id: &str, text: &str) -> String {
    output_event_with_stream(run_id, text, protocol::OutputStreamType::Stdout)
}

fn output_event_with_stream(run_id: &str, text: &str, stream_type: protocol::OutputStreamType) -> String {
    serde_json::to_string(&AgentRunEvent::OutputChunk {
        run_id: RunId(run_id.to_string()),
        stream_type,
        text: text.to_string(),
    })
    .expect("output event should serialize")
}

fn thinking_event(run_id: &str, content: &str) -> String {
    serde_json::to_string(&AgentRunEvent::Thinking { run_id: RunId(run_id.to_string()), content: content.to_string() })
        .expect("thinking event should serialize")
}

fn error_event(run_id: &str, error: &str) -> String {
    serde_json::to_string(&AgentRunEvent::Error { run_id: RunId(run_id.to_string()), error: error.to_string() })
        .expect("error event should serialize")
}

fn save_workflow(
    project_root: &str,
    workflow_id: &str,
    task_id: &str,
    status: WorkflowStatus,
    started_at: chrono::DateTime<Utc>,
    completed_at: Option<chrono::DateTime<Utc>>,
) {
    let manager = WorkflowStateManager::new(project_root);
    manager
        .save(&OrchestratorWorkflow {
            id: workflow_id.to_string(),
            task_id: task_id.to_string(),
            workflow_ref: None,
            input: None,
            vars: HashMap::new(),
            status,
            current_phase_index: 0,
            phases: Vec::new(),
            machine_state: orchestrator_core::WorkflowMachineState::Idle,
            current_phase: None,
            started_at,
            completed_at,
            failure_reason: None,
            checkpoint_metadata: orchestrator_core::WorkflowCheckpointMetadata::default(),
            rework_counts: HashMap::new(),
            total_reworks: 0,
            decision_history: Vec::new(),
            subject: protocol::SubjectRef::task(task_id.to_string()),
        })
        .expect("workflow should be written");
}

#[test]
fn build_task_get_args_includes_id() {
    let args = build_task_get_args("task-123".to_string());
    assert_eq!(args, vec!["task".to_string(), "get".to_string(), "--id".to_string(), "task-123".to_string()]);
}

#[test]
fn build_task_list_args_includes_filters_and_sort() {
    let args = build_task_list_args(&TaskListInput {
        task_type: Some("feature".to_string()),
        status: Some("in-progress".to_string()),
        priority: Some("high".to_string()),
        risk: Some("low".to_string()),
        assignee_type: Some("human".to_string()),
        tag: vec!["api".to_string()],
        linked_requirement: Some("REQ-123".to_string()),
        linked_architecture_entity: Some("ARCH-42".to_string()),
        search: Some("critical path".to_string()),
        sort: Some("updated_at".to_string()),
        limit: Some(10),
        offset: Some(5),
        max_tokens: Some(4000),
        project_root: None,
    });
    assert_eq!(
        args,
        vec![
            "task".to_string(),
            "list".to_string(),
            "--task-type".to_string(),
            "feature".to_string(),
            "--status".to_string(),
            "in-progress".to_string(),
            "--priority".to_string(),
            "high".to_string(),
            "--risk".to_string(),
            "low".to_string(),
            "--assignee-type".to_string(),
            "human".to_string(),
            "--tag".to_string(),
            "api".to_string(),
            "--linked-requirement".to_string(),
            "REQ-123".to_string(),
            "--linked-architecture-entity".to_string(),
            "ARCH-42".to_string(),
            "--search".to_string(),
            "critical path".to_string(),
            "--sort".to_string(),
            "updated_at".to_string(),
        ]
    );
}

fn sample_cli_failure_result() -> CliExecutionResult {
    CliExecutionResult {
        command: "ao".to_string(),
        args: vec!["--json".to_string()],
        requested_args: vec!["daemon".to_string(), "start".to_string()],
        project_root: "/tmp/project".to_string(),
        exit_code: 5,
        success: false,
        stdout: String::new(),
        stderr: String::new(),
        stdout_json: None,
        stderr_json: None,
    }
}

#[test]
fn build_cli_error_payload_prefers_stderr_envelope_over_stdout_envelope() {
    let mut result = sample_cli_failure_result();
    result.stdout_json = Some(json!({
        "schema": CLI_SCHEMA_ID,
        "ok": false,
        "error": { "message": "stdout-error" }
    }));
    result.stderr_json = Some(json!({
        "schema": CLI_SCHEMA_ID,
        "ok": false,
        "error": { "message": "stderr-error" }
    }));
    result.stderr = "stderr body".to_string();

    let payload = build_cli_error_payload("ao.daemon.start", &result);
    assert_eq!(payload.pointer("/error/message").and_then(Value::as_str), Some("stderr-error"));
    assert_eq!(payload.get("exit_code").and_then(Value::as_i64), Some(5));
    assert_eq!(payload.get("stderr").and_then(Value::as_str), Some("stderr body"));
}

#[test]
fn build_cli_error_payload_falls_back_to_stdout_envelope_when_stderr_json_missing() {
    let mut result = sample_cli_failure_result();
    result.stdout_json = Some(json!({
        "schema": CLI_SCHEMA_ID,
        "ok": false,
        "error": { "message": "stdout-error" }
    }));

    let payload = build_cli_error_payload("ao.daemon.start", &result);
    assert_eq!(payload.pointer("/error/message").and_then(Value::as_str), Some("stdout-error"));
}

#[test]
fn build_task_create_args_includes_linked_requirements() {
    let args = build_task_create_args(&TaskCreateInput {
        title: "Traceability task".to_string(),
        description: Some("desc".to_string()),
        task_type: Some("feature".to_string()),
        priority: Some("high".to_string()),
        linked_requirement: vec!["REQ-123".to_string(), "REQ-456".to_string()],
        linked_architecture_entity: Vec::new(),
        project_root: None,
    });
    assert_eq!(
        args,
        vec![
            "task".to_string(),
            "create".to_string(),
            "--title".to_string(),
            "Traceability task".to_string(),
            "--description".to_string(),
            "desc".to_string(),
            "--task-type".to_string(),
            "feature".to_string(),
            "--priority".to_string(),
            "high".to_string(),
            "--linked-requirement".to_string(),
            "REQ-123".to_string(),
            "--linked-requirement".to_string(),
            "REQ-456".to_string(),
        ]
    );
}

#[test]
fn build_task_create_args_uses_empty_description_when_omitted() {
    let args = build_task_create_args(&TaskCreateInput {
        title: "Task".to_string(),
        description: None,
        task_type: None,
        priority: None,
        linked_requirement: Vec::new(),
        linked_architecture_entity: Vec::new(),
        project_root: None,
    });
    assert_eq!(
        args,
        vec![
            "task".to_string(),
            "create".to_string(),
            "--title".to_string(),
            "Task".to_string(),
            "--description".to_string(),
            String::new(),
        ]
    );
}

#[test]
fn build_task_delete_args_includes_id() {
    let args = build_task_delete_args("task-123".to_string(), None, false);
    assert_eq!(args, vec!["task".to_string(), "delete".to_string(), "--id".to_string(), "task-123".to_string()]);
}

#[test]
fn build_task_delete_args_supports_confirmation_and_dry_run() {
    let args = build_task_delete_args("task-123".to_string(), Some("task-123".to_string()), true);
    assert_eq!(
        args,
        vec![
            "task".to_string(),
            "delete".to_string(),
            "--id".to_string(),
            "task-123".to_string(),
            "--confirm".to_string(),
            "task-123".to_string(),
            "--dry-run".to_string(),
        ]
    );
}

#[test]
fn build_task_control_args_emits_pause() {
    let args = build_task_control_args("pause", "TASK-123".to_string());
    assert_eq!(args, vec!["task".to_string(), "pause".to_string(), "--id".to_string(), "TASK-123".to_string(),]);
}

#[test]
fn build_task_control_args_emits_resume() {
    let args = build_task_control_args("resume", "TASK-456".to_string());
    assert_eq!(args, vec!["task".to_string(), "resume".to_string(), "--id".to_string(), "TASK-456".to_string(),]);
}

#[test]
fn builds_requirements_get_args() {
    let args = build_requirements_get_args("REQ-123".to_string());
    assert_eq!(args, vec!["requirements".to_string(), "get".to_string(), "--id".to_string(), "REQ-123".to_string(),]);
}

#[test]
fn build_requirements_list_args_includes_filters_and_sort() {
    let args = build_requirements_list_args(&RequirementListInput {
        status: Some("draft".to_string()),
        priority: Some("must".to_string()),
        category: Some("runtime".to_string()),
        requirement_type: Some("technical".to_string()),
        tag: vec!["backend".to_string()],
        linked_task_id: Some("TASK-123".to_string()),
        search: Some("cache".to_string()),
        sort: Some("updated_at".to_string()),
        limit: Some(20),
        offset: Some(5),
        max_tokens: Some(4000),
        project_root: None,
    });
    assert_eq!(
        args,
        vec![
            "requirements".to_string(),
            "list".to_string(),
            "--status".to_string(),
            "draft".to_string(),
            "--priority".to_string(),
            "must".to_string(),
            "--category".to_string(),
            "runtime".to_string(),
            "--type".to_string(),
            "technical".to_string(),
            "--tag".to_string(),
            "backend".to_string(),
            "--linked-task-id".to_string(),
            "TASK-123".to_string(),
            "--search".to_string(),
            "cache".to_string(),
            "--sort".to_string(),
            "updated_at".to_string(),
        ]
    );
}

#[test]
fn build_requirements_create_args_includes_acceptance_criteria() {
    let args = build_requirements_create_args(&RequirementCreateInput {
        title: "Offline mode".to_string(),
        description: Some("Support sync after reconnect".to_string()),
        priority: Some("must".to_string()),
        category: Some("product".to_string()),
        requirement_type: Some("feature".to_string()),
        source: Some("research".to_string()),
        acceptance_criterion: vec!["Queues local writes".to_string(), "Resumes sync".to_string()],
        input_json: None,
        project_root: None,
    });
    assert_eq!(
        args,
        vec![
            "requirements".to_string(),
            "create".to_string(),
            "--title".to_string(),
            "Offline mode".to_string(),
            "--description".to_string(),
            "Support sync after reconnect".to_string(),
            "--priority".to_string(),
            "must".to_string(),
            "--category".to_string(),
            "product".to_string(),
            "--type".to_string(),
            "feature".to_string(),
            "--source".to_string(),
            "research".to_string(),
            "--acceptance-criterion".to_string(),
            "Queues local writes".to_string(),
            "--acceptance-criterion".to_string(),
            "Resumes sync".to_string(),
        ]
    );
}

#[test]
fn build_requirements_update_args_includes_status_links_and_replace_flag() {
    let args = build_requirements_update_args(&RequirementUpdateInput {
        id: "REQ-123".to_string(),
        title: Some("Tighten requirement".to_string()),
        description: None,
        priority: Some("should".to_string()),
        status: Some("in-progress".to_string()),
        category: None,
        requirement_type: None,
        source: None,
        linked_task_id: vec!["TASK-1".to_string(), "TASK-2".to_string()],
        acceptance_criterion: vec!["Adds retry handling".to_string()],
        replace_acceptance_criteria: true,
        input_json: None,
        project_root: None,
    });
    assert_eq!(
        args,
        vec![
            "requirements".to_string(),
            "update".to_string(),
            "--id".to_string(),
            "REQ-123".to_string(),
            "--title".to_string(),
            "Tighten requirement".to_string(),
            "--priority".to_string(),
            "should".to_string(),
            "--status".to_string(),
            "in-progress".to_string(),
            "--linked-task-id".to_string(),
            "TASK-1".to_string(),
            "--linked-task-id".to_string(),
            "TASK-2".to_string(),
            "--acceptance-criterion".to_string(),
            "Adds retry handling".to_string(),
            "--replace-acceptance-criteria".to_string(),
        ]
    );
}

#[test]
fn build_requirements_refine_args_includes_ids_and_optional_flags() {
    let args = build_requirements_refine_args(&RequirementRefineInput {
        requirement_ids: vec!["REQ-1".to_string(), "REQ-2".to_string()],
        focus: Some("tighten acceptance criteria".to_string()),
        use_ai: Some(true),
        tool: Some("codex".to_string()),
        model: Some("gpt-5".to_string()),
        timeout_secs: Some(45),
        start_runner: Some(false),
        input_json: None,
        project_root: None,
    });
    assert_eq!(
        args,
        vec![
            "requirements".to_string(),
            "refine".to_string(),
            "--id".to_string(),
            "REQ-1".to_string(),
            "--id".to_string(),
            "REQ-2".to_string(),
            "--focus".to_string(),
            "tighten acceptance criteria".to_string(),
            "--use-ai".to_string(),
            "true".to_string(),
            "--tool".to_string(),
            "codex".to_string(),
            "--model".to_string(),
            "gpt-5".to_string(),
            "--timeout-secs".to_string(),
            "45".to_string(),
            "--start-runner".to_string(),
            "false".to_string(),
        ]
    );
}

#[test]
fn build_bulk_status_item_args_basic() {
    let item = BulkTaskStatusItem { id: "TASK-1".to_string(), status: "done".to_string() };
    let args = build_bulk_status_item_args(&item);
    assert_eq!(
        args,
        vec![
            "task".to_string(),
            "status".to_string(),
            "--id".to_string(),
            "TASK-1".to_string(),
            "--status".to_string(),
            "done".to_string(),
        ]
    );
}

#[test]
fn build_bulk_update_item_args_id_only_field() {
    let item = BulkTaskUpdateItem {
        id: "TASK-2".to_string(),
        title: Some("New title".to_string()),
        description: None,
        priority: None,
        status: None,
        assignee: None,
        input_json: None,
    };
    let args = build_bulk_update_item_args(&item);
    assert_eq!(
        args,
        vec![
            "task".to_string(),
            "update".to_string(),
            "--id".to_string(),
            "TASK-2".to_string(),
            "--title".to_string(),
            "New title".to_string(),
        ]
    );
}

#[test]
fn build_bulk_update_item_args_all_optional_fields() {
    let item = BulkTaskUpdateItem {
        id: "TASK-3".to_string(),
        title: Some("T".to_string()),
        description: Some("D".to_string()),
        priority: Some("high".to_string()),
        status: Some("in-progress".to_string()),
        assignee: Some("alice".to_string()),
        input_json: Some(r#"{"k":"v"}"#.to_string()),
    };
    let args = build_bulk_update_item_args(&item);
    assert_eq!(
        args,
        vec![
            "task".to_string(),
            "update".to_string(),
            "--id".to_string(),
            "TASK-3".to_string(),
            "--title".to_string(),
            "T".to_string(),
            "--description".to_string(),
            "D".to_string(),
            "--priority".to_string(),
            "high".to_string(),
            "--status".to_string(),
            "in-progress".to_string(),
            "--assignee".to_string(),
            "alice".to_string(),
            "--input-json".to_string(),
            r#"{"k":"v"}"#.to_string(),
        ]
    );
}

#[test]
fn build_bulk_workflow_run_item_args_basic() {
    let item = BulkWorkflowRunItem { task_id: "TASK-4".to_string(), workflow_ref: None, input_json: None };
    let args = build_bulk_workflow_run_item_args(&item);
    assert_eq!(args, vec!["workflow".to_string(), "run".to_string(), "--task-id".to_string(), "TASK-4".to_string(),]);
}

#[test]
fn build_bulk_workflow_run_item_args_with_workflow_ref_and_input() {
    let item = BulkWorkflowRunItem {
        task_id: "TASK-5".to_string(),
        workflow_ref: Some("my-pipeline".to_string()),
        input_json: Some(r#"{"key":"val"}"#.to_string()),
    };
    let args = build_bulk_workflow_run_item_args(&item);
    assert_eq!(
        args,
        vec![
            "workflow".to_string(),
            "run".to_string(),
            "--task-id".to_string(),
            "TASK-5".to_string(),
            "--workflow-ref".to_string(),
            "my-pipeline".to_string(),
            "--input-json".to_string(),
            r#"{"key":"val"}"#.to_string(),
        ]
    );
}

#[test]
fn validate_bulk_status_input_rejects_empty() {
    let err = validate_bulk_status_input("ao.task.bulk-status", &[]).unwrap_err();
    assert!(err.contains("must not be empty"), "expected empty-array error, got: {err}");
}

#[test]
fn validate_bulk_status_input_rejects_over_max() {
    let updates: Vec<BulkTaskStatusItem> = (0..=MAX_BATCH_SIZE)
        .map(|i| BulkTaskStatusItem { id: format!("TASK-{i}"), status: "done".to_string() })
        .collect();
    let err = validate_bulk_status_input("ao.task.bulk-status", &updates).unwrap_err();
    assert!(err.contains("exceeds maximum"), "expected max-size error, got: {err}");
}

#[test]
fn validate_bulk_status_input_rejects_duplicate_ids() {
    let updates = vec![
        BulkTaskStatusItem { id: "TASK-1".to_string(), status: "done".to_string() },
        BulkTaskStatusItem { id: "TASK-1".to_string(), status: "todo".to_string() },
    ];
    let err = validate_bulk_status_input("ao.task.bulk-status", &updates).unwrap_err();
    assert!(err.contains("duplicate id"), "expected duplicate-id error, got: {err}");
}

#[test]
fn validate_bulk_status_input_rejects_empty_id() {
    let updates = vec![BulkTaskStatusItem { id: "  ".to_string(), status: "done".to_string() }];
    let err = validate_bulk_status_input("ao.task.bulk-status", &updates).unwrap_err();
    assert!(err.contains(".id must not be empty"), "expected empty-id error, got: {err}");
}

#[test]
fn validate_bulk_update_input_rejects_empty() {
    let err = validate_bulk_update_input("ao.task.bulk-update", &[]).unwrap_err();
    assert!(err.contains("must not be empty"), "expected empty-array error, got: {err}");
}

#[test]
fn validate_bulk_update_input_rejects_item_with_no_fields() {
    let updates = vec![BulkTaskUpdateItem {
        id: "TASK-1".to_string(),
        title: None,
        description: None,
        priority: None,
        status: None,
        assignee: None,
        input_json: None,
    }];
    let err = validate_bulk_update_input("ao.task.bulk-update", &updates).unwrap_err();
    assert!(err.contains("must include at least one update field"), "expected no-field error, got: {err}");
}

#[test]
fn validate_bulk_update_input_rejects_duplicate_ids() {
    let updates = vec![
        BulkTaskUpdateItem {
            id: "TASK-1".to_string(),
            title: Some("A".to_string()),
            description: None,
            priority: None,
            status: None,
            assignee: None,
            input_json: None,
        },
        BulkTaskUpdateItem {
            id: "TASK-1".to_string(),
            title: Some("B".to_string()),
            description: None,
            priority: None,
            status: None,
            assignee: None,
            input_json: None,
        },
    ];
    let err = validate_bulk_update_input("ao.task.bulk-update", &updates).unwrap_err();
    assert!(err.contains("duplicate id"), "expected duplicate-id error, got: {err}");
}

#[test]
fn validate_bulk_update_input_accepts_valid_items() {
    let updates = vec![
        BulkTaskUpdateItem {
            id: "TASK-1".to_string(),
            title: Some("New title".to_string()),
            description: None,
            priority: None,
            status: None,
            assignee: None,
            input_json: None,
        },
        BulkTaskUpdateItem {
            id: "TASK-2".to_string(),
            title: None,
            description: None,
            priority: Some("high".to_string()),
            status: None,
            assignee: None,
            input_json: None,
        },
    ];
    assert!(validate_bulk_update_input("ao.task.bulk-update", &updates).is_ok());
}

#[test]
fn validate_workflow_run_multiple_rejects_empty() {
    let err = validate_workflow_run_multiple_input("ao.workflow.run-multiple", &[]).unwrap_err();
    assert!(err.contains("must not be empty"), "expected empty-array error, got: {err}");
}

#[test]
fn validate_workflow_run_multiple_rejects_empty_task_id() {
    let runs = vec![BulkWorkflowRunItem { task_id: "".to_string(), workflow_ref: None, input_json: None }];
    let err = validate_workflow_run_multiple_input("ao.workflow.run-multiple", &runs).unwrap_err();
    assert!(err.contains("task_id must not be empty"), "expected empty-task-id error, got: {err}");
}

#[test]
fn validate_workflow_run_multiple_accepts_valid_runs() {
    let runs = vec![
        BulkWorkflowRunItem { task_id: "TASK-1".to_string(), workflow_ref: None, input_json: None },
        BulkWorkflowRunItem { task_id: "TASK-2".to_string(), workflow_ref: Some("p1".to_string()), input_json: None },
    ];
    assert!(validate_workflow_run_multiple_input("ao.workflow.run-multiple", &runs).is_ok());
}

#[test]
fn on_error_default_is_stop() {
    let on_error = OnError::default();
    assert_eq!(on_error, OnError::Stop);
    assert_eq!(on_error.as_str(), "stop");
}

#[test]
fn on_error_continue_as_str() {
    assert_eq!(OnError::Continue.as_str(), "continue");
}

#[test]
fn validate_bulk_update_input_rejects_over_max() {
    let updates: Vec<BulkTaskUpdateItem> = (0..=MAX_BATCH_SIZE)
        .map(|i| BulkTaskUpdateItem {
            id: format!("TASK-{i}"),
            title: Some("T".to_string()),
            description: None,
            priority: None,
            status: None,
            assignee: None,
            input_json: None,
        })
        .collect();
    let err = validate_bulk_update_input("ao.task.bulk-update", &updates).unwrap_err();
    assert!(err.contains("exceeds maximum"), "expected max-size error, got: {err}");
}

#[test]
fn validate_workflow_run_multiple_rejects_over_max() {
    let runs: Vec<BulkWorkflowRunItem> = (0..=MAX_BATCH_SIZE)
        .map(|i| BulkWorkflowRunItem { task_id: format!("TASK-{i}"), workflow_ref: None, input_json: None })
        .collect();
    let err = validate_workflow_run_multiple_input("ao.workflow.run-multiple", &runs).unwrap_err();
    assert!(err.contains("exceeds maximum"), "expected max-size error, got: {err}");
}

#[test]
fn list_limit_defaults_and_clamps() {
    assert_eq!(list_limit(None), DEFAULT_MCP_LIST_LIMIT);
    assert_eq!(list_limit(Some(0)), 1);
    assert_eq!(list_limit(Some(MAX_MCP_LIST_LIMIT + 10)), MAX_MCP_LIST_LIMIT);
}

#[test]
fn list_max_tokens_defaults_and_clamps() {
    assert_eq!(list_max_tokens(None), DEFAULT_MCP_LIST_MAX_TOKENS);
    assert_eq!(list_max_tokens(Some(0)), MIN_MCP_LIST_MAX_TOKENS);
    assert_eq!(list_max_tokens(Some(MAX_MCP_LIST_MAX_TOKENS + 500)), MAX_MCP_LIST_MAX_TOKENS);
}

#[test]
fn build_guarded_list_result_normalizes_limit_and_max_tokens_hint() {
    let data = json!([
        { "id": "TASK-1", "status": "todo" },
        { "id": "TASK-2", "status": "done" }
    ]);
    let result = build_guarded_list_result(
        "ao.task.list",
        data,
        ListGuardInput { limit: Some(0), offset: Some(0), max_tokens: Some(0) },
    )
    .expect("guarded list should build");

    assert_eq!(result.pointer("/pagination/limit").and_then(Value::as_u64), Some(1));
    assert_eq!(result.pointer("/pagination/returned").and_then(Value::as_u64), Some(1));
    assert_eq!(
        result.pointer("/size_guard/max_tokens_hint").and_then(Value::as_u64),
        Some(MIN_MCP_LIST_MAX_TOKENS as u64)
    );
}

#[test]
fn build_guarded_list_result_handles_offset_beyond_total() {
    let data = json!([
        { "id": "TASK-1", "status": "todo" },
        { "id": "TASK-2", "status": "done" }
    ]);
    let result = build_guarded_list_result(
        "ao.task.list",
        data,
        ListGuardInput { limit: Some(5), offset: Some(99), max_tokens: Some(3000) },
    )
    .expect("guarded list should build");

    assert_eq!(result.get("items").and_then(Value::as_array).map(Vec::len), Some(0));
    assert_eq!(result.pointer("/pagination/offset").and_then(Value::as_u64), Some(2));
    assert_eq!(result.pointer("/pagination/returned").and_then(Value::as_u64), Some(0));
    assert_eq!(result.pointer("/pagination/total").and_then(Value::as_u64), Some(2));
    assert_eq!(result.pointer("/pagination/has_more").and_then(Value::as_bool), Some(false));
    assert!(
        result.pointer("/pagination/next_offset").map(Value::is_null).unwrap_or(false),
        "next_offset should be null when page is exhausted"
    );
}

#[test]
fn build_guarded_list_result_applies_offset_then_limit() {
    let data = json!([
        { "id": "TASK-1", "status": "todo" },
        { "id": "TASK-2", "status": "in-progress" },
        { "id": "TASK-3", "status": "blocked" },
        { "id": "TASK-4", "status": "done" }
    ]);
    let result = build_guarded_list_result(
        "ao.task.list",
        data,
        ListGuardInput { limit: Some(2), offset: Some(1), max_tokens: Some(3000) },
    )
    .expect("guarded list should build");

    assert_eq!(result.get("schema").and_then(Value::as_str), Some(MCP_LIST_RESULT_SCHEMA));
    assert_eq!(result.get("tool").and_then(Value::as_str), Some("ao.task.list"));
    let items = result.get("items").and_then(Value::as_array).expect("items should be an array");
    assert_eq!(items.len(), 2);
    assert_eq!(items[0].get("id").and_then(Value::as_str), Some("TASK-2"));
    assert_eq!(items[1].get("id").and_then(Value::as_str), Some("TASK-3"));

    let pagination = result.get("pagination").and_then(Value::as_object).expect("pagination should be object");
    assert_eq!(pagination.get("limit").and_then(Value::as_u64), Some(2));
    assert_eq!(pagination.get("offset").and_then(Value::as_u64), Some(1));
    assert_eq!(pagination.get("returned").and_then(Value::as_u64), Some(2));
    assert_eq!(pagination.get("total").and_then(Value::as_u64), Some(4));
    assert_eq!(pagination.get("has_more").and_then(Value::as_bool), Some(true));
    assert_eq!(pagination.get("next_offset").and_then(Value::as_u64), Some(3));

    let size_guard = result.get("size_guard").and_then(Value::as_object).expect("size_guard should be object");
    assert_eq!(size_guard.get("mode").and_then(Value::as_str), Some("full"));
    assert_eq!(size_guard.get("truncated").and_then(Value::as_bool), Some(false));
}

#[test]
fn build_guarded_list_result_falls_back_to_summary_fields_mode() {
    let data = json!([{
        "id": "wf-1",
        "task_id": "TASK-077",
        "status": "running",
        "workflow_ref": "default",
        "decision_history": "x".repeat(8000),
        "raw_state": { "huge_blob": "y".repeat(4000) }
    }]);

    let result = build_guarded_list_result(
        "ao.workflow.list",
        data,
        ListGuardInput { limit: Some(25), offset: Some(0), max_tokens: Some(256) },
    )
    .expect("guarded list should build");

    assert_eq!(result.pointer("/size_guard/mode").and_then(Value::as_str).expect("size guard mode"), "summary_fields");
    assert_eq!(result.pointer("/size_guard/truncated").and_then(Value::as_bool), Some(true));
    let item = result.pointer("/items/0").and_then(Value::as_object).expect("summary field item should be object");
    assert_eq!(item.get("id").and_then(Value::as_str), Some("wf-1"));
    assert!(item.get("decision_history").is_none());
    assert!(item.get("raw_state").is_none());
}

#[test]
fn build_guarded_list_result_falls_back_to_summary_only_mode() {
    let items: Vec<Value> = (0..25)
        .map(|idx| {
            json!({
                "id": format!("TASK-{idx:03}"),
                "title": "x".repeat(120),
                "status": "in-progress",
                "details": "y".repeat(500)
            })
        })
        .collect();

    let result = build_guarded_list_result(
        "ao.task.list",
        Value::Array(items),
        ListGuardInput { limit: Some(25), offset: Some(0), max_tokens: Some(256) },
    )
    .expect("guarded list should build");

    assert_eq!(result.pointer("/size_guard/mode").and_then(Value::as_str).expect("size guard mode"), "summary_only");
    let items = result.get("items").and_then(Value::as_array).expect("summary-only items should be array");
    assert_eq!(items.len(), 1);
    let digest = items[0].as_object().expect("digest should be object");
    assert_eq!(digest.get("kind").and_then(Value::as_str), Some("summary_only"));
    assert_eq!(digest.get("item_count").and_then(Value::as_u64), Some(25));
    assert!(digest.get("ids").and_then(Value::as_array).map(|ids| ids.len() <= 10).unwrap_or(false));
}

#[test]
fn build_guarded_list_result_summary_only_respects_max_tokens_hint() {
    let items: Vec<Value> = (0..MAX_MCP_LIST_LIMIT)
        .map(|idx| {
            json!({
                "id": format!("TASK-{idx:03}"),
                "status": format!("{idx:03}-{}", "s".repeat(48)),
                "details": "y".repeat(1200),
            })
        })
        .collect();

    let result = build_guarded_list_result(
        "ao.task.list",
        Value::Array(items),
        ListGuardInput { limit: Some(MAX_MCP_LIST_LIMIT), offset: Some(0), max_tokens: Some(MIN_MCP_LIST_MAX_TOKENS) },
    )
    .expect("guarded list should build");

    assert_eq!(result.pointer("/size_guard/mode").and_then(Value::as_str).expect("size guard mode"), "summary_only");
    assert!(
        result
            .pointer("/size_guard/estimated_tokens")
            .and_then(Value::as_u64)
            .map(|tokens| tokens <= MIN_MCP_LIST_MAX_TOKENS as u64)
            .unwrap_or(false),
        "summary-only payload should stay within max_tokens hint"
    );
    assert!(
        result
            .pointer("/items/0/omitted_status_item_count")
            .and_then(Value::as_u64)
            .map(|count| count > 0)
            .unwrap_or(false),
        "summary-only payload should drop status buckets when needed"
    );
}

#[test]
fn build_guarded_list_result_supports_workflow_decisions() {
    let result = build_guarded_list_result(
        "ao.workflow.decisions",
        json!([{
            "timestamp": "2026-02-27T12:00:00Z",
            "phase_id": "code-review",
            "source": "llm",
            "decision": "advance",
            "reason": "ok",
            "confidence": 0.9,
            "risk": "low"
        }]),
        ListGuardInput { limit: Some(10), offset: Some(0), max_tokens: Some(3000) },
    )
    .expect("workflow decisions should support guarded list responses");

    assert_eq!(result.get("tool").and_then(Value::as_str), Some("ao.workflow.decisions"));
    assert_eq!(result.pointer("/pagination/returned").and_then(Value::as_u64), Some(1));
}

#[test]
fn build_workflow_list_args_includes_filters_and_sort() {
    let args = build_workflow_list_args(&WorkflowListInput {
        status: Some("running".to_string()),
        workflow_ref: Some("default".to_string()),
        task_id: Some("TASK-123".to_string()),
        phase_id: Some("implementation".to_string()),
        search: Some("retry".to_string()),
        sort: Some("started_at".to_string()),
        limit: Some(10),
        offset: Some(2),
        max_tokens: Some(4000),
        project_root: None,
    });
    assert_eq!(
        args,
        vec![
            "workflow".to_string(),
            "list".to_string(),
            "--status".to_string(),
            "running".to_string(),
            "--workflow-ref".to_string(),
            "default".to_string(),
            "--task-id".to_string(),
            "TASK-123".to_string(),
            "--phase-id".to_string(),
            "implementation".to_string(),
            "--search".to_string(),
            "retry".to_string(),
            "--sort".to_string(),
            "started_at".to_string(),
        ]
    );
}

#[test]
fn build_guarded_list_result_rejects_non_array_payloads() {
    let err = build_guarded_list_result(
        "ao.workflow.list",
        json!({"id": "wf-1"}),
        ListGuardInput { limit: None, offset: None, max_tokens: None },
    )
    .expect_err("non-array list payload should fail");
    assert!(err.to_string().contains("expected list data as JSON array"));
}

#[test]
fn build_daemon_start_args_defaults_minimal() {
    let input = DaemonStartInput::default();
    let args = build_daemon_start_args(&input);
    assert_eq!(args, vec!["daemon".to_string(), "start".to_string()]);
}

#[test]
fn build_daemon_start_args_with_flags() {
    let input = DaemonStartInput {
        pool_size: Some(4),
        skip_runner: Some(true),
        auto_run_ready: Some(true),
        runner_scope: Some("project".to_string()),
        ..Default::default()
    };
    let args = build_daemon_start_args(&input);
    assert_eq!(
        args,
        vec![
            "daemon".to_string(),
            "start".to_string(),
            "--pool-size".to_string(),
            "4".to_string(),
            "--skip-runner".to_string(),
            "--auto-run-ready".to_string(),
            "true".to_string(),
            "--runner-scope".to_string(),
            "project".to_string(),
        ]
    );
}

#[test]
fn build_daemon_start_args_includes_stale_threshold_hours() {
    let input = DaemonStartInput { stale_threshold_hours: Some(48), ..Default::default() };
    let args = build_daemon_start_args(&input);
    assert_eq!(
        args,
        vec!["daemon".to_string(), "start".to_string(), "--stale-threshold-hours".to_string(), "48".to_string(),]
    );
}

#[test]
fn build_daemon_config_set_args_defaults_minimal() {
    let input = DaemonConfigSetInput::default();
    let args = build_daemon_config_set_args(&input);
    assert_eq!(args, vec!["daemon".to_string(), "config".to_string()]);
}

#[test]
fn build_daemon_config_set_args_wires_pool_size() {
    let input = DaemonConfigSetInput { pool_size: Some(8), ..Default::default() };
    let args = build_daemon_config_set_args(&input);
    assert_eq!(
        args,
        vec!["daemon", "config", "--pool-size", "8"]
            .into_iter()
            .map(String::from)
            .collect::<Vec<_>>()
    );
}

#[test]
fn build_daemon_config_set_args_wires_interval_secs() {
    let input = DaemonConfigSetInput { interval_secs: Some(15), ..Default::default() };
    let args = build_daemon_config_set_args(&input);
    assert_eq!(
        args,
        vec!["daemon", "config", "--interval-secs", "15"]
            .into_iter()
            .map(String::from)
            .collect::<Vec<_>>()
    );
}

#[test]
fn build_daemon_config_set_args_wires_max_tasks_per_tick() {
    let input = DaemonConfigSetInput { max_tasks_per_tick: Some(10), ..Default::default() };
    let args = build_daemon_config_set_args(&input);
    assert_eq!(
        args,
        vec!["daemon", "config", "--max-tasks-per-tick", "10"]
            .into_iter()
            .map(String::from)
            .collect::<Vec<_>>()
    );
}

#[test]
fn build_daemon_config_set_args_wires_all_runtime_settings() {
    let input = DaemonConfigSetInput {
        auto_merge: Some(true),
        auto_pr: Some(false),
        auto_run_ready: Some(false),
        pool_size: Some(4),
        interval_secs: Some(10),
        max_tasks_per_tick: Some(5),
        stale_threshold_hours: Some(48),
        phase_timeout_secs: Some(300),
        idle_timeout_secs: Some(600),
        ..Default::default()
    };
    let args = build_daemon_config_set_args(&input);
    assert!(args.contains(&"--pool-size".to_string()));
    assert!(args.contains(&"4".to_string()));
    assert!(args.contains(&"--interval-secs".to_string()));
    assert!(args.contains(&"10".to_string()));
    assert!(args.contains(&"--max-tasks-per-tick".to_string()));
    assert!(args.contains(&"5".to_string()));
    assert!(args.contains(&"--auto-run-ready".to_string()));
    assert!(args.contains(&"false".to_string()));
    assert!(args.contains(&"--stale-threshold-hours".to_string()));
    assert!(args.contains(&"48".to_string()));
    assert!(args.contains(&"--phase-timeout-secs".to_string()));
    assert!(args.contains(&"300".to_string()));
    assert!(args.contains(&"--idle-timeout-secs".to_string()));
    assert!(args.contains(&"600".to_string()));
    assert!(args.contains(&"--auto-merge".to_string()));
    assert!(args.contains(&"true".to_string()));
    assert!(args.contains(&"--auto-pr".to_string()));
}

#[test]
fn build_queue_enqueue_args_includes_optional_fields() {
    let input = QueueEnqueueInput {
        task_id: Some("TASK-123".to_string()),
        requirement_id: None,
        title: None,
        description: None,
        workflow_ref: Some("ops".to_string()),
        input_json: Some("{\"mode\":\"fast\"}".to_string()),
        project_root: None,
    };
    let args = build_queue_enqueue_args(&input);
    assert_eq!(
        args,
        vec![
            "queue".to_string(),
            "enqueue".to_string(),
            "--task-id".to_string(),
            "TASK-123".to_string(),
            "--workflow-ref".to_string(),
            "ops".to_string(),
            "--input-json".to_string(),
            "{\"mode\":\"fast\"}".to_string(),
        ]
    );
}

#[test]
fn build_queue_reorder_args_repeats_subject_flags() {
    let input = QueueReorderInput { subject_ids: vec!["TASK-2".to_string(), "TASK-1".to_string()], project_root: None };
    let args = build_queue_reorder_args(&input);
    assert_eq!(
        args,
        vec![
            "queue".to_string(),
            "reorder".to_string(),
            "--subject-id".to_string(),
            "TASK-2".to_string(),
            "--subject-id".to_string(),
            "TASK-1".to_string(),
        ]
    );
}

#[test]
fn build_agent_run_args_defaults_detach_and_stream() {
    let input = AgentRunInput {
        tool: "codex".to_string(),
        model: Some("codex".to_string()),
        prompt: None,
        cwd: None,
        timeout_secs: None,
        context_json: None,
        runtime_contract_json: None,
        detach: true,
        run_id: None,
        runner_scope: None,
        project_root: None,
    };
    let args = build_agent_run_args(&input);
    assert_eq!(
        args,
        vec![
            "agent".to_string(),
            "run".to_string(),
            "--tool".to_string(),
            "codex".to_string(),
            "--stream".to_string(),
            "false".to_string(),
            "--model".to_string(),
            "codex".to_string(),
            "--detach".to_string(),
        ]
    );
}

#[test]
fn build_agent_run_args_with_all_options() {
    let input = AgentRunInput {
        tool: "claude".to_string(),
        model: Some("opus".to_string()),
        prompt: Some("hello".to_string()),
        cwd: Some("/tmp".to_string()),
        timeout_secs: Some(300),
        context_json: Some("{}".to_string()),
        runtime_contract_json: Some("{\"k\":1}".to_string()),
        detach: false,
        run_id: Some("run-1".to_string()),
        runner_scope: Some("global".to_string()),
        project_root: None,
    };
    let args = build_agent_run_args(&input);
    assert_eq!(
        args,
        vec![
            "agent".to_string(),
            "run".to_string(),
            "--tool".to_string(),
            "claude".to_string(),
            "--stream".to_string(),
            "false".to_string(),
            "--model".to_string(),
            "opus".to_string(),
            "--prompt".to_string(),
            "hello".to_string(),
            "--cwd".to_string(),
            "/tmp".to_string(),
            "--timeout-secs".to_string(),
            "300".to_string(),
            "--context-json".to_string(),
            "{}".to_string(),
            "--runtime-contract-json".to_string(),
            "{\"k\":1}".to_string(),
            "--run-id".to_string(),
            "run-1".to_string(),
            "--runner-scope".to_string(),
            "global".to_string(),
        ]
    );
}

#[test]
fn daemon_events_poll_limit_defaults_and_clamps() {
    assert_eq!(daemon_events_poll_limit(None), DEFAULT_DAEMON_EVENTS_LIMIT);
    assert_eq!(daemon_events_poll_limit(Some(0)), 1);
    assert_eq!(daemon_events_poll_limit(Some(MAX_DAEMON_EVENTS_LIMIT + 25)), MAX_DAEMON_EVENTS_LIMIT);
}

#[test]
fn resolve_daemon_events_project_root_uses_default_when_override_blank() {
    let default_root = TempDir::new().expect("default project root");
    let expected = crate::services::runtime::canonicalize_lossy(default_root.path().to_string_lossy().as_ref());
    assert_eq!(resolve_daemon_events_project_root(expected.as_str(), Some("   ".to_string())), expected);
}

#[test]
fn build_daemon_events_poll_result_returns_non_null_structured_events() {
    let _lock = crate::shared::test_env_lock().lock().expect("env lock should be available");
    let config_root = TempDir::new().expect("config temp dir");
    let _config_guard = EnvVarGuard::set("AO_CONFIG_DIR", Some(config_root.path().to_string_lossy().as_ref()));
    let _legacy_guard = EnvVarGuard::set("AGENT_ORCHESTRATOR_CONFIG_DIR", None);

    let project = TempDir::new().expect("project temp dir");
    let project_root = project.path().to_string_lossy().to_string();
    write_events(&[
        serde_json::to_string(&sample_event(1, "queue", project_root.as_str())).expect("event json"),
        "{not-json".to_string(),
        serde_json::to_string(&sample_event(2, "workflow", project_root.as_str())).expect("event json"),
    ]);

    let result = build_daemon_events_poll_result(
        project_root.as_str(),
        DaemonEventsInput { limit: Some(10), project_root: Some(project_root.clone()) },
    )
    .expect("poll result should be built");

    assert_eq!(result.get("schema").and_then(Value::as_str), Some("ao.daemon.events.poll.v1"));
    assert_eq!(result.get("count").and_then(Value::as_u64), Some(2));
    let events = result.get("events").and_then(Value::as_array).expect("events should be an array");
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].get("seq").and_then(Value::as_u64), Some(1));
    assert_eq!(events[1].get("seq").and_then(Value::as_u64), Some(2));
}

#[test]
fn build_daemon_events_poll_result_filters_by_project_root() {
    let _lock = crate::shared::test_env_lock().lock().expect("env lock should be available");
    let config_root = TempDir::new().expect("config temp dir");
    let _config_guard = EnvVarGuard::set("AO_CONFIG_DIR", Some(config_root.path().to_string_lossy().as_ref()));
    let _legacy_guard = EnvVarGuard::set("AGENT_ORCHESTRATOR_CONFIG_DIR", None);

    let project_a = TempDir::new().expect("project A");
    let project_b = TempDir::new().expect("project B");
    let root_a = project_a.path().to_string_lossy().to_string();
    let root_b = project_b.path().to_string_lossy().to_string();
    write_events(&[
        serde_json::to_string(&sample_event(1, "queue", root_a.as_str())).expect("event json"),
        serde_json::to_string(&sample_event(2, "queue", root_b.as_str())).expect("event json"),
        serde_json::to_string(&sample_event(3, "log", root_a.as_str())).expect("event json"),
    ]);

    let result = build_daemon_events_poll_result(
        root_a.as_str(),
        DaemonEventsInput { limit: Some(50), project_root: Some(root_a.clone()) },
    )
    .expect("poll result should be built");
    let events = result.get("events").and_then(Value::as_array).expect("events should be an array");
    assert_eq!(events.len(), 2);
    assert!(events.iter().all(|event| { event.get("project_root").and_then(Value::as_str) == Some(root_a.as_str()) }));
    assert_eq!(events[0].get("seq").and_then(Value::as_u64), Some(1));
    assert_eq!(events[1].get("seq").and_then(Value::as_u64), Some(3));
}

#[test]
fn build_daemon_events_poll_result_blank_project_root_falls_back_to_default() {
    let _lock = crate::shared::test_env_lock().lock().expect("env lock should be available");
    let config_root = TempDir::new().expect("config temp dir");
    let _config_guard = EnvVarGuard::set("AO_CONFIG_DIR", Some(config_root.path().to_string_lossy().as_ref()));
    let _legacy_guard = EnvVarGuard::set("AGENT_ORCHESTRATOR_CONFIG_DIR", None);

    let project_a = TempDir::new().expect("project A");
    let project_b = TempDir::new().expect("project B");
    let root_a = crate::services::runtime::canonicalize_lossy(project_a.path().to_string_lossy().as_ref());
    let root_b = crate::services::runtime::canonicalize_lossy(project_b.path().to_string_lossy().as_ref());
    write_events(&[
        serde_json::to_string(&sample_event(1, "queue", root_a.as_str())).expect("event json"),
        serde_json::to_string(&sample_event(2, "queue", root_b.as_str())).expect("event json"),
        serde_json::to_string(&sample_event(3, "log", root_a.as_str())).expect("event json"),
    ]);

    let result = build_daemon_events_poll_result(
        root_a.as_str(),
        DaemonEventsInput { limit: Some(50), project_root: Some("   ".to_string()) },
    )
    .expect("poll result should be built");
    assert_eq!(result.get("project_root").and_then(Value::as_str), Some(root_a.as_str()));
    let events = result.get("events").and_then(Value::as_array).expect("events should be an array");
    assert_eq!(events.len(), 2);
    assert!(events.iter().all(|event| { event.get("project_root").and_then(Value::as_str) == Some(root_a.as_str()) }));
}

#[test]
fn build_output_tail_result_requires_exactly_one_identifier() {
    let err_none = build_output_tail_result(
        "/tmp/project",
        OutputTailInput { run_id: None, task_id: None, limit: None, event_types: None, project_root: None },
    )
    .expect_err("missing identifiers should fail");
    assert!(err_none.to_string().contains("exactly one"));

    let err_both = build_output_tail_result(
        "/tmp/project",
        OutputTailInput {
            run_id: Some("run-1".to_string()),
            task_id: Some("TASK-1".to_string()),
            limit: None,
            event_types: None,
            project_root: None,
        },
    )
    .expect_err("multiple identifiers should fail");
    assert!(err_both.to_string().contains("exactly one"));
}

#[test]
fn build_output_tail_result_rejects_invalid_event_type() {
    let err = build_output_tail_result(
        "/tmp/project",
        OutputTailInput {
            run_id: Some("run-1".to_string()),
            task_id: None,
            limit: None,
            event_types: Some(vec!["unknown".to_string()]),
            project_root: None,
        },
    )
    .expect_err("unknown filter should fail");
    assert!(err.to_string().contains("invalid event type"));
}

#[test]
fn build_output_tail_result_rejects_unsafe_run_id() {
    let err = build_output_tail_result(
        "/tmp/project",
        OutputTailInput {
            run_id: Some("../escape".to_string()),
            task_id: None,
            limit: None,
            event_types: None,
            project_root: None,
        },
    )
    .expect_err("unsafe run id should fail");
    assert!(err.to_string().contains("invalid run_id"));
}

#[test]
fn build_output_tail_result_filters_out_events_for_other_runs() {
    let _lock = crate::shared::test_env_lock().lock().expect("env lock should be available");
    let temp = TempDir::new().expect("tempdir should be created");
    let _home_guard = EnvVarGuard::set("HOME", Some(temp.path().to_string_lossy().as_ref()));
    let project_root = temp.path().join("project");
    std::fs::create_dir_all(&project_root).expect("project dir should exist");
    let root = project_root.to_string_lossy().to_string();
    let run_id = "wf-filter-run-match-phase-0-d4";
    let other_run = "wf-filter-run-other-phase-0-e5";
    write_run_events(
        root.as_str(),
        run_id,
        &[
            output_event(run_id, "keep-output"),
            output_event(other_run, "drop-output"),
            thinking_event(other_run, "drop-thinking"),
            thinking_event(run_id, "keep-thinking"),
            error_event(run_id, "keep-error"),
        ],
    );

    let result = build_output_tail_result(
        root.as_str(),
        OutputTailInput {
            run_id: Some(run_id.to_string()),
            task_id: None,
            limit: Some(10),
            event_types: Some(vec!["output".to_string(), "thinking".to_string(), "error".to_string()]),
            project_root: None,
        },
    )
    .expect("tail result should build");

    assert_eq!(result.get("count").and_then(Value::as_u64), Some(3));
    let events = result.get("events").and_then(Value::as_array).expect("events should be an array");
    assert_eq!(events.len(), 3);
    assert_eq!(events[0].get("text").and_then(Value::as_str), Some("keep-output"));
    assert_eq!(events[1].get("text").and_then(Value::as_str), Some("keep-thinking"));
    assert_eq!(events[2].get("text").and_then(Value::as_str), Some("keep-error"));
}

#[test]
fn build_output_tail_result_returns_empty_when_events_log_missing() {
    let _lock = crate::shared::test_env_lock().lock().expect("env lock should be available");
    let temp = TempDir::new().expect("tempdir should be created");
    let _home_guard = EnvVarGuard::set("HOME", Some(temp.path().to_string_lossy().as_ref()));
    let project_root = temp.path().join("project");
    std::fs::create_dir_all(&project_root).expect("project dir should exist");
    let root = project_root.to_string_lossy().to_string();
    let run_id = "wf-missing-events-phase-0-f6";
    let run_path = run_dir(root.as_str(), &RunId(run_id.to_string()), None);
    std::fs::create_dir_all(&run_path).expect("run directory should exist");

    let result = build_output_tail_result(
        root.as_str(),
        OutputTailInput {
            run_id: Some(run_id.to_string()),
            task_id: None,
            limit: Some(10),
            event_types: None,
            project_root: None,
        },
    )
    .expect("tail result should build");

    assert_eq!(result.get("count").and_then(Value::as_u64), Some(0));
    assert_eq!(result.get("events").and_then(Value::as_array).map(Vec::len), Some(0));
}

#[test]
fn build_output_tail_result_skips_invalid_utf8_log_lines() {
    let _lock = crate::shared::test_env_lock().lock().expect("env lock should be available");
    let temp = TempDir::new().expect("tempdir should be created");
    let _home_guard = EnvVarGuard::set("HOME", Some(temp.path().to_string_lossy().as_ref()));
    let project_root = temp.path().join("project");
    std::fs::create_dir_all(&project_root).expect("project dir should exist");
    let root = project_root.to_string_lossy().to_string();
    let run_id = "wf-invalid-utf8-phase-0-g7";
    let run_path = run_dir(root.as_str(), &RunId(run_id.to_string()), None);
    std::fs::create_dir_all(&run_path).expect("run directory should be created");
    let mut payload = Vec::new();
    payload.extend_from_slice(output_event(run_id, "visible-output").as_bytes());
    payload.push(b'\n');
    payload.extend_from_slice(&[0xff, 0xfe, b'\n']);
    payload.extend_from_slice(thinking_event(run_id, "visible-thinking").as_bytes());
    payload.push(b'\n');
    std::fs::write(run_path.join("events.jsonl"), payload).expect("events should be written");

    let result = build_output_tail_result(
        root.as_str(),
        OutputTailInput {
            run_id: Some(run_id.to_string()),
            task_id: None,
            limit: Some(10),
            event_types: None,
            project_root: None,
        },
    )
    .expect("tail result should build");

    assert_eq!(result.get("count").and_then(Value::as_u64), Some(2));
    let events = result.get("events").and_then(Value::as_array).expect("events should be an array");
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].get("text").and_then(Value::as_str), Some("visible-output"));
    assert_eq!(events[1].get("text").and_then(Value::as_str), Some("visible-thinking"));
}

#[test]
fn build_output_tail_result_defaults_to_output_and_thinking() {
    let _lock = crate::shared::test_env_lock().lock().expect("env lock should be available");
    let temp = TempDir::new().expect("tempdir should be created");
    let _home_guard = EnvVarGuard::set("HOME", Some(temp.path().to_string_lossy().as_ref()));
    let project_root = temp.path().join("project");
    std::fs::create_dir_all(&project_root).expect("project dir should exist");
    let root = project_root.to_string_lossy().to_string();
    let run_id = "wf-default-filter-phase-0-a1";
    write_run_events(
        root.as_str(),
        run_id,
        &[
            output_event(run_id, "first output"),
            "{malformed".to_string(),
            error_event(run_id, "ignored error"),
            thinking_event(run_id, "visible thought"),
        ],
    );

    let result = build_output_tail_result(
        root.as_str(),
        OutputTailInput {
            run_id: Some(run_id.to_string()),
            task_id: None,
            limit: None,
            event_types: None,
            project_root: None,
        },
    )
    .expect("tail result should build");

    assert_eq!(result.get("schema").and_then(Value::as_str), Some(OUTPUT_TAIL_SCHEMA));
    assert_eq!(result.get("resolved_from").and_then(Value::as_str), Some("run_id"));
    assert_eq!(result.get("limit").and_then(Value::as_u64), Some(50));
    assert_eq!(result.get("count").and_then(Value::as_u64), Some(2));
    let events = result.get("events").and_then(Value::as_array).expect("events should be an array");
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].get("event_type").and_then(Value::as_str), Some("output"));
    assert_eq!(events[0].get("text").and_then(Value::as_str), Some("first output"));
    assert_eq!(events[1].get("event_type").and_then(Value::as_str), Some("thinking"));
    assert_eq!(events[1].get("text").and_then(Value::as_str), Some("visible thought"));
}

#[test]
fn build_output_tail_result_normalizes_output_stream_types() {
    let _lock = crate::shared::test_env_lock().lock().expect("env lock should be available");
    let temp = TempDir::new().expect("tempdir should be created");
    let _home_guard = EnvVarGuard::set("HOME", Some(temp.path().to_string_lossy().as_ref()));
    let project_root = temp.path().join("project");
    std::fs::create_dir_all(&project_root).expect("project dir should exist");
    let root = project_root.to_string_lossy().to_string();
    let run_id = "wf-stream-types-phase-0-s9";
    write_run_events(
        root.as_str(),
        run_id,
        &[
            output_event_with_stream(run_id, "stdout line", protocol::OutputStreamType::Stdout),
            output_event_with_stream(run_id, "stderr line", protocol::OutputStreamType::Stderr),
            output_event_with_stream(run_id, "system line", protocol::OutputStreamType::System),
        ],
    );

    let result = build_output_tail_result(
        root.as_str(),
        OutputTailInput {
            run_id: Some(run_id.to_string()),
            task_id: None,
            limit: Some(10),
            event_types: Some(vec!["output".to_string()]),
            project_root: None,
        },
    )
    .expect("tail result should build");

    assert_eq!(result.get("count").and_then(Value::as_u64), Some(3));
    let events = result.get("events").and_then(Value::as_array).expect("events should be an array");
    assert_eq!(events.len(), 3);
    assert_eq!(events[0].get("stream_type").and_then(Value::as_str), Some("stdout"));
    assert_eq!(events[1].get("stream_type").and_then(Value::as_str), Some("stderr"));
    assert_eq!(events[2].get("stream_type").and_then(Value::as_str), Some("system"));
}

#[test]
fn build_output_tail_result_applies_filter_and_limit_in_order() {
    let _lock = crate::shared::test_env_lock().lock().expect("env lock should be available");
    let temp = TempDir::new().expect("tempdir should be created");
    let _home_guard = EnvVarGuard::set("HOME", Some(temp.path().to_string_lossy().as_ref()));
    let project_root = temp.path().join("project");
    std::fs::create_dir_all(&project_root).expect("project dir should exist");
    let root = project_root.to_string_lossy().to_string();
    let run_id = "wf-limit-filter-phase-0-b2";
    write_run_events(
        root.as_str(),
        run_id,
        &[
            output_event(run_id, "out-1"),
            thinking_event(run_id, "think-1"),
            output_event(run_id, "out-2"),
            error_event(run_id, "err-1"),
        ],
    );

    let result = build_output_tail_result(
        root.as_str(),
        OutputTailInput {
            run_id: Some(run_id.to_string()),
            task_id: None,
            limit: Some(2),
            event_types: Some(vec!["output".to_string(), "thinking".to_string(), "error".to_string()]),
            project_root: None,
        },
    )
    .expect("tail result should build");

    assert_eq!(result.get("count").and_then(Value::as_u64), Some(2));
    let events = result.get("events").and_then(Value::as_array).expect("events should be an array");
    assert_eq!(events[0].get("text").and_then(Value::as_str), Some("out-2"));
    assert_eq!(events[1].get("text").and_then(Value::as_str), Some("err-1"));
    assert_eq!(events[1].get("event_type").and_then(Value::as_str), Some("error"));
}

#[test]
fn build_output_tail_result_clamps_limit_to_minimum() {
    let _lock = crate::shared::test_env_lock().lock().expect("env lock should be available");
    let temp = TempDir::new().expect("tempdir should be created");
    let _home_guard = EnvVarGuard::set("HOME", Some(temp.path().to_string_lossy().as_ref()));
    let project_root = temp.path().join("project");
    std::fs::create_dir_all(&project_root).expect("project dir should exist");
    let root = project_root.to_string_lossy().to_string();
    let run_id = "wf-limit-min-phase-0-c3";
    write_run_events(root.as_str(), run_id, &[error_event(run_id, "first"), error_event(run_id, "second")]);

    let result = build_output_tail_result(
        root.as_str(),
        OutputTailInput {
            run_id: Some(run_id.to_string()),
            task_id: None,
            limit: Some(0),
            event_types: Some(vec!["error".to_string()]),
            project_root: None,
        },
    )
    .expect("tail result should build");

    assert_eq!(result.get("limit").and_then(Value::as_u64), Some(1));
    assert_eq!(result.get("count").and_then(Value::as_u64), Some(1));
    let events = result.get("events").and_then(Value::as_array).expect("events should be an array");
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].get("text").and_then(Value::as_str), Some("second"));
}

#[test]
fn build_output_tail_result_resolves_task_to_running_workflow_run() {
    let _lock = crate::shared::test_env_lock().lock().expect("env lock should be available");
    let temp = TempDir::new().expect("tempdir should be created");
    let _home_guard = EnvVarGuard::set("HOME", Some(temp.path().to_string_lossy().as_ref()));
    let project_root = temp.path().join("project");
    std::fs::create_dir_all(&project_root).expect("project dir should exist");
    let root = project_root.to_string_lossy().to_string();
    let now = Utc::now();

    save_workflow(
        root.as_str(),
        "wf-completed",
        "TASK-043",
        WorkflowStatus::Completed,
        now - Duration::minutes(20),
        Some(now - Duration::minutes(10)),
    );
    save_workflow(root.as_str(), "wf-running", "TASK-043", WorkflowStatus::Running, now - Duration::minutes(1), None);

    let completed_run = "wf-wf-completed-implementation-0-old";
    let running_run = "wf-wf-running-implementation-0-new";
    write_run_events(root.as_str(), completed_run, &[output_event(completed_run, "completed-output")]);
    write_run_events(root.as_str(), running_run, &[output_event(running_run, "running-output")]);

    let result = build_output_tail_result(
        root.as_str(),
        OutputTailInput {
            run_id: None,
            task_id: Some("TASK-043".to_string()),
            limit: Some(10),
            event_types: Some(vec!["output".to_string()]),
            project_root: None,
        },
    )
    .expect("tail result should build");

    assert_eq!(result.get("resolved_from").and_then(Value::as_str), Some("task_id"));
    assert_eq!(result.get("resolved_run_id").and_then(Value::as_str), Some(running_run));
    let events = result.get("events").and_then(Value::as_array).expect("events should be an array");
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].get("text").and_then(Value::as_str), Some("running-output"));
}

#[test]
fn compact_json_str_minifies_json_payloads() {
    let compacted = compact_json_str("{\n  \"a\": 1,\n  \"b\": [1, 2]\n}").expect("json should be compacted");
    assert_eq!(compacted, r#"{"a":1,"b":[1,2]}"#);
}

#[test]
fn compact_json_str_ignores_non_json_text() {
    assert!(compact_json_str("plain text").is_none());
}

#[test]
fn extract_cli_success_data_preserves_nested_json_strings() {
    let data = extract_cli_success_data(Some(json!({
        "schema": CLI_SCHEMA_ID,
        "ok": true,
        "data": {
            "runtime_contract_json": "{\n  \"mcp\": { \"enabled\": true }\n}",
            "label": "unchanged"
        }
    })));

    assert_eq!(
        data.pointer("/runtime_contract_json").and_then(Value::as_str),
        Some("{\n  \"mcp\": { \"enabled\": true }\n}")
    );
    assert_eq!(data.pointer("/label").and_then(Value::as_str), Some("unchanged"));
}

#[test]
fn build_cli_error_payload_preserves_json_like_error_text() {
    let mut result = sample_cli_failure_result();
    result.stdout_json = Some(json!({
        "schema": CLI_SCHEMA_ID,
        "ok": false,
        "error": {
            "message": "{\n  \"detail\": \"keep formatting\"\n}"
        }
    }));

    let payload = build_cli_error_payload("ao.task.get", &result);
    assert_eq!(
        payload.pointer("/error/message").and_then(Value::as_str),
        Some("{\n  \"detail\": \"keep formatting\"\n}")
    );
}
