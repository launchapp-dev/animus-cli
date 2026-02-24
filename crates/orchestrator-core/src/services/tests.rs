use super::*;
use crate::types::{RequirementPriority, WorkflowStatus};
use sha2::{Digest, Sha256};

fn sanitize_identifier(value: &str) -> String {
    let mut out = String::new();
    for ch in value.chars() {
        match ch {
            'a'..='z' | 'A'..='Z' | '0'..='9' => out.push(ch.to_ascii_lowercase()),
            ' ' | '_' | '-' => out.push('-'),
            _ => {}
        }
    }

    while out.contains("--") {
        out = out.replace("--", "-");
    }
    out = out.trim_matches('-').to_string();
    if out.is_empty() {
        "repo".to_string()
    } else {
        out
    }
}

fn repository_scope_for_path(path: &std::path::Path) -> String {
    let canonical = path
        .canonicalize()
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .to_string();

    let repo_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .map(sanitize_identifier)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "repo".to_string());

    let mut hasher = Sha256::new();
    hasher.update(canonical.as_bytes());
    let digest = hasher.finalize();
    let suffix = format!(
        "{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        digest[0], digest[1], digest[2], digest[3], digest[4], digest[5]
    );

    format!("{repo_name}-{suffix}")
}

fn global_requirements_index_dir(project_root: &std::path::Path) -> std::path::PathBuf {
    let home = dirs::home_dir().expect("home dir available");
    home.join(".ao")
        .join("index")
        .join(repository_scope_for_path(project_root))
        .join("requirements")
}

#[tokio::test]
async fn file_hub_persists_projects_with_rich_payload() {
    let temp = tempfile::tempdir().expect("tempdir");
    let hub = FileServiceHub::new(temp.path()).expect("create hub");
    let created = ProjectServiceApi::create(
        &hub,
        ProjectCreateInput {
            name: "Standalone Core".to_string(),
            path: temp.path().join("standalone-core").display().to_string(),
            project_type: Some(ProjectType::WebApp),
            description: Some("Core project".to_string()),
            tech_stack: vec!["rust".to_string(), "desktop-gui".to_string()],
            metadata: Some(crate::types::ProjectMetadata {
                problem_statement: Some("Unify desktop and CLI".to_string()),
                target_users: vec!["engineers".to_string()],
                goals: vec!["single runtime".to_string()],
                description: None,
                custom: std::collections::HashMap::new(),
            }),
        },
    )
    .await
    .expect("create project");

    let second_hub = FileServiceHub::new(temp.path()).expect("reload hub");
    let loaded = ProjectServiceApi::load(&second_hub, &created.path)
        .await
        .expect("load by path");
    assert_eq!(loaded.id, created.id);
    assert_eq!(loaded.config.project_type, ProjectType::WebApp);
    assert_eq!(loaded.config.tech_stack, vec!["rust", "desktop-gui"]);
    assert_eq!(loaded.metadata.goals, vec!["single runtime"]);
    assert_eq!(
        loaded.metadata.description,
        Some("Core project".to_string())
    );
}

#[tokio::test]
async fn file_hub_project_create_bootstraps_base_configs_for_project_path() {
    let temp = tempfile::tempdir().expect("tempdir");
    let hub = FileServiceHub::new(temp.path()).expect("create hub");
    let project_path = temp.path().join("scaffolded-project");

    let created = ProjectServiceApi::create(
        &hub,
        ProjectCreateInput {
            name: "Scaffolded".to_string(),
            path: project_path.display().to_string(),
            project_type: Some(ProjectType::WebApp),
            description: None,
            tech_stack: vec![],
            metadata: None,
        },
    )
    .await
    .expect("create project");

    assert_eq!(created.path, project_path.display().to_string());
    assert!(project_path.join(".ao").join("core-state.json").exists());
    assert!(project_path.join(".ao").join("config.json").exists());
    assert!(project_path.join(".ao").join("resume-config.json").exists());
    assert!(project_path
        .join(".ao")
        .join("state")
        .join("workflow-config.v2.json")
        .exists());
    assert!(project_path
        .join(".ao")
        .join("state")
        .join("state-machines.v1.json")
        .exists());
    assert!(project_path
        .join(".ao")
        .join("state")
        .join("agent-runtime-config.v2.json")
        .exists());

    let git_repo_status = std::process::Command::new("git")
        .arg("-C")
        .arg(&project_path)
        .args(["rev-parse", "--is-inside-work-tree"])
        .status()
        .expect("git should be available");
    assert!(git_repo_status.success());

    let head_status = std::process::Command::new("git")
        .arg("-C")
        .arg(&project_path)
        .args(["rev-parse", "--verify", "HEAD"])
        .status()
        .expect("git should resolve HEAD");
    assert!(head_status.success());
}

#[tokio::test]
async fn file_hub_bootstraps_workflow_config_v2_with_phase_catalog() {
    let temp = tempfile::tempdir().expect("tempdir");
    let hub = FileServiceHub::new(temp.path()).expect("create hub");
    let project_path = temp.path().join("configured-project");

    let created = ProjectServiceApi::create(
        &hub,
        ProjectCreateInput {
            name: "Configured".to_string(),
            path: project_path.display().to_string(),
            project_type: Some(ProjectType::WebApp),
            description: None,
            tech_stack: vec![],
            metadata: None,
        },
    )
    .await
    .expect("create project");

    assert_eq!(created.path, project_path.display().to_string());
    let workflow_config_path = project_path
        .join(".ao")
        .join("state")
        .join("workflow-config.v2.json");
    let config_content =
        std::fs::read_to_string(workflow_config_path).expect("workflow config should be readable");
    let config: serde_json::Value =
        serde_json::from_str(&config_content).expect("workflow config should parse");

    assert_eq!(
        config
            .pointer("/schema")
            .and_then(serde_json::Value::as_str),
        Some("ao.workflow-config.v2")
    );
    assert_eq!(
        config
            .pointer("/version")
            .and_then(serde_json::Value::as_u64),
        Some(2)
    );
    assert_eq!(
        config
            .pointer("/default_pipeline_id")
            .and_then(serde_json::Value::as_str),
        Some("standard")
    );
    assert_eq!(
        config
            .pointer("/phase_catalog/implementation/label")
            .and_then(serde_json::Value::as_str),
        Some("Implementation")
    );
    assert_eq!(
        config
            .pointer("/pipelines/1/phases/1")
            .and_then(serde_json::Value::as_str),
        Some("ux-research")
    );
}

#[tokio::test]
async fn file_hub_load_persists_active_project_selection() {
    let temp = tempfile::tempdir().expect("tempdir");
    let hub = FileServiceHub::new(temp.path()).expect("create hub");

    let first = ProjectServiceApi::create(
        &hub,
        ProjectCreateInput {
            name: "First".to_string(),
            path: temp.path().join("first").display().to_string(),
            project_type: Some(ProjectType::Other),
            description: None,
            tech_stack: vec![],
            metadata: None,
        },
    )
    .await
    .expect("create first");

    let second = ProjectServiceApi::create(
        &hub,
        ProjectCreateInput {
            name: "Second".to_string(),
            path: temp.path().join("second").display().to_string(),
            project_type: Some(ProjectType::Other),
            description: None,
            tech_stack: vec![],
            metadata: None,
        },
    )
    .await
    .expect("create second");

    assert_ne!(first.id, second.id);
    ProjectServiceApi::load(&hub, &first.id)
        .await
        .expect("load first");

    let reloaded = FileServiceHub::new(temp.path()).expect("reload hub");
    let active = ProjectServiceApi::active(&reloaded)
        .await
        .expect("active project")
        .expect("active project should exist");
    assert_eq!(active.id, first.id);
}

#[tokio::test]
async fn file_hub_persists_tasks() {
    let temp = tempfile::tempdir().expect("tempdir");
    let hub = FileServiceHub::new(temp.path()).expect("create hub");
    let created = TaskServiceApi::create(
        &hub,
        TaskCreateInput {
            title: "Persist me".to_string(),
            description: String::new(),
            task_type: None,
            priority: None,
            created_by: None,
            tags: Vec::new(),
            linked_requirements: Vec::new(),
        },
    )
    .await
    .expect("create task");

    let second_hub = FileServiceHub::new(temp.path()).expect("reload hub");
    let loaded = TaskServiceApi::get(&second_hub, &created.id)
        .await
        .expect("load task");
    assert_eq!(loaded.title, "Persist me");
}

#[tokio::test]
async fn file_hub_persists_workflows_with_machine_state() {
    let temp = tempfile::tempdir().expect("tempdir");
    let hub = FileServiceHub::new(temp.path()).expect("create hub");
    let workflow = WorkflowServiceApi::run(
        &hub,
        WorkflowRunInput {
            task_id: "TASK-1".to_string(),
            pipeline_id: Some("standard".to_string()),
        },
    )
    .await
    .expect("run workflow");

    assert_eq!(workflow.status, WorkflowStatus::Running);
    assert_eq!(
        workflow.machine_state,
        crate::types::WorkflowMachineState::RunPhase
    );
    assert_eq!(workflow.checkpoint_metadata.checkpoint_count, 1);
    assert!(workflow.decision_history.is_empty());

    let second_hub = FileServiceHub::new(temp.path()).expect("reload hub");
    let loaded = WorkflowServiceApi::get(&second_hub, &workflow.id)
        .await
        .expect("load workflow");
    assert_eq!(loaded.id, workflow.id);
    assert_eq!(loaded.status, WorkflowStatus::Running);
    assert_eq!(
        loaded.machine_state,
        crate::types::WorkflowMachineState::RunPhase
    );
}

#[tokio::test]
async fn file_hub_uses_custom_pipeline_from_workflow_config_v2() {
    let temp = tempfile::tempdir().expect("tempdir");
    let state_dir = temp.path().join(".ao").join("state");
    std::fs::create_dir_all(&state_dir).expect("state dir should exist");
    std::fs::write(
        state_dir.join("workflow-config.v2.json"),
        serde_json::json!({
            "schema": "ao.workflow-config.v2",
            "version": 2,
            "default_pipeline_id": "xhigh-dev",
            "phase_catalog": {
                "requirements": {
                    "label": "Requirements",
                    "description": "",
                    "category": "planning",
                    "icon": null,
                    "docs_url": null,
                    "tags": [],
                    "visible": true
                },
                "implementation": {
                    "label": "Implementation",
                    "description": "",
                    "category": "build",
                    "icon": null,
                    "docs_url": null,
                    "tags": [],
                    "visible": true
                },
                "code-review": {
                    "label": "Code Review",
                    "description": "",
                    "category": "review",
                    "icon": null,
                    "docs_url": null,
                    "tags": [],
                    "visible": true
                },
                "testing": {
                    "label": "Testing",
                    "description": "",
                    "category": "qa",
                    "icon": null,
                    "docs_url": null,
                    "tags": [],
                    "visible": true
                },
                "qa-signoff": {
                    "label": "QA Signoff",
                    "description": "",
                    "category": "qa",
                    "icon": null,
                    "docs_url": null,
                    "tags": [],
                    "visible": true
                }
            },
            "pipelines": [
                {
                    "id": "xhigh-dev",
                    "name": "XHigh Dev",
                    "description": "custom pipeline",
                    "phases": [
                        "requirements",
                        "implementation",
                        "code-review",
                        "testing",
                        "qa-signoff"
                    ]
                }
            ]
        })
        .to_string(),
    )
    .expect("workflow config should be written");
    std::fs::write(
        state_dir.join("agent-runtime-config.v2.json"),
        serde_json::json!({
            "schema": "ao.agent-runtime-config.v2",
            "version": 2,
            "tools_allowlist": ["cargo"],
            "agents": {
                "default": {
                    "description": "default",
                    "system_prompt": "default prompt",
                    "tool": null,
                    "model": null,
                    "fallback_models": [],
                    "reasoning_effort": null,
                    "web_search": null,
                    "timeout_secs": null,
                    "max_attempts": null
                }
            },
            "phases": {
                "default": {
                    "mode": "agent",
                    "agent_id": "default",
                    "directive": "default directive",
                    "runtime": null,
                    "output_contract": null,
                    "output_json_schema": null,
                    "command": null,
                    "manual": null
                },
                "requirements": {
                    "mode": "agent",
                    "agent_id": "default",
                    "directive": "requirements",
                    "runtime": null,
                    "output_contract": null,
                    "output_json_schema": null,
                    "command": null,
                    "manual": null
                },
                "implementation": {
                    "mode": "agent",
                    "agent_id": "default",
                    "directive": "implementation",
                    "runtime": null,
                    "output_contract": null,
                    "output_json_schema": null,
                    "command": null,
                    "manual": null
                },
                "code-review": {
                    "mode": "agent",
                    "agent_id": "default",
                    "directive": "review",
                    "runtime": null,
                    "output_contract": null,
                    "output_json_schema": null,
                    "command": null,
                    "manual": null
                },
                "testing": {
                    "mode": "agent",
                    "agent_id": "default",
                    "directive": "testing",
                    "runtime": null,
                    "output_contract": null,
                    "output_json_schema": null,
                    "command": null,
                    "manual": null
                },
                "qa-signoff": {
                    "mode": "manual",
                    "agent_id": null,
                    "directive": "manual",
                    "runtime": null,
                    "output_contract": null,
                    "output_json_schema": null,
                    "command": null,
                    "manual": {
                        "instructions": "approve qa signoff",
                        "approval_note_required": true
                    }
                }
            }
        })
        .to_string(),
    )
    .expect("agent runtime config should be written");

    let hub = FileServiceHub::new(temp.path()).expect("create hub");
    let workflow = WorkflowServiceApi::run(
        &hub,
        WorkflowRunInput {
            task_id: "TASK-1".to_string(),
            pipeline_id: Some("xhigh-dev".to_string()),
        },
    )
    .await
    .expect("run workflow");

    let phase_ids = workflow
        .phases
        .iter()
        .map(|phase| phase.phase_id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        phase_ids,
        vec![
            "requirements",
            "implementation",
            "code-review",
            "testing",
            "qa-signoff"
        ]
    );
}

#[tokio::test]
async fn project_service_tracks_active_project_and_rename() {
    let hub = InMemoryServiceHub::new();
    let first = ProjectServiceApi::create(
        &hub,
        ProjectCreateInput {
            name: "One".to_string(),
            path: "/tmp/project-one".to_string(),
            project_type: Some(ProjectType::Other),
            description: None,
            tech_stack: vec![],
            metadata: None,
        },
    )
    .await
    .expect("create first project");
    let second = ProjectServiceApi::create(
        &hub,
        ProjectCreateInput {
            name: "Two".to_string(),
            path: "/tmp/project-two".to_string(),
            project_type: Some(ProjectType::Other),
            description: None,
            tech_stack: vec![],
            metadata: None,
        },
    )
    .await
    .expect("create second project");

    let active = ProjectServiceApi::active(&hub)
        .await
        .expect("active project")
        .expect("expected active project");
    assert_eq!(active.id, second.id);

    let loaded = ProjectServiceApi::load(&hub, &first.id)
        .await
        .expect("load by id");
    assert_eq!(loaded.id, first.id);

    let renamed = ProjectServiceApi::rename(&hub, &first.id, "Renamed")
        .await
        .expect("rename project");
    assert_eq!(renamed.name, "Renamed");

    let active = ProjectServiceApi::active(&hub)
        .await
        .expect("active project")
        .expect("expected active project");
    assert_eq!(active.id, first.id);
}

#[tokio::test]
async fn task_service_supports_priority_checklists_and_dependencies() {
    let hub = InMemoryServiceHub::new();
    let low = TaskServiceApi::create(
        &hub,
        TaskCreateInput {
            title: "Low".to_string(),
            description: String::new(),
            task_type: Some(TaskType::Feature),
            priority: Some(Priority::Low),
            created_by: Some("tester".to_string()),
            tags: vec![],
            linked_requirements: vec![],
        },
    )
    .await
    .expect("create low");
    let high = TaskServiceApi::create(
        &hub,
        TaskCreateInput {
            title: "High".to_string(),
            description: String::new(),
            task_type: Some(TaskType::Feature),
            priority: Some(Priority::High),
            created_by: Some("tester".to_string()),
            tags: vec!["backend".to_string()],
            linked_requirements: vec![],
        },
    )
    .await
    .expect("create high");

    let prioritized = TaskServiceApi::list_prioritized(&hub)
        .await
        .expect("prioritized list");
    assert_eq!(
        prioritized.first().map(|task| task.id.as_str()),
        Some(high.id.as_str())
    );

    let updated = TaskServiceApi::add_checklist_item(
        &hub,
        &high.id,
        "Write tests".to_string(),
        "tester".to_string(),
    )
    .await
    .expect("add checklist");
    assert_eq!(updated.checklist.len(), 1);

    let item_id = updated.checklist[0].id.clone();
    let updated =
        TaskServiceApi::update_checklist_item(&hub, &high.id, &item_id, true, "tester".to_string())
            .await
            .expect("update checklist");
    assert!(updated.checklist[0].completed);

    let with_dep = TaskServiceApi::add_dependency(
        &hub,
        &high.id,
        &low.id,
        DependencyType::BlockedBy,
        "tester".to_string(),
    )
    .await
    .expect("add dependency");
    assert_eq!(with_dep.dependencies.len(), 1);

    let without_dep =
        TaskServiceApi::remove_dependency(&hub, &high.id, &low.id, "tester".to_string())
            .await
            .expect("remove dependency");
    assert!(without_dep.dependencies.is_empty());

    let stats = TaskServiceApi::statistics(&hub)
        .await
        .expect("task statistics");
    assert_eq!(stats.total, 2);
}

#[tokio::test]
async fn workflow_service_exposes_decisions_and_checkpoints() {
    let temp = tempfile::tempdir().expect("tempdir");
    let hub = FileServiceHub::new(temp.path()).expect("create hub");
    let workflow = WorkflowServiceApi::run(
        &hub,
        WorkflowRunInput {
            task_id: "TASK-123".to_string(),
            pipeline_id: Some("standard".to_string()),
        },
    )
    .await
    .expect("run workflow");

    let workflow = WorkflowServiceApi::complete_current_phase(&hub, &workflow.id)
        .await
        .expect("complete current phase");
    assert!(!workflow.decision_history.is_empty());

    let decisions = WorkflowServiceApi::decisions(&hub, &workflow.id)
        .await
        .expect("get decisions");
    assert!(!decisions.is_empty());

    let checkpoints = WorkflowServiceApi::list_checkpoints(&hub, &workflow.id)
        .await
        .expect("list checkpoints");
    assert_eq!(checkpoints, vec![1, 2]);

    let checkpoint = WorkflowServiceApi::get_checkpoint(&hub, &workflow.id, 1)
        .await
        .expect("get checkpoint");
    assert_eq!(checkpoint.id, workflow.id);
}

#[tokio::test]
async fn planning_service_drafts_and_executes_requirements() {
    let hub = InMemoryServiceHub::new();

    let vision = PlanningServiceApi::draft_vision(
        &hub,
        VisionDraftInput {
            project_name: Some("Parity Test".to_string()),
            problem_statement: "Users cannot ship quickly".to_string(),
            target_users: vec!["Founders".to_string()],
            goals: vec![
                "Draft requirements from vision".to_string(),
                "Execute tasks from requirements".to_string(),
            ],
            constraints: vec!["Keep current stack".to_string()],
            value_proposition: Some("Faster delivery with lower coordination cost".to_string()),
            complexity_assessment: None,
        },
    )
    .await
    .expect("draft vision");
    assert!(vision.markdown.contains("Product Vision"));

    let drafted = PlanningServiceApi::draft_requirements(
        &hub,
        RequirementsDraftInput {
            include_codebase_scan: false,
            append_only: true,
            max_requirements: 4,
        },
    )
    .await
    .expect("draft requirements");
    assert!(drafted.appended_count > 0);

    let refined = PlanningServiceApi::refine_requirements(
        &hub,
        RequirementsRefineInput {
            requirement_ids: vec![],
            focus: Some("testability".to_string()),
        },
    )
    .await
    .expect("refine requirements");
    assert!(!refined.is_empty());
    assert!(refined
        .iter()
        .all(|item| item.status == RequirementStatus::Refined));

    let execution = PlanningServiceApi::execute_requirements(
        &hub,
        RequirementsExecutionInput {
            requirement_ids: vec![],
            start_workflows: true,
            pipeline_id: Some("standard".to_string()),
            include_wont: false,
        },
    )
    .await
    .expect("execute requirements");
    assert!(execution.requirements_considered > 0);
    assert!(!execution.workflow_ids_started.is_empty());
}

#[tokio::test]
async fn planning_draft_requirements_preserves_vision_constraints_when_max_is_small() {
    let hub = InMemoryServiceHub::new();

    PlanningServiceApi::draft_vision(
        &hub,
        VisionDraftInput {
            project_name: Some("Constraint Gate".to_string()),
            problem_statement: "Need strict stack compliance".to_string(),
            target_users: vec!["Platform engineers".to_string()],
            goals: vec!["Ship MVP quickly".to_string()],
            constraints: vec![
                "Frontend must use Next.js App Router with TypeScript".to_string(),
                "Primary database must be PostgreSQL".to_string(),
            ],
            value_proposition: Some("Prevent architecture drift".to_string()),
            complexity_assessment: None,
        },
    )
    .await
    .expect("draft vision");

    let drafted = PlanningServiceApi::draft_requirements(
        &hub,
        RequirementsDraftInput {
            include_codebase_scan: false,
            append_only: true,
            max_requirements: 1,
        },
    )
    .await
    .expect("draft requirements");

    assert!(drafted
        .requirements
        .iter()
        .any(|requirement| requirement.source == "vision-constraint"));
    assert!(drafted.requirements.iter().any(|requirement| {
        requirement
            .title
            .to_ascii_lowercase()
            .contains("next.js app router with typescript")
    }));
    assert!(drafted.requirements.iter().any(|requirement| {
        requirement
            .title
            .to_ascii_lowercase()
            .contains("primary database must be postgresql")
    }));
}

#[tokio::test]
async fn execute_requirements_blocks_when_vision_constraints_are_not_covered() {
    let hub = InMemoryServiceHub::new();

    PlanningServiceApi::draft_vision(
        &hub,
        VisionDraftInput {
            project_name: Some("Constraint Coverage".to_string()),
            problem_statement: "Need guaranteed stack constraints".to_string(),
            target_users: vec!["Founders".to_string()],
            goals: vec!["Build product".to_string()],
            constraints: vec!["Primary database must be PostgreSQL".to_string()],
            value_proposition: None,
            complexity_assessment: None,
        },
    )
    .await
    .expect("draft vision");

    let now = chrono::Utc::now();
    let unrelated = PlanningServiceApi::upsert_requirement(
        &hub,
        RequirementItem {
            id: String::new(),
            title: "Add marketing copy polish".to_string(),
            description: "Improve hero copy and CTA clarity.".to_string(),
            body: None,
            legacy_id: None,
            category: None,
            requirement_type: None,
            acceptance_criteria: vec!["Copy updates are reviewed".to_string()],
            priority: RequirementPriority::Should,
            status: RequirementStatus::Draft,
            source: "manual".to_string(),
            tags: vec!["frontend".to_string()],
            links: crate::types::RequirementLinks::default(),
            comments: vec![],
            relative_path: None,
            linked_task_ids: vec![],
            created_at: now,
            updated_at: now,
        },
    )
    .await
    .expect("upsert requirement");

    let error = PlanningServiceApi::execute_requirements(
        &hub,
        RequirementsExecutionInput {
            requirement_ids: vec![unrelated.id],
            start_workflows: false,
            pipeline_id: None,
            include_wont: false,
        },
    )
    .await
    .expect_err("execution should be blocked by missing constraint coverage");

    assert!(error
        .to_string()
        .to_ascii_lowercase()
        .contains("vision constraints missing from requirements"));
}

#[tokio::test]
async fn file_hub_persists_planning_artifacts() {
    let temp = tempfile::tempdir().expect("tempdir");
    let hub = FileServiceHub::new(temp.path()).expect("create hub");

    PlanningServiceApi::draft_vision(
        &hub,
        VisionDraftInput {
            project_name: Some("Docs".to_string()),
            problem_statement: "Need repeatable planning".to_string(),
            target_users: vec!["PM".to_string()],
            goals: vec!["Generate requirements".to_string()],
            constraints: vec![],
            value_proposition: None,
            complexity_assessment: None,
        },
    )
    .await
    .expect("draft vision");

    PlanningServiceApi::draft_requirements(&hub, RequirementsDraftInput::default())
        .await
        .expect("draft requirements");

    let vision_path = temp
        .path()
        .join(".ao")
        .join("docs")
        .join("product-vision.md");
    let vision_json_path = temp.path().join(".ao").join("docs").join("vision.json");
    assert!(vision_path.exists());
    assert!(vision_json_path.exists());
}

#[tokio::test]
async fn requirements_refine_propagates_research_metadata_to_tasks() {
    let hub = InMemoryServiceHub::new();

    PlanningServiceApi::draft_vision(
        &hub,
        VisionDraftInput {
            project_name: Some("Research Flow".to_string()),
            problem_statement: "Need validated technical direction".to_string(),
            target_users: vec!["Engineers".to_string()],
            goals: vec!["Reduce unknowns".to_string()],
            constraints: vec![],
            value_proposition: None,
            complexity_assessment: None,
        },
    )
    .await
    .expect("draft vision");

    let now = chrono::Utc::now();
    let requirement = RequirementItem {
        id: String::new(),
        title: "Investigate authentication provider tradeoffs".to_string(),
        description: "Research and compare options, then validate decision assumptions".to_string(),
        body: None,
        legacy_id: None,
        category: None,
        requirement_type: None,
        acceptance_criteria: vec!["Decision documented".to_string()],
        priority: RequirementPriority::Should,
        status: RequirementStatus::Draft,
        source: "manual".to_string(),
        tags: vec![],
        links: crate::types::RequirementLinks::default(),
        comments: vec![],
        relative_path: None,
        linked_task_ids: vec![],
        created_at: now,
        updated_at: now,
    };

    let requirement = PlanningServiceApi::upsert_requirement(&hub, requirement)
        .await
        .expect("upsert requirement");

    let refined = PlanningServiceApi::refine_requirements(
        &hub,
        RequirementsRefineInput {
            requirement_ids: vec![requirement.id.clone()],
            focus: Some("validation".to_string()),
        },
    )
    .await
    .expect("refine requirements");

    let refined_requirement = refined
        .iter()
        .find(|item| item.id == requirement.id)
        .expect("requirement should be refined");
    assert!(refined_requirement
        .tags
        .iter()
        .any(|tag| tag == "needs-research"));
    assert!(refined_requirement
        .acceptance_criteria
        .iter()
        .any(|criterion| {
            criterion
                .to_ascii_lowercase()
                .contains("research findings documented")
        }));

    let execution = PlanningServiceApi::execute_requirements(
        &hub,
        RequirementsExecutionInput {
            requirement_ids: vec![requirement.id.clone()],
            start_workflows: false,
            pipeline_id: None,
            include_wont: false,
        },
    )
    .await
    .expect("execute requirements");
    let task_id = execution
        .task_ids_created
        .first()
        .expect("task should be created");
    let task = TaskServiceApi::get(&hub, task_id)
        .await
        .expect("task should exist");
    assert!(task.tags.iter().any(|tag| tag == "needs-research"));
    assert!(task.workflow_metadata.requires_architecture);
}

#[tokio::test]
async fn execute_requirements_runs_requirement_state_machine_before_task_materialization() {
    let hub = InMemoryServiceHub::new();

    PlanningServiceApi::draft_vision(
        &hub,
        VisionDraftInput {
            project_name: Some("Lifecycle Loop".to_string()),
            problem_statement: "Need requirements with explicit review gates".to_string(),
            target_users: vec!["Marketing leads".to_string()],
            goals: vec!["Launch production-ready campaign workspace".to_string()],
            constraints: vec![],
            value_proposition: Some("Deterministic quality before implementation".to_string()),
            complexity_assessment: None,
        },
    )
    .await
    .expect("draft vision");

    let now = chrono::Utc::now();
    let requirement = PlanningServiceApi::upsert_requirement(
        &hub,
        RequirementItem {
            id: String::new(),
            title: "Investigate campaign intelligence approaches".to_string(),
            description: "Investigate architecture options and choose one.".to_string(),
            body: None,
            legacy_id: None,
            category: None,
            requirement_type: None,
            acceptance_criteria: vec!["Decision documented".to_string()],
            priority: RequirementPriority::Should,
            status: RequirementStatus::Draft,
            source: "manual".to_string(),
            tags: vec![],
            links: crate::types::RequirementLinks::default(),
            comments: vec![],
            relative_path: None,
            linked_task_ids: vec![],
            created_at: now,
            updated_at: now,
        },
    )
    .await
    .expect("upsert requirement");

    let execution = PlanningServiceApi::execute_requirements(
        &hub,
        RequirementsExecutionInput {
            requirement_ids: vec![requirement.id.clone()],
            start_workflows: false,
            pipeline_id: None,
            include_wont: false,
        },
    )
    .await
    .expect("execute requirements");
    assert!(!execution.task_ids_created.is_empty());

    let updated_requirement = PlanningServiceApi::get_requirement(&hub, &requirement.id)
        .await
        .expect("requirement should exist");
    assert_eq!(updated_requirement.status, RequirementStatus::Planned);
    assert!(updated_requirement
        .tags
        .iter()
        .any(|tag| tag.eq_ignore_ascii_case("needs-research")));
    assert!(updated_requirement
        .acceptance_criteria
        .iter()
        .any(|criterion| {
            criterion
                .to_ascii_lowercase()
                .contains("research findings documented")
        }));
    assert!(updated_requirement
        .acceptance_criteria
        .iter()
        .any(|criterion| {
            criterion
                .to_ascii_lowercase()
                .contains("automated test coverage")
        }));
    assert!(updated_requirement
        .comments
        .iter()
        .any(|comment| comment.phase.as_deref() == Some("po-review")));
    assert!(updated_requirement
        .comments
        .iter()
        .any(|comment| comment.phase.as_deref() == Some("em-review")));
    assert!(updated_requirement
        .comments
        .iter()
        .any(|comment| comment.phase.as_deref() == Some("rework")));
    assert!(updated_requirement
        .comments
        .iter()
        .any(|comment| comment.phase.as_deref() == Some("approved")));

    let created_task_id = execution
        .task_ids_created
        .first()
        .expect("task should exist");
    let task = TaskServiceApi::get(&hub, created_task_id)
        .await
        .expect("task should be loadable");
    assert!(task.workflow_metadata.requires_architecture);
    assert!(!task.checklist.is_empty());
    assert!(task.checklist.iter().any(|item| {
        item.description
            .to_ascii_lowercase()
            .contains("code review gate")
    }));
}

#[tokio::test]
async fn file_hub_writes_legacy_style_requirement_and_task_files() {
    let temp = tempfile::tempdir().expect("tempdir");
    let hub = FileServiceHub::new(temp.path()).expect("create hub");

    PlanningServiceApi::draft_vision(
        &hub,
        VisionDraftInput {
            project_name: Some("Parity Files".to_string()),
            problem_statement: "Need CLI-compatible artifacts".to_string(),
            target_users: vec!["PM".to_string(), "Engineer".to_string()],
            goals: vec!["Generate detailed requirement and task artifacts".to_string()],
            constraints: vec!["Use Next.js and PostgreSQL".to_string()],
            value_proposition: None,
            complexity_assessment: None,
        },
    )
    .await
    .expect("draft vision");

    let drafted = PlanningServiceApi::draft_requirements(
        &hub,
        RequirementsDraftInput {
            include_codebase_scan: false,
            append_only: true,
            max_requirements: 2,
        },
    )
    .await
    .expect("draft requirements");
    let requirement_id = drafted
        .requirements
        .first()
        .expect("requirement should exist")
        .id
        .clone();

    let execution = PlanningServiceApi::execute_requirements(
        &hub,
        RequirementsExecutionInput {
            requirement_ids: vec![requirement_id.clone()],
            start_workflows: false,
            pipeline_id: None,
            include_wont: false,
        },
    )
    .await
    .expect("execute requirements");
    let task_id = execution
        .task_ids_created
        .first()
        .expect("task should be created")
        .clone();

    let requirement = PlanningServiceApi::get_requirement(&hub, &requirement_id)
        .await
        .expect("load requirement");
    assert!(!requirement.links.tasks.is_empty());
    assert!(requirement.links.tasks.contains(&task_id));
    let requirement_relative_path = requirement
        .relative_path
        .clone()
        .expect("relative path should be set");

    let requirement_file_path = temp
        .path()
        .join(".ao")
        .join("requirements")
        .join(requirement_relative_path);
    assert!(requirement_file_path.exists());

    let requirement_file_json: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(&requirement_file_path)
            .expect("requirement file should be readable"),
    )
    .expect("requirement file should be json");
    assert_eq!(
        requirement_file_json
            .get("id")
            .and_then(serde_json::Value::as_str),
        Some(requirement_id.as_str())
    );

    let requirement_index_path = global_requirements_index_dir(temp.path()).join("index.json");
    assert!(requirement_index_path.exists());

    let task_file_path = temp
        .path()
        .join(".ao")
        .join("tasks")
        .join(format!("{}.json", task_id));
    assert!(task_file_path.exists());
    let task_file_json: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(&task_file_path).expect("task file should be readable"),
    )
    .expect("task file should be json");
    let task_description = task_file_json
        .get("description")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    assert!(!task_description.trim().is_empty());
    assert!(
        task_description.contains("Acceptance Criteria")
            || task_description.contains("## Implementation Notes")
    );
}

#[tokio::test]
async fn execute_requirements_generates_stable_task_titles() {
    let hub = InMemoryServiceHub::new();
    PlanningServiceApi::draft_vision(
        &hub,
        VisionDraftInput {
            project_name: Some("Task Title Parity".to_string()),
            problem_statement: "Need structured task generation".to_string(),
            target_users: vec!["Engineering".to_string()],
            goals: vec![
                "Deliver end-to-end workflow".to_string(),
                "Ship with tests and review gates".to_string(),
            ],
            constraints: vec![],
            value_proposition: None,
            complexity_assessment: None,
        },
    )
    .await
    .expect("draft vision");

    let drafted = PlanningServiceApi::draft_requirements(&hub, RequirementsDraftInput::default())
        .await
        .expect("draft requirements");
    let requirement_id = drafted
        .requirements
        .first()
        .expect("requirement should exist")
        .id
        .clone();

    let execution = PlanningServiceApi::execute_requirements(
        &hub,
        RequirementsExecutionInput {
            requirement_ids: vec![requirement_id],
            start_workflows: false,
            pipeline_id: None,
            include_wont: false,
        },
    )
    .await
    .expect("execute requirements");

    assert!(!execution.task_ids_created.is_empty());
    for task_id in execution.task_ids_created {
        let task = TaskServiceApi::get(&hub, &task_id)
            .await
            .expect("task should exist");
        assert!(!task.title.contains("[AC"));
        assert!(!task.title.contains("[Integration]"));
    }
}

#[tokio::test]
async fn execute_requirements_excludes_wont_by_default() {
    let hub = InMemoryServiceHub::new();
    PlanningServiceApi::draft_vision(
        &hub,
        VisionDraftInput {
            project_name: Some("No Wont By Default".to_string()),
            problem_statement: "Validate execute requirement filtering".to_string(),
            target_users: vec!["Engineering".to_string()],
            goals: vec!["Run only actionable requirements".to_string()],
            constraints: vec![],
            value_proposition: None,
            complexity_assessment: None,
        },
    )
    .await
    .expect("draft vision");

    let drafted = PlanningServiceApi::draft_requirements(&hub, RequirementsDraftInput::default())
        .await
        .expect("draft requirements");
    let mut requirement = drafted
        .requirements
        .first()
        .cloned()
        .expect("requirement should exist");
    requirement.priority = RequirementPriority::Wont;
    PlanningServiceApi::upsert_requirement(&hub, requirement.clone())
        .await
        .expect("upsert requirement");

    let error = PlanningServiceApi::execute_requirements(
        &hub,
        RequirementsExecutionInput {
            requirement_ids: vec![requirement.id],
            start_workflows: false,
            pipeline_id: None,
            include_wont: false,
        },
    )
    .await
    .expect_err("wont requirement should be excluded by default");
    assert!(error.to_string().contains("include-wont"));
}

#[tokio::test]
async fn execute_requirements_can_include_wont_with_opt_in() {
    let hub = InMemoryServiceHub::new();
    PlanningServiceApi::draft_vision(
        &hub,
        VisionDraftInput {
            project_name: Some("Include Wont Opt In".to_string()),
            problem_statement: "Validate explicit include_wont behavior".to_string(),
            target_users: vec!["Engineering".to_string()],
            goals: vec!["Run gated requirement sets".to_string()],
            constraints: vec![],
            value_proposition: None,
            complexity_assessment: None,
        },
    )
    .await
    .expect("draft vision");

    let drafted = PlanningServiceApi::draft_requirements(&hub, RequirementsDraftInput::default())
        .await
        .expect("draft requirements");
    let mut requirement = drafted
        .requirements
        .first()
        .cloned()
        .expect("requirement should exist");
    requirement.priority = RequirementPriority::Wont;
    PlanningServiceApi::upsert_requirement(&hub, requirement.clone())
        .await
        .expect("upsert requirement");

    let result = PlanningServiceApi::execute_requirements(
        &hub,
        RequirementsExecutionInput {
            requirement_ids: vec![requirement.id],
            start_workflows: false,
            pipeline_id: None,
            include_wont: true,
        },
    )
    .await
    .expect("wont requirement should run when include_wont=true");
    assert_eq!(result.requirements_considered, 1);
}
