use super::*;

mod requirement_workflow_tests {
    use super::{execute_workflow, workflow_exit_success, WorkflowExecuteParams};
    use orchestrator_core::{
        load_agent_runtime_config, services::ServiceHub, write_agent_runtime_config, FileServiceHub,
        PhaseExecutionMode, PhaseManualDefinition, Priority, TaskCreateInput, TaskStatus, TaskType, WorkflowRunInput,
        WorkflowStatus,
    };
    use std::collections::HashMap;
    use std::process::Command as ProcessCommand;
    use std::sync::Arc;
    use tempfile::TempDir;

    fn init_git_repo(temp: &TempDir) {
        let init_main = ProcessCommand::new("git")
            .arg("init")
            .arg("-b")
            .arg("main")
            .current_dir(temp.path())
            .status()
            .expect("git init should run");
        if !init_main.success() {
            let init =
                ProcessCommand::new("git").arg("init").current_dir(temp.path()).status().expect("git init should run");
            assert!(init.success(), "git init should succeed");
            let rename = ProcessCommand::new("git")
                .args(["branch", "-M", "main"])
                .current_dir(temp.path())
                .status()
                .expect("git branch -M should run");
            assert!(rename.success(), "git branch -M main should succeed");
        }

        let email = ProcessCommand::new("git")
            .args(["config", "user.email", "ao-test@example.com"])
            .current_dir(temp.path())
            .status()
            .expect("git config user.email should run");
        assert!(email.success(), "git config user.email should succeed");
        let name = ProcessCommand::new("git")
            .args(["config", "user.name", "AO Test"])
            .current_dir(temp.path())
            .status()
            .expect("git config user.name should run");
        assert!(name.success(), "git config user.name should succeed");

        std::fs::write(temp.path().join("README.md"), "# test\n").expect("readme should be written");
        let add = ProcessCommand::new("git")
            .args(["add", "README.md"])
            .current_dir(temp.path())
            .status()
            .expect("git add should run");
        assert!(add.success(), "git add should succeed");
        let commit = ProcessCommand::new("git")
            .args(["commit", "-m", "initial"])
            .current_dir(temp.path())
            .status()
            .expect("git commit should run");
        assert!(commit.success(), "initial commit should succeed");
    }

    #[tokio::test]
    async fn execute_workflow_pauses_manual_pending_workflows() {
        let temp = TempDir::new().expect("temp dir");
        init_git_repo(&temp);
        let project_root = temp.path().to_string_lossy().to_string();
        let hub = Arc::new(FileServiceHub::new(&project_root).expect("file service hub"));

        let task = hub
            .tasks()
            .create(TaskCreateInput {
                title: "manual gate".to_string(),
                description: "waits for approval".to_string(),
                task_type: Some(TaskType::Feature),
                priority: Some(Priority::High),
                created_by: Some("test".to_string()),
                tags: Vec::new(),
                linked_requirements: Vec::new(),
                linked_architecture_entities: Vec::new(),
            })
            .await
            .expect("task should be created");
        hub.tasks().set_status(&task.id, TaskStatus::InProgress, false).await.expect("task should be in progress");

        let workflow = hub
            .workflows()
            .run(WorkflowRunInput::for_task(task.id.clone(), None))
            .await
            .expect("workflow should start");

        let current_phase = workflow.current_phase.clone().expect("workflow should have a current phase");
        let mut runtime = load_agent_runtime_config(temp.path()).expect("runtime config");
        let mut definition = runtime.phase_execution(&current_phase).cloned().expect("current phase should exist");
        definition.mode = PhaseExecutionMode::Manual;
        definition.agent_id = None;
        definition.command = None;
        definition.manual = Some(PhaseManualDefinition {
            instructions: "Wait for approval".to_string(),
            approval_note_required: false,
            timeout_secs: None,
        });
        runtime.phases.insert(current_phase.clone(), definition);
        write_agent_runtime_config(temp.path(), &runtime).expect("runtime config should write");

        let result = execute_workflow(WorkflowExecuteParams {
            project_root: project_root.clone(),
            workflow_id: None,
            task_id: Some(task.id.clone()),
            requirement_id: None,
            title: None,
            description: None,
            workflow_ref: None,
            input: None,
            vars: HashMap::new(),
            model: None,
            tool: None,
            phase_timeout_secs: None,
            phase_filter: None,
            on_phase_event: None,
            hub: Some(hub.clone()),
            phase_routing: None,
            mcp_config: None,
        })
        .await
        .expect("workflow execution should succeed");

        assert!(result.success, "manual wait should not exit as a runner failure");
        assert_eq!(result.workflow_status, WorkflowStatus::Paused);
        assert_eq!(result.phase_results[0]["status"].as_str(), Some("manual_pending"));
        assert_eq!(result.phase_results[0]["workflow_status"].as_str(), Some("paused"));

        let updated = hub.workflows().get(&result.workflow_id).await.expect("workflow should reload");
        assert_eq!(updated.status, WorkflowStatus::Paused);
    }

    #[test]
    fn cancelled_workflows_exit_unsuccessfully() {
        assert!(workflow_exit_success(WorkflowStatus::Completed));
        assert!(workflow_exit_success(WorkflowStatus::Paused));
        assert!(!workflow_exit_success(WorkflowStatus::Cancelled));
    }
}

use chrono::Utc;
use orchestrator_core::{
    InMemoryServiceHub, RequirementItem, RequirementLinks, RequirementPriority, RequirementStatus,
    REQUIREMENT_TASK_GENERATION_RUN_WORKFLOW_REF, REQUIREMENT_TASK_GENERATION_WORKFLOW_REF,
};

#[tokio::test]
async fn resolve_execution_subject_context_uses_requirement_metadata() {
    let hub = Arc::new(InMemoryServiceHub::new());
    let now = Utc::now();

    hub.planning()
        .upsert_requirement(RequirementItem {
            id: "REQ-123".to_string(),
            title: "Generate linked tasks".to_string(),
            description: "Create implementation-ready tasks from this requirement.".to_string(),
            body: None,
            legacy_id: None,
            category: None,
            requirement_type: None,
            acceptance_criteria: vec!["Derived tasks exist".to_string()],
            priority: RequirementPriority::Should,
            status: RequirementStatus::Refined,
            source: "test".to_string(),
            tags: Vec::new(),
            links: RequirementLinks::default(),
            comments: Vec::new(),
            relative_path: None,
            linked_task_ids: Vec::new(),
            created_at: now,
            updated_at: now,
        })
        .await
        .expect("upsert requirement");

    let context = resolve_execution_subject_context(
        hub as Arc<dyn ServiceHub>,
        &WorkflowSubject::Requirement { id: "REQ-123".to_string() },
        None,
        None,
    )
    .await
    .expect("resolve requirement context");

    assert_eq!(context.subject_title, "Generate linked tasks");
    assert_eq!(context.subject_description, "Create implementation-ready tasks from this requirement.");
    assert!(context.task.is_none());
}

#[tokio::test]
async fn project_requirement_success_status_projects_planned_for_plan_workflow() {
    let hub = Arc::new(InMemoryServiceHub::new());
    let now = Utc::now();

    hub.planning()
        .upsert_requirement(RequirementItem {
            id: "REQ-200".to_string(),
            title: "Plan requirement".to_string(),
            description: "Requirement lifecycle parity".to_string(),
            body: None,
            legacy_id: None,
            category: None,
            requirement_type: None,
            acceptance_criteria: Vec::new(),
            priority: RequirementPriority::Should,
            status: RequirementStatus::Refined,
            source: "test".to_string(),
            tags: Vec::new(),
            links: RequirementLinks::default(),
            comments: Vec::new(),
            relative_path: None,
            linked_task_ids: Vec::new(),
            created_at: now,
            updated_at: now,
        })
        .await
        .expect("upsert requirement");

    project_requirement_success_status(
        hub.clone(),
        &WorkflowSubject::Requirement { id: "REQ-200".to_string() },
        REQUIREMENT_TASK_GENERATION_WORKFLOW_REF,
    )
    .await
    .expect("projection should succeed");

    let updated = hub.planning().get_requirement("REQ-200").await.expect("requirement should exist");
    assert_eq!(updated.status, RequirementStatus::Planned);
}

#[tokio::test]
async fn project_requirement_success_status_projects_in_progress_for_run_workflow() {
    let hub = Arc::new(InMemoryServiceHub::new());
    let now = Utc::now();

    hub.planning()
        .upsert_requirement(RequirementItem {
            id: "REQ-201".to_string(),
            title: "Run requirement".to_string(),
            description: "Requirement lifecycle parity".to_string(),
            body: None,
            legacy_id: None,
            category: None,
            requirement_type: None,
            acceptance_criteria: Vec::new(),
            priority: RequirementPriority::Should,
            status: RequirementStatus::Refined,
            source: "test".to_string(),
            tags: Vec::new(),
            links: RequirementLinks::default(),
            comments: Vec::new(),
            relative_path: None,
            linked_task_ids: Vec::new(),
            created_at: now,
            updated_at: now,
        })
        .await
        .expect("upsert requirement");

    project_requirement_success_status(
        hub.clone(),
        &WorkflowSubject::Requirement { id: "REQ-201".to_string() },
        REQUIREMENT_TASK_GENERATION_RUN_WORKFLOW_REF,
    )
    .await
    .expect("projection should succeed");

    let updated = hub.planning().get_requirement("REQ-201").await.expect("requirement should exist");
    assert_eq!(updated.status, RequirementStatus::InProgress);
}
