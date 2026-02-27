use super::*;
use crate::types::{ArchitectureEntity, RequirementPriority, RequirementStatus, WorkflowStatus};

fn global_requirements_index_dir(project_root: &std::path::Path) -> std::path::PathBuf {
    let home = dirs::home_dir().expect("home dir available");
    home.join(".ao")
        .join("index")
        .join(protocol::repository_scope_for_path(project_root))
        .join("requirements")
}

fn assert_core_state_json_is_valid(project_root: &std::path::Path) {
    let state_path = project_root.join(".ao").join("core-state.json");
    let raw = std::fs::read_to_string(&state_path).expect("core-state should be readable");
    serde_json::from_str::<serde_json::Value>(&raw).expect("core-state should be valid json");
}

fn ensure_test_config_env() {
    static INIT: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    INIT.get_or_init(|| {
        let config_dir = std::env::temp_dir().join(format!(
            "ao-orchestrator-core-test-config-{}",
            std::process::id()
        ));
        let home_dir = config_dir.join("home");
        std::fs::create_dir_all(&config_dir).expect("create test AO config dir");
        std::fs::create_dir_all(&home_dir).expect("create test home dir");
        std::env::set_var("HOME", &home_dir);
        std::env::set_var("AO_CONFIG_DIR", &config_dir);
        std::env::set_var("AGENT_ORCHESTRATOR_CONFIG_DIR", &config_dir);
        std::env::set_var("AO_RUNNER_CONFIG_DIR", &config_dir);
    });
}

fn file_hub(project_root: &std::path::Path) -> anyhow::Result<FileServiceHub> {
    ensure_test_config_env();
    FileServiceHub::new(project_root)
}

#[tokio::test]
async fn file_hub_persists_projects_with_rich_payload() {
    let temp = tempfile::tempdir().expect("tempdir");
    let hub = file_hub(temp.path()).expect("create hub");
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

    let second_hub = file_hub(temp.path()).expect("reload hub");
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

#[test]
fn file_hub_new_does_not_rewrite_existing_core_state_on_boot() {
    let temp = tempfile::tempdir().expect("tempdir");
    let _hub = file_hub(temp.path()).expect("create hub");

    let state_path = temp.path().join(".ao").join("core-state.json");
    let mut raw: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(&state_path).expect("core-state should be readable"),
    )
    .expect("core-state should parse");
    raw.as_object_mut().expect("core-state is object").insert(
        "__sentinel".to_string(),
        serde_json::json!({"source":"regression-test"}),
    );
    std::fs::write(
        &state_path,
        serde_json::to_string_pretty(&raw).expect("serialize state"),
    )
    .expect("write state with sentinel");
    let before = std::fs::read_to_string(&state_path).expect("read sentinel state");

    let _reloaded = file_hub(temp.path()).expect("reload hub");
    let after = std::fs::read_to_string(&state_path).expect("read reloaded state");

    assert_eq!(before, after, "hub startup should not rewrite core-state");
}

#[test]
fn file_hub_new_bootstraps_ao_without_initializing_git_repository() {
    let temp = tempfile::tempdir().expect("tempdir");
    let _hub = file_hub(temp.path()).expect("create hub");

    assert!(temp.path().join(".ao").join("core-state.json").exists());
    assert!(!temp.path().join(".git").exists());

    let git_repo_status = std::process::Command::new("git")
        .arg("-C")
        .arg(temp.path())
        .args(["rev-parse", "--is-inside-work-tree"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .expect("git should be available");
    assert!(!git_repo_status.success());

    let head_status = std::process::Command::new("git")
        .arg("-C")
        .arg(temp.path())
        .args(["rev-parse", "--verify", "HEAD"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .expect("git should be available");
    assert!(!head_status.success());
}

#[tokio::test]
async fn file_hub_project_create_bootstraps_base_configs_for_project_path() {
    let temp = tempfile::tempdir().expect("tempdir");
    let hub = file_hub(temp.path()).expect("create hub");
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
    assert!(!project_path.join(".git").exists());
}

#[test]
fn file_hub_explicit_git_bootstrap_initializes_repository_and_head() {
    let temp = tempfile::tempdir().expect("tempdir");
    let project_path = temp.path().join("explicit-git-bootstrap");

    FileServiceHub::bootstrap_project_git_repository(&project_path)
        .expect("bootstrap git repository");
    assert!(project_path.join(".git").exists());

    let git_repo_status = std::process::Command::new("git")
        .arg("-C")
        .arg(&project_path)
        .args(["rev-parse", "--is-inside-work-tree"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .expect("git should be available");
    assert!(git_repo_status.success());

    let head_status = std::process::Command::new("git")
        .arg("-C")
        .arg(&project_path)
        .args(["rev-parse", "--verify", "HEAD"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .expect("git should resolve HEAD");
    assert!(head_status.success());
}

#[tokio::test]
async fn file_hub_bootstraps_workflow_config_v2_with_phase_catalog() {
    let temp = tempfile::tempdir().expect("tempdir");
    let hub = file_hub(temp.path()).expect("create hub");
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
async fn file_hub_bootstraps_architecture_docs_file() {
    let temp = tempfile::tempdir().expect("tempdir");
    let _hub = file_hub(temp.path()).expect("create hub");

    let architecture_path = temp
        .path()
        .join(".ao")
        .join("docs")
        .join("architecture.json");
    assert!(architecture_path.exists());

    let architecture_json: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(architecture_path).expect("architecture doc should be readable"),
    )
    .expect("architecture doc should be json");
    assert_eq!(
        architecture_json
            .get("schema")
            .and_then(serde_json::Value::as_str),
        Some("ao.architecture.v1")
    );
}

#[tokio::test]
async fn file_hub_load_persists_active_project_selection() {
    let temp = tempfile::tempdir().expect("tempdir");
    let hub = file_hub(temp.path()).expect("create hub");

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

    let reloaded = file_hub(temp.path()).expect("reload hub");
    let active = ProjectServiceApi::active(&reloaded)
        .await
        .expect("active project")
        .expect("active project should exist");
    assert_eq!(active.id, first.id);
}

#[tokio::test]
async fn file_hub_persists_tasks() {
    let temp = tempfile::tempdir().expect("tempdir");
    let hub = file_hub(temp.path()).expect("create hub");
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
            linked_architecture_entities: Vec::new(),
        },
    )
    .await
    .expect("create task");

    let second_hub = file_hub(temp.path()).expect("reload hub");
    let loaded = TaskServiceApi::get(&second_hub, &created.id)
        .await
        .expect("load task");
    assert_eq!(loaded.title, "Persist me");
}

#[tokio::test]
async fn file_hub_mutations_fail_closed_for_invalid_core_state_json() {
    let temp = tempfile::tempdir().expect("tempdir");
    let hub = file_hub(temp.path()).expect("create hub");
    let state_path = temp.path().join(".ao").join("core-state.json");
    std::fs::write(&state_path, "{not-valid-json").expect("write malformed state");

    let error = TaskServiceApi::create(
        &hub,
        TaskCreateInput {
            title: "Should fail".to_string(),
            description: String::new(),
            task_type: None,
            priority: None,
            created_by: Some("test".to_string()),
            tags: vec![],
            linked_requirements: vec![],
            linked_architecture_entities: vec![],
        },
    )
    .await
    .expect_err("malformed core-state should reject mutation");
    let message = format!("{error:#}");
    assert!(message.contains("refusing mutation to avoid data loss"));
    assert_eq!(
        std::fs::read_to_string(&state_path).expect("malformed state remains on disk"),
        "{not-valid-json"
    );
}

#[test]
fn file_hub_concurrent_requirement_upserts_keep_unique_ids() {
    let temp = tempfile::tempdir().expect("tempdir");
    let hub_a = file_hub(temp.path()).expect("create first hub");
    let hub_b = file_hub(temp.path()).expect("create second hub");
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));

    let barrier_a = barrier.clone();
    let thread_a = std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime should build");
        barrier_a.wait();
        runtime.block_on(async {
            let now = chrono::Utc::now();
            PlanningServiceApi::upsert_requirement(
                &hub_a,
                RequirementItem {
                    id: String::new(),
                    title: "Concurrent requirement A".to_string(),
                    description: "First concurrent requirement".to_string(),
                    body: None,
                    legacy_id: None,
                    category: None,
                    requirement_type: None,
                    acceptance_criteria: vec!["AC-A".to_string()],
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
            .expect("upsert requirement A")
            .id
        })
    });

    let barrier_b = barrier.clone();
    let thread_b = std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime should build");
        barrier_b.wait();
        runtime.block_on(async {
            let now = chrono::Utc::now();
            PlanningServiceApi::upsert_requirement(
                &hub_b,
                RequirementItem {
                    id: String::new(),
                    title: "Concurrent requirement B".to_string(),
                    description: "Second concurrent requirement".to_string(),
                    body: None,
                    legacy_id: None,
                    category: None,
                    requirement_type: None,
                    acceptance_criteria: vec!["AC-B".to_string()],
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
            .expect("upsert requirement B")
            .id
        })
    });

    barrier.wait();
    let first_id = thread_a.join().expect("thread A should finish");
    let second_id = thread_b.join().expect("thread B should finish");
    assert_ne!(first_id, second_id, "requirement IDs must be unique");

    let reloaded = file_hub(temp.path()).expect("reload hub");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime should build");
    let requirements = runtime.block_on(async {
        PlanningServiceApi::list_requirements(&reloaded)
            .await
            .expect("list requirements")
    });

    let ids: std::collections::HashSet<String> = requirements
        .into_iter()
        .map(|requirement| requirement.id)
        .collect();
    assert_eq!(ids.len(), 2, "both concurrent requirements must persist");
    assert!(ids.contains(&first_id));
    assert!(ids.contains(&second_id));
    assert_core_state_json_is_valid(temp.path());
}

#[test]
fn file_hub_concurrent_task_creates_keep_unique_ids() {
    let temp = tempfile::tempdir().expect("tempdir");
    let hub_a = file_hub(temp.path()).expect("create first hub");
    let hub_b = file_hub(temp.path()).expect("create second hub");
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));

    let barrier_a = barrier.clone();
    let thread_a = std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime should build");
        barrier_a.wait();
        runtime.block_on(async {
            TaskServiceApi::create(
                &hub_a,
                TaskCreateInput {
                    title: "Concurrent task A".to_string(),
                    description: String::new(),
                    task_type: None,
                    priority: None,
                    created_by: Some("test-a".to_string()),
                    tags: vec![],
                    linked_requirements: vec![],
                    linked_architecture_entities: vec![],
                },
            )
            .await
            .expect("create task A")
            .id
        })
    });

    let barrier_b = barrier.clone();
    let thread_b = std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime should build");
        barrier_b.wait();
        runtime.block_on(async {
            TaskServiceApi::create(
                &hub_b,
                TaskCreateInput {
                    title: "Concurrent task B".to_string(),
                    description: String::new(),
                    task_type: None,
                    priority: None,
                    created_by: Some("test-b".to_string()),
                    tags: vec![],
                    linked_requirements: vec![],
                    linked_architecture_entities: vec![],
                },
            )
            .await
            .expect("create task B")
            .id
        })
    });

    barrier.wait();
    let first_id = thread_a.join().expect("thread A should finish");
    let second_id = thread_b.join().expect("thread B should finish");
    assert_ne!(first_id, second_id, "task IDs must be unique");

    let reloaded = file_hub(temp.path()).expect("reload hub");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime should build");
    let tasks =
        runtime.block_on(async { TaskServiceApi::list(&reloaded).await.expect("list tasks") });

    let ids: std::collections::HashSet<String> = tasks.into_iter().map(|task| task.id).collect();
    assert_eq!(ids.len(), 2, "both concurrent tasks must persist");
    assert!(ids.contains(&first_id));
    assert!(ids.contains(&second_id));
    assert_core_state_json_is_valid(temp.path());
}

#[test]
fn file_hub_daemon_mutation_interleaves_with_task_create_without_lost_updates() {
    let temp = tempfile::tempdir().expect("tempdir");
    let hub_a = file_hub(temp.path()).expect("create first hub");
    let hub_b = file_hub(temp.path()).expect("create second hub");
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));

    let barrier_a = barrier.clone();
    let daemon_thread = std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime should build");
        barrier_a.wait();
        runtime.block_on(async {
            DaemonServiceApi::pause(&hub_a)
                .await
                .expect("daemon pause should succeed");
        });
    });

    let barrier_b = barrier.clone();
    let task_thread = std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime should build");
        barrier_b.wait();
        runtime.block_on(async {
            TaskServiceApi::create(
                &hub_b,
                TaskCreateInput {
                    title: "Daemon interleave task".to_string(),
                    description: String::new(),
                    task_type: None,
                    priority: None,
                    created_by: Some("interleave".to_string()),
                    tags: vec![],
                    linked_requirements: vec![],
                    linked_architecture_entities: vec![],
                },
            )
            .await
            .expect("create interleave task")
            .id
        })
    });

    barrier.wait();
    daemon_thread.join().expect("daemon thread should finish");
    let task_id = task_thread.join().expect("task thread should finish");

    let reloaded = file_hub(temp.path()).expect("reload hub");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime should build");
    let status = runtime.block_on(async {
        DaemonServiceApi::status(&reloaded)
            .await
            .expect("daemon status should load")
    });
    assert_eq!(status, DaemonStatus::Paused);
    let task = runtime.block_on(async {
        TaskServiceApi::get(&reloaded, &task_id)
            .await
            .expect("interleaved task should exist")
    });
    assert_eq!(task.id, task_id);
    assert_core_state_json_is_valid(temp.path());
}

#[tokio::test]
async fn file_hub_persists_workflows_with_machine_state() {
    let temp = tempfile::tempdir().expect("tempdir");
    let hub = file_hub(temp.path()).expect("create hub");
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

    let second_hub = file_hub(temp.path()).expect("reload hub");
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

    let hub = file_hub(temp.path()).expect("create hub");
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
            linked_architecture_entities: vec![],
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
            linked_architecture_entities: vec![],
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
async fn task_service_rejects_unknown_architecture_entities() {
    let hub = InMemoryServiceHub::new();
    let error = TaskServiceApi::create(
        &hub,
        TaskCreateInput {
            title: "Unknown architecture link".to_string(),
            description: String::new(),
            task_type: Some(TaskType::Feature),
            priority: Some(Priority::Medium),
            created_by: Some("tester".to_string()),
            tags: vec![],
            linked_requirements: vec![],
            linked_architecture_entities: vec!["arch-does-not-exist".to_string()],
        },
    )
    .await
    .expect_err("task create should reject unknown architecture entity");
    assert!(error
        .to_string()
        .contains("linked architecture entity not found"));
}

#[tokio::test]
async fn task_filter_supports_linked_architecture_entity() {
    let hub = InMemoryServiceHub::new();
    {
        let mut state = hub.state.write().await;
        state.architecture.entities.push(ArchitectureEntity {
            id: "arch-api".to_string(),
            name: "API Layer".to_string(),
            kind: "module".to_string(),
            description: None,
            code_paths: vec!["crates/orchestrator-cli/src/services".to_string()],
            tags: vec!["backend".to_string()],
            metadata: std::collections::HashMap::new(),
        });
    }

    let linked = TaskServiceApi::create(
        &hub,
        TaskCreateInput {
            title: "Linked task".to_string(),
            description: String::new(),
            task_type: Some(TaskType::Feature),
            priority: Some(Priority::Medium),
            created_by: Some("tester".to_string()),
            tags: vec![],
            linked_requirements: vec![],
            linked_architecture_entities: vec!["arch-api".to_string()],
        },
    )
    .await
    .expect("linked task should be created");

    TaskServiceApi::create(
        &hub,
        TaskCreateInput {
            title: "Unlinked task".to_string(),
            description: String::new(),
            task_type: Some(TaskType::Feature),
            priority: Some(Priority::Low),
            created_by: Some("tester".to_string()),
            tags: vec![],
            linked_requirements: vec![],
            linked_architecture_entities: vec![],
        },
    )
    .await
    .expect("unlinked task should be created");

    let filtered = TaskServiceApi::list_filtered(
        &hub,
        TaskFilter {
            linked_architecture_entity: Some("arch-api".to_string()),
            ..TaskFilter::default()
        },
    )
    .await
    .expect("filter should succeed");
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].id, linked.id);
}

#[tokio::test]
async fn task_status_guardrails_enforced_for_set_update_and_replace() {
    let hub = InMemoryServiceHub::new();
    let task = TaskServiceApi::create(
        &hub,
        TaskCreateInput {
            title: "transition guardrails".to_string(),
            description: String::new(),
            task_type: Some(TaskType::Feature),
            priority: Some(Priority::Medium),
            created_by: Some("tester".to_string()),
            tags: vec![],
            linked_requirements: vec![],
            linked_architecture_entities: vec![],
        },
    )
    .await
    .expect("task should be created");

    let done_error = TaskServiceApi::set_status(&hub, &task.id, TaskStatus::Done)
        .await
        .expect_err("done should require in-progress");
    assert!(done_error
        .to_string()
        .contains("done requires prior status in-progress"));

    TaskServiceApi::set_status(&hub, &task.id, TaskStatus::InProgress)
        .await
        .expect("in-progress from backlog should be allowed");
    TaskServiceApi::set_status(&hub, &task.id, TaskStatus::Done)
        .await
        .expect("done from in-progress should be allowed");

    let terminal_task = TaskServiceApi::get(&hub, &task.id)
        .await
        .expect("task should load");
    let terminal_version = terminal_task.metadata.version;
    let terminal_updated_at = terminal_task.metadata.updated_at;

    let update_error = TaskServiceApi::update(
        &hub,
        &task.id,
        TaskUpdateInput {
            title: Some("should not apply".to_string()),
            description: None,
            priority: None,
            status: Some(TaskStatus::Ready),
            assignee: None,
            tags: None,
            updated_by: Some("tester".to_string()),
            deadline: None,
            linked_architecture_entities: None,
        },
    )
    .await
    .expect_err("terminal task should require explicit reopen");
    assert!(update_error
        .to_string()
        .contains("terminal states require explicit reopen action"));

    let after_failed_update = TaskServiceApi::get(&hub, &task.id)
        .await
        .expect("task should load");
    assert_eq!(after_failed_update.status, TaskStatus::Done);
    assert_eq!(after_failed_update.title, terminal_task.title);
    assert_eq!(after_failed_update.metadata.version, terminal_version);
    assert_eq!(after_failed_update.metadata.updated_at, terminal_updated_at);

    let mut replacement = after_failed_update.clone();
    replacement.status = TaskStatus::Ready;
    replacement.metadata.updated_by = "tester".to_string();
    let replace_error = TaskServiceApi::replace(&hub, replacement)
        .await
        .expect_err("replace should enforce transition guardrails");
    assert!(replace_error
        .to_string()
        .contains("terminal states require explicit reopen action"));

    let after_failed_replace = TaskServiceApi::get(&hub, &task.id)
        .await
        .expect("task should load");
    assert_eq!(after_failed_replace.status, TaskStatus::Done);
    assert_eq!(after_failed_replace.metadata.version, terminal_version);
}

#[tokio::test]
async fn task_reopen_requires_terminal_source_and_allowed_target() {
    let hub = InMemoryServiceHub::new();
    let task = TaskServiceApi::create(
        &hub,
        TaskCreateInput {
            title: "reopen lifecycle".to_string(),
            description: String::new(),
            task_type: Some(TaskType::Feature),
            priority: Some(Priority::Medium),
            created_by: Some("tester".to_string()),
            tags: vec![],
            linked_requirements: vec![],
            linked_architecture_entities: vec![],
        },
    )
    .await
    .expect("task should be created");

    let non_terminal_error = TaskServiceApi::reopen(&hub, &task.id, TaskStatus::Ready)
        .await
        .expect_err("reopen should fail for non-terminal task");
    assert!(non_terminal_error
        .to_string()
        .contains("reopen is only allowed from done or cancelled"));

    let cancelled = TaskServiceApi::set_status(&hub, &task.id, TaskStatus::Cancelled)
        .await
        .expect("cancelled should be allowed");
    assert!(cancelled.cancelled);

    let cancelled_set_status_error =
        TaskServiceApi::set_status(&hub, &task.id, TaskStatus::InProgress)
            .await
            .expect_err("cancelled task should require explicit reopen");
    assert!(cancelled_set_status_error
        .to_string()
        .contains("terminal states require explicit reopen action"));

    let reopened = TaskServiceApi::reopen(&hub, &task.id, TaskStatus::Backlog)
        .await
        .expect("cancelled task should reopen to backlog");
    assert_eq!(reopened.status, TaskStatus::Backlog);
    assert!(!reopened.cancelled);

    TaskServiceApi::set_status(&hub, &task.id, TaskStatus::InProgress)
        .await
        .expect("in-progress should be allowed");
    let done_once = TaskServiceApi::set_status(&hub, &task.id, TaskStatus::Done)
        .await
        .expect("done should be allowed");
    let first_completed_at = done_once
        .metadata
        .completed_at
        .expect("done status should set completed_at");

    let reopened_done = TaskServiceApi::reopen(&hub, &task.id, TaskStatus::Ready)
        .await
        .expect("done task should reopen to ready");
    assert_eq!(reopened_done.status, TaskStatus::Ready);
    assert!(!reopened_done.cancelled);
    assert!(
        reopened_done.metadata.completed_at.is_none(),
        "reopen should clear stale completion timestamp"
    );

    TaskServiceApi::set_status(&hub, &task.id, TaskStatus::InProgress)
        .await
        .expect("in-progress should be allowed");
    let done_twice = TaskServiceApi::set_status(&hub, &task.id, TaskStatus::Done)
        .await
        .expect("done should be allowed");
    let second_completed_at = done_twice
        .metadata
        .completed_at
        .expect("done status should set completed_at");
    assert!(second_completed_at > first_completed_at);

    let invalid_target_error = TaskServiceApi::reopen(&hub, &task.id, TaskStatus::InProgress)
        .await
        .expect_err("invalid reopen target should fail");
    assert!(invalid_target_error
        .to_string()
        .contains("reopen target must be backlog or ready"));
}

#[tokio::test]
async fn workflow_service_exposes_decisions_and_checkpoints() {
    let temp = tempfile::tempdir().expect("tempdir");
    let hub = file_hub(temp.path()).expect("create hub");
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
    let hub = file_hub(temp.path()).expect("create hub");

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
    let hub = file_hub(temp.path()).expect("create hub");

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

#[tokio::test]
async fn execute_requirements_maps_requirement_priority_to_task_priority() {
    let hub = InMemoryServiceHub::new();
    PlanningServiceApi::draft_vision(
        &hub,
        VisionDraftInput {
            project_name: Some("Priority Mapping".to_string()),
            problem_statement: "Validate requirement-to-task priority mapping".to_string(),
            target_users: vec!["Engineering".to_string()],
            goals: vec!["Maintain stable priority behavior".to_string()],
            constraints: vec![],
            value_proposition: None,
            complexity_assessment: None,
        },
    )
    .await
    .expect("draft vision");

    let cases = [
        (RequirementPriority::Must, Priority::High, "must"),
        (RequirementPriority::Should, Priority::Medium, "should"),
        (RequirementPriority::Could, Priority::Low, "could"),
        (RequirementPriority::Wont, Priority::Low, "wont"),
    ];

    for (index, (requirement_priority, expected_task_priority, label)) in
        cases.into_iter().enumerate()
    {
        let now = chrono::Utc::now();
        let requirement = PlanningServiceApi::upsert_requirement(
            &hub,
            RequirementItem {
                id: String::new(),
                title: format!("Priority mapping {label}"),
                description: format!("Ensure `{label}` maps to expected task priority"),
                body: None,
                legacy_id: None,
                category: None,
                requirement_type: None,
                acceptance_criteria: vec![format!(
                    "Task priority generated for `{label}` is deterministic"
                )],
                priority: requirement_priority,
                status: RequirementStatus::Draft,
                source: "manual".to_string(),
                tags: vec!["priority".to_string()],
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
                include_wont: true,
            },
        )
        .await
        .expect("execute requirements");
        assert!(
            !execution.task_ids_created.is_empty(),
            "expected tasks for case {index} ({label})"
        );

        for task_id in execution.task_ids_created {
            let task = TaskServiceApi::get(&hub, &task_id)
                .await
                .expect("task should exist");
            assert_eq!(
                task.priority, expected_task_priority,
                "unexpected task priority for case {index} ({label})"
            );
        }
    }
}
