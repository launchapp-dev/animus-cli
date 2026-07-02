use super::*;
use crate::types::{
    ArchitectureEntity, ListPageRequest, Priority, RequirementFilter, RequirementItem, RequirementPriority,
    RequirementQuery, RequirementQuerySort, RequirementStatus, RequirementType, TaskCreateInput, TaskQuery,
    TaskQuerySort, TaskType, WorkflowDecisionRisk, WorkflowFilter, WorkflowQuery, WorkflowQuerySort, WorkflowRunInput,
    WorkflowStatus,
};

fn scoped_ao_root(project_root: &std::path::Path) -> std::path::PathBuf {
    protocol::scoped_state_root(project_root).unwrap_or_else(|| project_root.join(".animus"))
}

fn assert_core_state_json_is_valid(project_root: &std::path::Path) {
    let state_path = scoped_ao_root(project_root).join("core-state.json");
    let raw = std::fs::read_to_string(&state_path).expect("core-state should be readable");
    serde_json::from_str::<serde_json::Value>(&raw).expect("core-state should be valid json");
}

fn ensure_test_config_env() {
    // HOME and the config-dir env vars are pinned together inside
    // `stable_test_home`'s one-time init, so the pin cannot land between
    // two env reads of a concurrently running test.
    crate::test_env::stable_test_home();
}

fn file_hub(project_root: &std::path::Path) -> anyhow::Result<FileServiceHub> {
    ensure_test_config_env();
    FileServiceHub::new(project_root)
}

#[test]
fn file_hub_new_does_not_rewrite_existing_core_state_on_boot() {
    let temp = tempfile::tempdir().expect("tempdir");
    let _hub = file_hub(temp.path()).expect("create hub");

    let state_path = scoped_ao_root(temp.path()).join("core-state.json");
    let mut raw: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&state_path).expect("core-state should be readable"))
            .expect("core-state should parse");
    raw.as_object_mut()
        .expect("core-state is object")
        .insert("__sentinel".to_string(), serde_json::json!({"source":"regression-test"}));
    std::fs::write(&state_path, serde_json::to_string_pretty(&raw).expect("serialize state"))
        .expect("write state with sentinel");
    let before = std::fs::read_to_string(&state_path).expect("read sentinel state");

    let _reloaded = file_hub(temp.path()).expect("reload hub");
    let after = std::fs::read_to_string(&state_path).expect("read reloaded state");

    assert_eq!(before, after, "hub startup should not rewrite core-state");
}

#[test]
fn file_hub_recompiles_repo_workflow_yaml_on_startup() {
    let temp = tempfile::tempdir().expect("tempdir");
    let _hub = file_hub(temp.path()).expect("create hub");

    let workflows_dir = temp.path().join(".animus").join("workflows");
    std::fs::create_dir_all(&workflows_dir).expect("create workflows dir");
    // v0.6 kernel-purification: this overwrites the scaffold's custom.yaml
    // (which holds the agent/phase definitions the scaffold workflows
    // reference), so it must redeclare them to keep the merged config valid.
    std::fs::write(
        workflows_dir.join("custom.yaml"),
        r#"
default_workflow_ref: yaml-standard

tools_allowlist:
  - cargo

agents:
  default:
    description: Default
    system_prompt: Default agent

phases:
  requirements:
    mode: agent
    agent_id: default
  research:
    mode: agent
    agent_id: default
  implementation:
    mode: agent
    agent_id: default
  code-review:
    mode: agent
    agent_id: default
  testing:
    mode: agent
    agent_id: default

workflows:
  - id: yaml-standard
    name: YAML Standard
    description: Startup-compiled workflow from repo-local YAML.
    phases:
      - requirements
      - implementation
"#,
    )
    .expect("write workflow yaml");

    let _reloaded = file_hub(temp.path()).expect("reload hub");
    let _config_source_seam =
        orchestrator_config::workflow_config::config_source_client::install_yaml_config_source_base(temp.path());
    let config = crate::load_workflow_config(temp.path(), None).expect("workflow config should load");

    assert_eq!(config.default_workflow_ref.as_str(), "yaml-standard");
    assert!(
        config.workflows.iter().any(|workflow| workflow.id == "yaml-standard"),
        "resolved workflow config should include repo-local YAML workflow"
    );
}

#[test]
fn file_hub_new_bootstraps_ao_without_initializing_git_repository() {
    let temp = tempfile::tempdir().expect("tempdir");
    let _hub = file_hub(temp.path()).expect("create hub");

    assert!(scoped_ao_root(temp.path()).join("core-state.json").exists());
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

#[test]
fn file_hub_new_bootstraps_base_configs_for_project_path() {
    let temp = tempfile::tempdir().expect("tempdir");
    let project_path = temp.path().join("scaffolded-project");

    let _hub = file_hub(&project_path).expect("create hub at project path");

    let scoped = scoped_ao_root(&project_path);
    assert!(scoped.join("core-state.json").exists());
    assert!(project_path.join(".animus").join("config.json").exists());
    assert!(scoped.join("resume-config.json").exists());
    // The kernel ships zero baked workflow content: bootstrap creates the
    // workflows dir but scaffolds NO default templates (workflows come from the
    // flavor / config_source plugin).
    assert!(project_path.join(".animus").join("workflows").exists());
    assert!(!project_path.join(".animus").join("workflows").join("custom.yaml").exists());
    assert!(!project_path.join(".animus").join("workflows").join("standard-workflow.yaml").exists());
    assert!(scoped.join("config").join("state-machines.v1.json").exists());
    assert!(!project_path.join(".git").exists());
    assert!(
        !scoped.join("config").join("workflow-config.v2.json").exists(),
        "bootstrap must not generate JSON workflow config"
    );
}

#[test]
fn file_hub_explicit_git_bootstrap_initializes_repository_and_head() {
    let temp = tempfile::tempdir().expect("tempdir");
    let project_path = temp.path().join("explicit-git-bootstrap");

    FileServiceHub::bootstrap_project_git_repository(&project_path).expect("bootstrap git repository");
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

#[test]
fn file_hub_bootstraps_workflow_yaml_with_phase_catalog() {
    let temp = tempfile::tempdir().expect("tempdir");
    let project_path = temp.path().join("configured-project");

    let _hub = file_hub(&project_path).expect("create hub at project path");
    let _config_source_seam =
        orchestrator_config::workflow_config::config_source_client::install_yaml_config_source_base(&project_path);

    let config = crate::load_workflow_config(&project_path, None).expect("workflow config should load");

    assert_eq!(config.schema.as_str(), "animus.workflow-config.v2");
    assert_eq!(config.version, 2);
    assert_eq!(config.default_workflow_ref.as_str(), "standard-workflow");
    assert_eq!(config.phase_catalog.get("implementation").map(|phase| phase.label.as_str()), Some("Implementation"));
    assert_eq!(
        config
            .workflows
            .iter()
            .find(|workflow| workflow.id == "standard-workflow")
            .and_then(|workflow| workflow.phases.get(1))
            .map(|phase| phase.phase_id()),
        Some("implementation")
    );
}

#[tokio::test]
async fn file_hub_bootstraps_architecture_docs_file() {
    let temp = tempfile::tempdir().expect("tempdir");
    let _hub = file_hub(temp.path()).expect("create hub");

    let architecture_path = scoped_ao_root(temp.path()).join("docs").join("architecture.json");
    assert!(architecture_path.exists());

    let architecture_json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(architecture_path).expect("architecture doc should be readable"))
            .expect("architecture doc should be json");
    assert_eq!(architecture_json.get("schema").and_then(serde_json::Value::as_str), Some("animus.architecture.v1"));
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
    let loaded = TaskServiceApi::get(&second_hub, &created.id).await.expect("load task");
    assert_eq!(loaded.title, "Persist me");
}

#[tokio::test]
async fn file_hub_mutations_fail_closed_for_invalid_core_state_json() {
    let temp = tempfile::tempdir().expect("tempdir");
    let hub = file_hub(temp.path()).expect("create hub");
    let state_path = scoped_ao_root(temp.path()).join("core-state.json");
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
    assert_eq!(std::fs::read_to_string(&state_path).expect("malformed state remains on disk"), "{not-valid-json");
}

#[test]
fn file_hub_concurrent_requirement_upserts_keep_unique_ids() {
    let temp = tempfile::tempdir().expect("tempdir");
    let hub_a = file_hub(temp.path()).expect("create first hub");
    let hub_b = file_hub(temp.path()).expect("create second hub");
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));

    let barrier_a = barrier.clone();
    let thread_a = std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("runtime should build");
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
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("runtime should build");
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
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("runtime should build");
    let requirements =
        runtime.block_on(async { PlanningServiceApi::list_requirements(&reloaded).await.expect("list requirements") });

    let ids: std::collections::HashSet<String> = requirements.into_iter().map(|requirement| requirement.id).collect();
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
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("runtime should build");
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
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("runtime should build");
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
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("runtime should build");
    let tasks = runtime.block_on(async { TaskServiceApi::list(&reloaded).await.expect("list tasks") });

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
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("runtime should build");
        barrier_a.wait();
        runtime.block_on(async {
            DaemonServiceApi::pause(&hub_a).await.expect("daemon pause should succeed");
        });
    });

    let barrier_b = barrier.clone();
    let task_thread = std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("runtime should build");
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
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("runtime should build");
    let status =
        runtime.block_on(async { DaemonServiceApi::status(&reloaded).await.expect("daemon status should load") });
    assert_eq!(status, DaemonStatus::Paused);
    let task = runtime
        .block_on(async { TaskServiceApi::get(&reloaded, &task_id).await.expect("interleaved task should exist") });
    assert_eq!(task.id, task_id);
    assert_core_state_json_is_valid(temp.path());
}

#[test]
fn persist_dirty_to_sqlite_rolls_back_all_entities_on_partial_failure() {
    ensure_test_config_env();
    let temp = tempfile::tempdir().expect("tempdir");

    let now = chrono::Utc::now();
    let task = OrchestratorTask {
        id: "TASK-atomic".to_string(),
        title: "Atomic task".to_string(),
        description: String::new(),
        task_type: TaskType::Feature,
        status: TaskStatus::Ready,
        blocked_reason: None,
        blocked_at: None,
        blocked_phase: None,
        blocked_by: None,
        priority: Priority::Medium,
        risk: RiskLevel::default(),
        scope: Scope::default(),
        complexity: Complexity::default(),
        impact_area: Vec::new(),
        assignee: Assignee::default(),
        estimated_effort: None,
        linked_requirements: Vec::new(),
        linked_architecture_entities: Vec::new(),
        dependencies: Vec::new(),
        checklist: Vec::new(),
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
            completed_at: None,
            status_changed_at: None,
            version: 1,
        },
        deadline: None,
        paused: false,
        cancelled: false,
        resolution: None,
        resource_requirements: crate::types::ResourceRequirements::default(),
        consecutive_dispatch_failures: None,
        last_dispatch_failure_at: None,
        dispatch_history: Vec::new(),
    };
    let requirement = RequirementItem {
        id: "REQ-atomic".to_string(),
        title: "Atomic requirement".to_string(),
        description: String::new(),
        body: None,
        legacy_id: None,
        category: None,
        requirement_type: None,
        acceptance_criteria: Vec::new(),
        priority: RequirementPriority::Should,
        status: RequirementStatus::Draft,
        source: "manual".to_string(),
        tags: Vec::new(),
        links: crate::types::RequirementLinks::default(),
        comments: Vec::new(),
        relative_path: None,
        linked_task_ids: Vec::new(),
        created_at: now,
        updated_at: now,
    };
    let mut state = CoreState::default();
    state.tasks.insert(task.id.clone(), task);
    state.requirements.insert(requirement.id.clone(), requirement);
    let task_ids = vec!["TASK-atomic".to_string()];
    let req_ids = vec!["REQ-atomic".to_string()];

    let conn = crate::workflow::open_project_db(temp.path()).expect("open db");
    conn.execute_batch(
        "CREATE TRIGGER fail_requirement_insert BEFORE INSERT ON requirements
         BEGIN SELECT RAISE(ABORT, 'injected failure'); END;",
    )
    .expect("create failure trigger");
    drop(conn);

    let error = FileServiceHub::persist_dirty_to_sqlite(temp.path(), &state, &task_ids, &req_ids)
        .expect_err("requirement persistence failure should fail the whole set");
    assert!(format!("{error:#}").contains("REQ-atomic"));
    assert!(
        crate::workflow::load_all_tasks(temp.path()).expect("load tasks").is_empty(),
        "task upsert must roll back when requirement persistence fails"
    );
    assert!(crate::workflow::load_all_requirements(temp.path()).expect("load requirements").is_empty());

    let conn = crate::workflow::open_project_db(temp.path()).expect("reopen db");
    conn.execute_batch("DROP TRIGGER fail_requirement_insert;").expect("drop failure trigger");
    drop(conn);

    FileServiceHub::persist_dirty_to_sqlite(temp.path(), &state, &task_ids, &req_ids)
        .expect("retry should persist the full set");
    assert_eq!(crate::workflow::load_all_tasks(temp.path()).expect("load tasks").len(), 1);
    assert_eq!(crate::workflow::load_all_requirements(temp.path()).expect("load requirements").len(), 1);
}

#[tokio::test]
async fn file_hub_yaml_only_repo_executes_workflow_without_json_config() {
    let temp = tempfile::tempdir().expect("tempdir");
    let hub = file_hub(temp.path()).expect("create hub");

    let workflows_dir = temp.path().join(".animus").join("workflows");
    std::fs::create_dir_all(&workflows_dir).expect("create workflows dir");
    // v0.6 kernel-purification: overwriting the scaffold's custom.yaml drops the
    // agent/phase definitions it carried, so redeclare them here.
    std::fs::write(
        workflows_dir.join("custom.yaml"),
        r#"
tools_allowlist:
  - cargo
agents:
  default:
    description: Default
    system_prompt: Default agent
phases:
  requirements:
    mode: agent
    agent_id: default
  research:
    mode: agent
    agent_id: default
  implementation:
    mode: agent
    agent_id: default
  code-review:
    mode: agent
    agent_id: default
  testing:
    mode: agent
    agent_id: default
workflows:
  - id: yaml-only-workflow
    name: YAML Only
    phases:
      - requirements
      - implementation
"#,
    )
    .expect("write yaml workflow");
    let _config_source_seam =
        orchestrator_config::workflow_config::config_source_client::install_yaml_config_source_base(temp.path());

    let scoped = scoped_ao_root(temp.path());
    assert!(!scoped.join("state").join("workflow-config.v2.json").exists(), "no JSON config should exist");

    let config = crate::load_workflow_config(temp.path(), None).expect("yaml-only repo should load workflow config");
    assert!(
        config.workflows.iter().any(|w| w.id == "yaml-only-workflow"),
        "workflow config should include yaml-defined workflow"
    );

    let workflow = WorkflowServiceApi::run(
        &hub,
        WorkflowRunInput::for_task("TASK-yaml-only".to_string(), Some("yaml-only-workflow".to_string())),
        None,
    )
    .await
    .expect("workflow should start in yaml-only repo");

    assert_eq!(workflow.status, WorkflowStatus::Running);
    let phase_ids: Vec<&str> = workflow.phases.iter().map(|p| p.phase_id.as_str()).collect();
    assert_eq!(phase_ids, vec!["requirements", "implementation"]);
    assert!(!scoped.join("state").join("workflow-config.v2.json").exists(), "execution must not generate JSON config");
}

#[tokio::test]
async fn file_hub_run_persists_actor_for_lifecycle_continuation() {
    // Codex P1: the actor a run bootstraps with must persist so EVERY later
    // lifecycle transition (resume/complete/cancel/...) re-resolves config from
    // the same partition — otherwise an actor-private workflow starts then fails
    // at the next transition (global re-lookup). run() persists it on the run row.
    let temp = tempfile::tempdir().expect("tempdir");
    let hub = file_hub(temp.path()).expect("create hub");
    let _config_source_seam =
        orchestrator_config::workflow_config::config_source_client::install_yaml_config_source_base(temp.path());
    let actor = animus_actor::Actor {
        user_id: "alice".to_string(),
        claims: vec!["admin".to_string()],
        tenant_id: Some("acme".to_string()),
    };
    let workflow = WorkflowServiceApi::run(
        &hub,
        WorkflowRunInput::for_task("TASK-actor".to_string(), Some("standard".to_string())),
        Some(&actor),
    )
    .await
    .expect("run workflow");

    let manager = crate::WorkflowStateManager::new(temp.path());
    assert_eq!(
        manager.load_workflow_actor(&workflow.id),
        Some(actor.clone()),
        "run() must persist the bootstrap actor for lifecycle continuation"
    );

    // The actor MUST survive a lifecycle save: `save()` upserts (not INSERT OR
    // REPLACE), preserving the `actor` column. Without that, the first pause/
    // resume/complete would NULL it and later transitions fall back to global.
    WorkflowServiceApi::pause(&hub, &workflow.id).await.expect("pause workflow");
    assert_eq!(
        manager.load_workflow_actor(&workflow.id),
        Some(actor),
        "actor must survive a lifecycle save (upsert preserves the actor column)"
    );

    // A system-initiated run (actor = None) persists no actor (global scope).
    let global = WorkflowServiceApi::run(
        &hub,
        WorkflowRunInput::for_task("TASK-global".to_string(), Some("standard".to_string())),
        None,
    )
    .await
    .expect("run workflow");
    assert_eq!(manager.load_workflow_actor(&global.id), None, "actor = None must persist nothing (global scope)");
}

#[tokio::test]
async fn file_hub_persists_workflows_with_machine_state() {
    let temp = tempfile::tempdir().expect("tempdir");
    let hub = file_hub(temp.path()).expect("create hub");
    let _config_source_seam =
        orchestrator_config::workflow_config::config_source_client::install_yaml_config_source_base(temp.path());
    let workflow = WorkflowServiceApi::run(
        &hub,
        WorkflowRunInput::for_task("TASK-1".to_string(), Some("standard".to_string())),
        None,
    )
    .await
    .expect("run workflow");

    assert_eq!(workflow.status, WorkflowStatus::Running);
    assert_eq!(workflow.machine_state, crate::types::WorkflowMachineState::RunPhase);
    assert_eq!(workflow.checkpoint_metadata.checkpoint_count, 1);
    assert!(workflow.decision_history.is_empty());

    let second_hub = file_hub(temp.path()).expect("reload hub");
    let loaded = WorkflowServiceApi::get(&second_hub, &workflow.id).await.expect("load workflow");
    assert_eq!(loaded.id, workflow.id);
    assert_eq!(loaded.status, WorkflowStatus::Running);
    assert_eq!(loaded.machine_state, crate::types::WorkflowMachineState::RunPhase);
}

#[tokio::test]
async fn file_hub_complete_phase_with_decision_honors_rework_routing() {
    ensure_test_config_env();
    let temp = tempfile::tempdir().expect("tempdir");
    let mut workflow_config = crate::load_workflow_config_or_default(temp.path()).config;
    // v0.6 kernel-purification: cross-validation requires an EXECUTABLE phase
    // definition (phase_catalog is UI metadata only). Seed an agent + phase
    // definitions alongside the catalog entries.
    workflow_config.tools_allowlist = vec!["cargo".to_string()];
    workflow_config.agent_profiles.insert(
        "default".to_string(),
        orchestrator_config::AgentProfileOverlay { description: Some("Default".to_string()), ..Default::default() },
    );
    for (phase_id, label, category) in
        [("implement", "Implement", "build"), ("qa-review", "QA Review", "qa"), ("push-branch", "Push Branch", "git")]
    {
        workflow_config.phase_catalog.insert(
            phase_id.to_string(),
            crate::PhaseUiDefinition {
                label: label.to_string(),
                description: String::new(),
                category: category.to_string(),
                icon: None,
                docs_url: None,
                tags: Vec::new(),
                visible: true,
            },
        );
        workflow_config.phase_definitions.insert(
            phase_id.to_string(),
            crate::PhaseExecutionDefinition {
                mode: crate::PhaseExecutionMode::Agent,
                agent_id: Some("default".to_string()),
                directive: None,
                system_prompt: None,
                runtime: None,
                capabilities: None,
                output_contract: None,
                output_json_schema: None,
                decision_contract: None,
                retry: None,
                skills: Vec::new(),
                command: None,
                manual: None,
                default_tool: None,
                idempotency: crate::Idempotency::Unknown,
                worktree: None,
                evals: None,
            },
        );
    }
    workflow_config.workflows.push(crate::WorkflowDefinition {
        id: "routed-rework".to_string(),
        name: "Routed Rework".to_string(),
        description: "Regression test for file-backed rework routing".to_string(),
        phases: vec![
            "implement".to_string().into(),
            crate::WorkflowPhaseEntry::Rich(crate::WorkflowPhaseConfig {
                id: "qa-review".to_string(),
                skip_if: Vec::new(),
                max_rework_attempts: 3,
                on_verdict: std::collections::HashMap::from([(
                    "rework".to_string(),
                    crate::PhaseTransitionConfig {
                        target: "implement".to_string(),
                        guard: None,
                        allow_agent_target: false,
                        allowed_targets: Vec::new(),
                    },
                )]),
                budget: None,
                environment: None,
                workspace: None,
            }),
            "push-branch".to_string().into(),
        ],
        variables: Vec::new(),
        worktree: None,
        budget: None,
        environment: None,
        workspace: None,
    });
    crate::write_workflow_config(temp.path(), &workflow_config).expect("write workflow config");

    let hub = file_hub(temp.path()).expect("create hub");
    let _config_source_seam =
        orchestrator_config::workflow_config::config_source_client::install_yaml_config_source_base(temp.path());
    let workflow = WorkflowServiceApi::run(
        &hub,
        WorkflowRunInput::for_task("TASK-routed-rework".to_string(), Some("routed-rework".to_string())),
        None,
    )
    .await
    .expect("run workflow");

    let workflow =
        WorkflowServiceApi::complete_current_phase(&hub, &workflow.id).await.expect("complete implement phase");
    assert_eq!(workflow.current_phase.as_deref(), Some("qa-review"));
    assert_eq!(workflow.current_phase_index, 1);

    let workflow = WorkflowServiceApi::complete_current_phase_with_decision(
        &hub,
        &workflow.id,
        Some(crate::PhaseDecision {
            kind: "phase_result".to_string(),
            phase_id: "qa-review".to_string(),
            verdict: crate::PhaseDecisionVerdict::Rework,
            reason: "needs fixes".to_string(),
            confidence: 0.9,
            risk: WorkflowDecisionRisk::Medium,
            evidence: Vec::new(),
            guardrail_violations: Vec::new(),
            commit_message: None,
            target_phase: None,
        }),
    )
    .await
    .expect("qa review should request rework");

    assert_eq!(workflow.status, WorkflowStatus::Running);
    assert_eq!(workflow.current_phase.as_deref(), Some("implement"));
    assert_eq!(workflow.current_phase_index, 0);
    assert_eq!(workflow.phases[0].status, crate::types::WorkflowPhaseStatus::Running);
    assert_eq!(workflow.decision_history.last().and_then(|entry| entry.target_phase.as_deref()), Some("implement"));
}

#[tokio::test]
async fn file_hub_auto_prunes_checkpoints_on_completion_when_enabled() {
    let temp = tempfile::tempdir().expect("tempdir");
    let hub = file_hub(temp.path()).expect("create hub");
    let _config_source_seam =
        orchestrator_config::workflow_config::config_source_client::install_yaml_config_source_base(temp.path());

    let mut config = crate::load_workflow_config(temp.path(), None).expect("load workflow config");
    config.checkpoint_retention.keep_last_per_phase = 1;
    config.checkpoint_retention.max_age_hours = None;
    config.checkpoint_retention.auto_prune_on_completion = true;
    crate::write_workflow_config(temp.path(), &config).expect("write workflow config");
    let _config_source_seam_after_write =
        orchestrator_config::workflow_config::config_source_client::install_yaml_config_source_base(temp.path());

    let mut workflow = WorkflowServiceApi::run(
        &hub,
        WorkflowRunInput::for_task("TASK-prune".to_string(), Some("standard".to_string())),
        None,
    )
    .await
    .expect("run workflow");

    while workflow.status == WorkflowStatus::Running {
        workflow = WorkflowServiceApi::complete_current_phase(&hub, &workflow.id).await.expect("complete phase");
    }
    assert_eq!(workflow.status, WorkflowStatus::Completed);

    let checkpoints = WorkflowServiceApi::list_checkpoints(&hub, &workflow.id).await.expect("list checkpoints");
    assert_eq!(checkpoints.len(), 5, "completion should auto-prune to one checkpoint per phase");
    assert!(checkpoints.contains(&5));
}

#[tokio::test]
async fn file_hub_completion_remains_successful_when_auto_prune_errors() {
    let temp = tempfile::tempdir().expect("tempdir");
    let hub = file_hub(temp.path()).expect("create hub");
    let _config_source_seam =
        orchestrator_config::workflow_config::config_source_client::install_yaml_config_source_base(temp.path());

    let mut config = crate::load_workflow_config(temp.path(), None).expect("load workflow config");
    config.checkpoint_retention.keep_last_per_phase = 1;
    config.checkpoint_retention.max_age_hours = Some(u64::MAX);
    config.checkpoint_retention.auto_prune_on_completion = true;
    crate::write_workflow_config(temp.path(), &config).expect("write workflow config");
    let _config_source_seam_after_write =
        orchestrator_config::workflow_config::config_source_client::install_yaml_config_source_base(temp.path());

    let mut workflow = WorkflowServiceApi::run(
        &hub,
        WorkflowRunInput::for_task("TASK-prune-error".to_string(), Some("standard".to_string())),
        None,
    )
    .await
    .expect("run workflow");

    while workflow.status == WorkflowStatus::Running {
        workflow = WorkflowServiceApi::complete_current_phase(&hub, &workflow.id)
            .await
            .expect("completion should remain successful even when auto-prune fails");
    }
    assert_eq!(workflow.status, WorkflowStatus::Completed);

    let checkpoints = WorkflowServiceApi::list_checkpoints(&hub, &workflow.id).await.expect("list checkpoints");
    assert_eq!(checkpoints.len(), 5, "failed auto-prune should not remove checkpoints when completion succeeds");
}

#[tokio::test]
async fn file_hub_uses_custom_pipeline_from_workflow_config_v2() {
    let temp = tempfile::tempdir().expect("tempdir");
    ensure_test_config_env();
    let mut workflow_config = crate::builtin_workflow_config();
    workflow_config.default_workflow_ref = "xhigh-dev".to_string();
    workflow_config.tools_allowlist = vec!["cargo".to_string()];
    // v0.6 kernel-purification: the kernel ships an empty phase_catalog. Declare
    // the phases this custom pipeline references so the workflow config validates.
    for phase_id in ["requirements", "implementation", "code-review", "testing"] {
        workflow_config.phase_catalog.insert(
            phase_id.to_string(),
            crate::PhaseUiDefinition {
                label: phase_id.to_string(),
                description: String::new(),
                category: "build".to_string(),
                icon: None,
                docs_url: None,
                tags: Vec::new(),
                visible: true,
            },
        );
    }
    workflow_config.phase_catalog.insert(
        "qa-signoff".to_string(),
        crate::PhaseUiDefinition {
            label: "QA Signoff".to_string(),
            description: String::new(),
            category: "qa".to_string(),
            icon: None,
            docs_url: None,
            tags: Vec::new(),
            visible: true,
        },
    );
    workflow_config.workflows.push(crate::WorkflowDefinition {
        id: "xhigh-dev".to_string(),
        name: "XHigh Dev".to_string(),
        description: "custom pipeline".to_string(),
        phases: vec![
            "requirements".to_string().into(),
            "implementation".to_string().into(),
            "code-review".to_string().into(),
            "testing".to_string().into(),
            "qa-signoff".to_string().into(),
        ],
        variables: Vec::new(),
        worktree: None,
        budget: None,
        environment: None,
        workspace: None,
    });
    crate::write_workflow_config(temp.path(), &workflow_config).expect("workflow config should be written");

    let mut runtime_config = crate::AgentRuntimeConfig {
        schema: crate::agent_runtime_config::AGENT_RUNTIME_CONFIG_SCHEMA_ID.to_string(),
        version: crate::agent_runtime_config::AGENT_RUNTIME_CONFIG_VERSION,
        tools_allowlist: vec!["cargo".to_string()],
        agents: std::collections::BTreeMap::new(),
        phases: std::collections::BTreeMap::new(),
        cli_tools: std::collections::BTreeMap::new(),
    };
    runtime_config.agents.insert(
        "default".to_string(),
        crate::AgentProfile {
            name: None,
            description: "default".to_string(),
            system_prompt: "default prompt".to_string(),
            system_prompt_file: None,
            role: None,
            persona: None,
            memory: Default::default(),
            communication: Default::default(),
            mcp_servers: Vec::new(),
            tool_policy: Default::default(),
            approval_policy: None,
            hooks: Default::default(),
            skills: Vec::new(),
            capabilities: Default::default(),
            mcp_server_configs: None,
            structured_capabilities: None,
            project_overrides: None,
            tool: None,
            tool_profile: None,
            model: None,
            fallback_models: Vec::new(),
            reasoning_effort: None,
            permission_mode: None,
            web_search: None,
            network_access: None,
            timeout_secs: None,
            max_attempts: None,
            retry_on: Vec::new(),
            no_retry_on: Vec::new(),
            extra_args: Vec::new(),
            codex_config_overrides: Vec::new(),
            max_continuations: None,
            fallback_tools: Vec::new(),
            models: Vec::new(),
        },
    );
    for (phase_id, directive) in [
        ("default", "default directive"),
        ("requirements", "requirements"),
        ("implementation", "implementation"),
        ("code-review", "review"),
        ("testing", "testing"),
    ] {
        runtime_config.phases.insert(
            phase_id.to_string(),
            crate::PhaseExecutionDefinition {
                mode: crate::PhaseExecutionMode::Agent,
                agent_id: Some("default".to_string()),
                directive: Some(directive.to_string()),
                system_prompt: None,
                runtime: None,
                capabilities: None,
                output_contract: None,
                output_json_schema: None,
                decision_contract: None,
                retry: None,
                skills: Vec::new(),
                command: None,
                manual: None,
                default_tool: None,
                idempotency: crate::Idempotency::Unknown,
                worktree: None,
                evals: None,
            },
        );
    }
    runtime_config.phases.insert(
        "qa-signoff".to_string(),
        crate::PhaseExecutionDefinition {
            mode: crate::PhaseExecutionMode::Manual,
            agent_id: None,
            directive: Some("manual".to_string()),
            system_prompt: None,
            runtime: None,
            capabilities: None,
            output_contract: None,
            output_json_schema: None,
            decision_contract: None,
            retry: None,
            skills: Vec::new(),
            command: None,
            manual: Some(crate::PhaseManualDefinition {
                instructions: "approve qa signoff".to_string(),
                approval_note_required: true,
                timeout_secs: None,
            }),
            default_tool: None,
            idempotency: crate::Idempotency::Unknown,
            worktree: None,
            evals: None,
        },
    );
    crate::write_agent_runtime_config(temp.path(), &runtime_config).expect("agent runtime config should be written");

    let hub = file_hub(temp.path()).expect("create hub");
    let _config_source_seam =
        orchestrator_config::workflow_config::config_source_client::install_yaml_config_source_base(temp.path());
    let workflow = WorkflowServiceApi::run(
        &hub,
        WorkflowRunInput::for_task("TASK-1".to_string(), Some("xhigh-dev".to_string())),
        None,
    )
    .await
    .expect("run workflow");

    let phase_ids = workflow.phases.iter().map(|phase| phase.phase_id.as_str()).collect::<Vec<_>>();
    assert_eq!(phase_ids, vec!["requirements", "implementation", "code-review", "testing", "qa-signoff"]);
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn file_hub_errors_when_requested_pipeline_is_missing_from_config() {
    // This test needs `resolve_pack_registry` to see an empty machine packs
    // dir — otherwise workflow-ref resolution takes the pack-overlay path
    // and produces a different error message. Hold the pack fixture lock so
    // the `workflow::phase_plan` pack tests cannot install fixtures into
    // the shared `~/.animus/packs` while this resolution runs.
    let _packs = crate::test_env::pack_fixture_lock().lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let machine_packs_dir = crate::machine_installed_packs_dir();
    if machine_packs_dir.exists() {
        std::fs::remove_dir_all(&machine_packs_dir).expect("clear shared machine pack fixtures");
    }
    let temp = tempfile::tempdir().expect("tempdir");
    let hub = file_hub(temp.path()).expect("create hub");
    let _config_source_seam =
        orchestrator_config::workflow_config::config_source_client::install_yaml_config_source_base(temp.path());

    let err = WorkflowServiceApi::run(
        &hub,
        WorkflowRunInput::for_task("TASK-1".to_string(), Some("missing-pipeline".to_string())),
        None,
    )
    .await
    .expect_err("unknown pipeline should fail when workflow config exists");

    let message = err.to_string();
    assert!(message.contains("missing-pipeline"));
    // With a config_source base present (the scaffolded workflows), the requested
    // ref is resolved against a real config and fails on the not-found path rather
    // than the "project defines no workflows" path. The intent — an unknown
    // pipeline fails when a workflow config exists — is preserved.
    assert!(message.contains("not found in workflow config"));
}

#[tokio::test]
async fn planning_execute_starts_workflows_with_config_phase_plan() {
    let temp = tempfile::tempdir().expect("tempdir");
    let hub = file_hub(temp.path()).expect("create hub");
    let _config_source_seam =
        orchestrator_config::workflow_config::config_source_client::install_yaml_config_source_base(temp.path());

    let mut workflow_config = crate::load_workflow_config(temp.path(), None).expect("load config");
    workflow_config.workflows.push(crate::WorkflowDefinition {
        id: "planning-custom".to_string(),
        name: "Planning Custom".to_string(),
        description: "planning execution pipeline".to_string(),
        phases: vec![
            "requirements".to_string().into(),
            "testing".to_string().into(),
            "implementation".to_string().into(),
        ],
        variables: Vec::new(),
        worktree: None,
        budget: None,
        environment: None,
        workspace: None,
    });
    crate::write_workflow_config(temp.path(), &workflow_config).expect("write config");
    let _config_source_seam_after_write =
        orchestrator_config::workflow_config::config_source_client::install_yaml_config_source_base(temp.path());

    PlanningServiceApi::draft_vision(
        &hub,
        VisionDraftInput {
            project_name: Some("Planning Config Parity".to_string()),
            problem_statement: "Need config-first planning workflow start".to_string(),
            target_users: vec!["Engineers".to_string()],
            goals: vec!["Start workflows from planning execution".to_string()],
            constraints: vec![],
            value_proposition: Some("One phase source for run and planning".to_string()),
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
            title: "Use configured planning pipeline".to_string(),
            description: "Planning execution should honor workflow config pipeline phases.".to_string(),
            body: None,
            legacy_id: None,
            category: None,
            requirement_type: None,
            acceptance_criteria: vec![
                "Workflow starts with configured phase order.".to_string(),
                "Planning and workflow run resolve the same phase source.".to_string(),
            ],
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
            start_workflows: true,
            workflow_ref: Some("planning-custom".to_string()),
            include_wont: false,
        },
    )
    .await
    .expect("execute requirements");
    assert!(!execution.workflow_ids_started.is_empty());

    for workflow_id in &execution.workflow_ids_started {
        let workflow = WorkflowServiceApi::get(&hub, workflow_id).await.expect("workflow should exist");
        assert_eq!(
            workflow.workflow_ref.as_deref(),
            Some("planning-custom"),
            "workflow should preserve configured workflow ref"
        );

        let phase_ids = workflow.phases.iter().map(|phase| phase.phase_id.as_str()).collect::<Vec<_>>();
        assert_eq!(
            phase_ids,
            vec!["requirements", "testing", "implementation"],
            "planning-started workflows should use configured phase order"
        );
    }
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

    let prioritized = TaskServiceApi::list_prioritized(&hub).await.expect("prioritized list");
    assert_eq!(prioritized.first().map(|task| task.id.as_str()), Some(high.id.as_str()));

    let updated = TaskServiceApi::add_checklist_item(&hub, &high.id, "Write tests".to_string(), "tester".to_string())
        .await
        .expect("add checklist");
    assert_eq!(updated.checklist.len(), 1);

    let item_id = updated.checklist[0].id.clone();
    let updated = TaskServiceApi::update_checklist_item(&hub, &high.id, &item_id, true, "tester".to_string())
        .await
        .expect("update checklist");
    assert!(updated.checklist[0].completed);

    let with_dep =
        TaskServiceApi::add_dependency(&hub, &high.id, &low.id, DependencyType::BlockedBy, "tester".to_string())
            .await
            .expect("add dependency");
    assert_eq!(with_dep.dependencies.len(), 1);

    let without_dep = TaskServiceApi::remove_dependency(&hub, &high.id, &low.id, "tester".to_string())
        .await
        .expect("remove dependency");
    assert!(without_dep.dependencies.is_empty());

    let stats = TaskServiceApi::statistics(&hub).await.expect("task statistics");
    assert_eq!(stats.total, 2);
}

#[tokio::test]
async fn task_priority_policy_reports_active_high_budget_overflow() {
    let hub = InMemoryServiceHub::new();
    let first = TaskServiceApi::create(
        &hub,
        TaskCreateInput {
            title: "High one".to_string(),
            description: String::new(),
            task_type: Some(TaskType::Feature),
            priority: Some(Priority::High),
            created_by: Some("tester".to_string()),
            tags: vec![],
            linked_requirements: vec![],
            linked_architecture_entities: vec![],
        },
    )
    .await
    .expect("create first high task");
    let second = TaskServiceApi::create(
        &hub,
        TaskCreateInput {
            title: "High two".to_string(),
            description: String::new(),
            task_type: Some(TaskType::Feature),
            priority: Some(Priority::High),
            created_by: Some("tester".to_string()),
            tags: vec![],
            linked_requirements: vec![],
            linked_architecture_entities: vec![],
        },
    )
    .await
    .expect("create second high task");
    let done = TaskServiceApi::create(
        &hub,
        TaskCreateInput {
            title: "High done".to_string(),
            description: String::new(),
            task_type: Some(TaskType::Feature),
            priority: Some(Priority::High),
            created_by: Some("tester".to_string()),
            tags: vec![],
            linked_requirements: vec![],
            linked_architecture_entities: vec![],
        },
    )
    .await
    .expect("create terminal high task");
    TaskServiceApi::set_status(&hub, &first.id, TaskStatus::Ready, false).await.expect("set first status");
    TaskServiceApi::set_status(&hub, &second.id, TaskStatus::InProgress, false).await.expect("set second status");
    TaskServiceApi::set_status(&hub, &done.id, TaskStatus::Done, false).await.expect("set terminal status");

    let tasks = TaskServiceApi::list(&hub).await.expect("list tasks");
    let report = evaluate_task_priority_policy(&tasks, 20).expect("evaluate policy");
    assert_eq!(report.total_tasks, 3);
    assert_eq!(report.active_tasks, 2);
    assert_eq!(report.high_budget_limit, 0);
    assert_eq!(report.active_by_priority.high, 2);
    assert_eq!(report.high_budget_overflow, 2);
    assert!(!report.high_budget_compliant);
}

#[tokio::test]
async fn task_priority_rebalance_plan_is_deterministic_and_budget_compliant() {
    let hub = InMemoryServiceHub::new();
    let blocked = TaskServiceApi::create(
        &hub,
        TaskCreateInput {
            title: "Blocked".to_string(),
            description: String::new(),
            task_type: Some(TaskType::Feature),
            priority: Some(Priority::High),
            created_by: Some("tester".to_string()),
            tags: vec![],
            linked_requirements: vec![],
            linked_architecture_entities: vec![],
        },
    )
    .await
    .expect("create blocked task");
    TaskServiceApi::set_status(&hub, &blocked.id, TaskStatus::Blocked, false).await.expect("set blocked status");

    let essential = TaskServiceApi::create(
        &hub,
        TaskCreateInput {
            title: "Essential".to_string(),
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
    .expect("create essential task");
    TaskServiceApi::set_status(&hub, &essential.id, TaskStatus::Ready, false).await.expect("set essential status");

    let early = TaskServiceApi::create(
        &hub,
        TaskCreateInput {
            title: "Early".to_string(),
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
    .expect("create early task");
    TaskServiceApi::set_status(&hub, &early.id, TaskStatus::InProgress, false).await.expect("set early status");
    let mut early_with_deadline = TaskServiceApi::get(&hub, &early.id).await.expect("load early");
    early_with_deadline.deadline = Some("2026-03-01T09:00:00Z".to_string());
    TaskServiceApi::replace(&hub, early_with_deadline).await.expect("set early deadline");

    let late = TaskServiceApi::create(
        &hub,
        TaskCreateInput {
            title: "Late".to_string(),
            description: String::new(),
            task_type: Some(TaskType::Feature),
            priority: Some(Priority::High),
            created_by: Some("tester".to_string()),
            tags: vec![],
            linked_requirements: vec![],
            linked_architecture_entities: vec![],
        },
    )
    .await
    .expect("create late task");
    TaskServiceApi::set_status(&hub, &late.id, TaskStatus::InProgress, false).await.expect("set late status");
    let mut late_with_deadline = TaskServiceApi::get(&hub, &late.id).await.expect("load late");
    late_with_deadline.deadline = Some("2026-03-10T09:00:00Z".to_string());
    TaskServiceApi::replace(&hub, late_with_deadline).await.expect("set late deadline");

    let nice_to_have = TaskServiceApi::create(
        &hub,
        TaskCreateInput {
            title: "Nice to have".to_string(),
            description: String::new(),
            task_type: Some(TaskType::Feature),
            priority: Some(Priority::High),
            created_by: Some("tester".to_string()),
            tags: vec![],
            linked_requirements: vec![],
            linked_architecture_entities: vec![],
        },
    )
    .await
    .expect("create nice-to-have task");
    TaskServiceApi::set_status(&hub, &nice_to_have.id, TaskStatus::InProgress, false)
        .await
        .expect("set nice-to-have status");

    let low_existing = TaskServiceApi::create(
        &hub,
        TaskCreateInput {
            title: "Existing low".to_string(),
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
    .expect("create low task");
    TaskServiceApi::set_status(&hub, &low_existing.id, TaskStatus::Backlog, false).await.expect("set low status");

    let tasks = TaskServiceApi::list(&hub).await.expect("list tasks");
    let options = TaskPriorityRebalanceOptions {
        high_budget_percent: 40,
        essential_task_ids: vec![essential.id.clone()],
        nice_to_have_task_ids: vec![nice_to_have.id.clone()],
    };
    let plan = plan_task_priority_rebalance(&tasks, options.clone()).expect("build plan");
    let repeat_plan = plan_task_priority_rebalance(&tasks, options).expect("build repeated plan");
    assert_eq!(plan, repeat_plan);

    assert_eq!(plan.after.active_by_priority.high, 2);
    assert_eq!(plan.after.active_by_priority.critical, 1);
    assert!(plan.after.high_budget_compliant);

    let mut resulting_priorities: std::collections::HashMap<String, Priority> =
        tasks.iter().map(|task| (task.id.clone(), task.priority)).collect();
    for change in &plan.changes {
        resulting_priorities.insert(change.task_id.clone(), change.to);
    }

    assert_eq!(resulting_priorities.get(&blocked.id), Some(&Priority::Critical));
    assert_eq!(resulting_priorities.get(&essential.id), Some(&Priority::High));
    assert_eq!(resulting_priorities.get(&early.id), Some(&Priority::High));
    assert_eq!(resulting_priorities.get(&late.id), Some(&Priority::Medium));
    assert_eq!(resulting_priorities.get(&nice_to_have.id), Some(&Priority::Low));
    assert_eq!(resulting_priorities.get(&low_existing.id), Some(&Priority::Low));
}

#[tokio::test]
async fn task_priority_rebalance_rejects_conflicting_override_task_ids() {
    let hub = InMemoryServiceHub::new();
    let task = TaskServiceApi::create(
        &hub,
        TaskCreateInput {
            title: "Conflicting override".to_string(),
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
    .expect("create task");
    TaskServiceApi::set_status(&hub, &task.id, TaskStatus::Ready, false).await.expect("set status");

    let tasks = TaskServiceApi::list(&hub).await.expect("list tasks");
    let error = plan_task_priority_rebalance(
        &tasks,
        TaskPriorityRebalanceOptions {
            high_budget_percent: 20,
            essential_task_ids: vec![task.id.clone()],
            nice_to_have_task_ids: vec![task.id.clone()],
        },
    )
    .expect_err("conflicting overrides should fail");
    let message = error.to_string();
    assert!(message.contains("conflicting task ids provided in overrides"));
    assert!(message.contains(task.id.as_str()));
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
    assert!(error.to_string().contains("linked architecture entity not found"));
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
        TaskFilter { linked_architecture_entity: Some("arch-api".to_string()), ..TaskFilter::default() },
    )
    .await
    .expect("filter should succeed");
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].id, linked.id);
}

#[tokio::test]
async fn workflow_service_exposes_decisions_and_checkpoints() {
    let temp = tempfile::tempdir().expect("tempdir");
    let hub = file_hub(temp.path()).expect("create hub");
    let _config_source_seam =
        orchestrator_config::workflow_config::config_source_client::install_yaml_config_source_base(temp.path());
    let workflow = WorkflowServiceApi::run(
        &hub,
        WorkflowRunInput::for_task("TASK-123".to_string(), Some("standard".to_string())),
        None,
    )
    .await
    .expect("run workflow");

    let workflow =
        WorkflowServiceApi::complete_current_phase(&hub, &workflow.id).await.expect("complete current phase");
    assert!(!workflow.decision_history.is_empty());

    let decisions = WorkflowServiceApi::decisions(&hub, &workflow.id).await.expect("get decisions");
    assert!(!decisions.is_empty());

    let checkpoints = WorkflowServiceApi::list_checkpoints(&hub, &workflow.id).await.expect("list checkpoints");
    assert_eq!(checkpoints, vec![1, 2]);

    let checkpoint = WorkflowServiceApi::get_checkpoint(&hub, &workflow.id, 1).await.expect("get checkpoint");
    assert_eq!(checkpoint.id, workflow.id);
}

#[tokio::test]
async fn task_service_query_returns_stable_paginated_priority_order() {
    let hub = InMemoryServiceHub::new();

    let critical = TaskServiceApi::create(
        &hub,
        TaskCreateInput {
            title: "Critical".to_string(),
            description: String::new(),
            task_type: Some(TaskType::Feature),
            priority: Some(Priority::Critical),
            created_by: Some("tester".to_string()),
            tags: vec![],
            linked_requirements: vec![],
            linked_architecture_entities: vec![],
        },
    )
    .await
    .expect("critical task should be created");

    let high = TaskServiceApi::create(
        &hub,
        TaskCreateInput {
            title: "High".to_string(),
            description: String::new(),
            task_type: Some(TaskType::Feature),
            priority: Some(Priority::High),
            created_by: Some("tester".to_string()),
            tags: vec![],
            linked_requirements: vec![],
            linked_architecture_entities: vec![],
        },
    )
    .await
    .expect("high task should be created");

    TaskServiceApi::create(
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
    .expect("low task should be created");

    let page = TaskServiceApi::query(
        &hub,
        TaskQuery {
            filter: Default::default(),
            page: ListPageRequest { limit: Some(1), offset: 1 },
            sort: TaskQuerySort::Priority,
        },
    )
    .await
    .expect("task query should succeed");

    assert_eq!(page.total, 3);
    assert_eq!(page.returned, 1);
    assert!(page.has_more);
    assert_eq!(page.next_offset, Some(2));
    assert_eq!(page.items[0].id, high.id);
    assert_ne!(page.items[0].id, critical.id);
}

#[tokio::test]
async fn planning_service_query_filters_and_sorts_requirements() {
    let hub = InMemoryServiceHub::new();
    let now = chrono::Utc::now();

    PlanningServiceApi::upsert_requirement(
        &hub,
        RequirementItem {
            id: "REQ-010".to_string(),
            title: "GraphQL query cleanup".to_string(),
            description: "Remove duplicate query adapters".to_string(),
            body: None,
            legacy_id: None,
            category: Some("integration".to_string()),
            requirement_type: Some(RequirementType::Technical),
            acceptance_criteria: vec!["One path for GraphQL queries".to_string()],
            priority: RequirementPriority::Must,
            status: RequirementStatus::Draft,
            source: "test".to_string(),
            tags: vec!["graphql".to_string()],
            links: Default::default(),
            comments: Vec::new(),
            relative_path: None,
            linked_task_ids: vec!["TASK-590".to_string()],
            created_at: now,
            updated_at: now,
        },
    )
    .await
    .expect("graphql requirement should be created");

    PlanningServiceApi::upsert_requirement(
        &hub,
        RequirementItem {
            id: "REQ-011".to_string(),
            title: "CLI help drift".to_string(),
            description: "Make docs match implementation".to_string(),
            body: None,
            legacy_id: None,
            category: Some("integration".to_string()),
            requirement_type: Some(RequirementType::Technical),
            acceptance_criteria: vec!["Help text is accurate".to_string()],
            priority: RequirementPriority::Should,
            status: RequirementStatus::Draft,
            source: "test".to_string(),
            tags: vec!["cli".to_string()],
            links: Default::default(),
            comments: Vec::new(),
            relative_path: None,
            linked_task_ids: vec!["TASK-592".to_string()],
            created_at: now,
            updated_at: now,
        },
    )
    .await
    .expect("cli requirement should be created");

    let page = PlanningServiceApi::query(
        &hub,
        RequirementQuery {
            filter: RequirementFilter {
                status: Some(RequirementStatus::Draft),
                priority: None,
                category: Some("integration".to_string()),
                requirement_type: Some(RequirementType::Technical),
                tags: None,
                linked_task_id: None,
                search_text: Some("query".to_string()),
            },
            page: ListPageRequest::unbounded(),
            sort: RequirementQuerySort::Priority,
        },
    )
    .await
    .expect("requirement query should succeed");

    assert_eq!(page.total, 1);
    assert_eq!(page.items[0].id, "REQ-010");
}

#[tokio::test]
async fn workflow_service_query_filters_by_status_and_reference() {
    let hub = InMemoryServiceHub::new();

    let first = WorkflowServiceApi::run(
        &hub,
        WorkflowRunInput::for_task("TASK-100".to_string(), Some("standard".to_string())),
        None,
    )
    .await
    .expect("first workflow should start");
    let second = WorkflowServiceApi::run(
        &hub,
        WorkflowRunInput::for_task("TASK-200".to_string(), Some("ui-ux".to_string())),
        None,
    )
    .await
    .expect("second workflow should start");

    WorkflowServiceApi::pause(&hub, &first.id).await.expect("first workflow should pause");
    WorkflowServiceApi::cancel(&hub, &second.id).await.expect("second workflow should cancel");

    let page = WorkflowServiceApi::query(
        &hub,
        WorkflowQuery {
            filter: WorkflowFilter {
                status: Some(WorkflowStatus::Paused),
                workflow_ref: Some("standard".to_string()),
                task_id: None,
                phase_id: None,
                search_text: None,
            },
            page: ListPageRequest::unbounded(),
            sort: WorkflowQuerySort::Id,
        },
    )
    .await
    .expect("workflow query should succeed");

    assert_eq!(page.total, 1);
    assert_eq!(page.items[0].id, first.id);
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
            goals: vec!["Draft requirements from vision".to_string(), "Execute tasks from requirements".to_string()],
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
        RequirementsDraftInput { include_codebase_scan: false, append_only: true, max_requirements: 4 },
    )
    .await
    .expect("draft requirements");
    assert!(drafted.appended_count > 0);

    let refined = PlanningServiceApi::refine_requirements(
        &hub,
        RequirementsRefineInput { requirement_ids: vec![], focus: Some("testability".to_string()) },
    )
    .await
    .expect("refine requirements");
    assert!(!refined.is_empty());
    assert!(refined.iter().all(|item| item.status == RequirementStatus::Refined));

    let execution = PlanningServiceApi::execute_requirements(
        &hub,
        RequirementsExecutionInput {
            requirement_ids: vec![],
            start_workflows: true,
            workflow_ref: Some("standard".to_string()),
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
        RequirementsDraftInput { include_codebase_scan: false, append_only: true, max_requirements: 1 },
    )
    .await
    .expect("draft requirements");

    assert!(drafted.requirements.iter().any(|requirement| requirement.source == "vision-constraint"));
    assert!(drafted
        .requirements
        .iter()
        .any(|requirement| { requirement.title.to_ascii_lowercase().contains("next.js app router with typescript") }));
    assert!(drafted
        .requirements
        .iter()
        .any(|requirement| { requirement.title.to_ascii_lowercase().contains("primary database must be postgresql") }));
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
            workflow_ref: None,
            include_wont: false,
        },
    )
    .await
    .expect_err("execution should be blocked by missing constraint coverage");

    assert!(error.to_string().to_ascii_lowercase().contains("vision constraints missing from requirements"));
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

    PlanningServiceApi::draft_requirements(&hub, RequirementsDraftInput::default()).await.expect("draft requirements");

    let scoped = scoped_ao_root(temp.path());
    let vision_path = scoped.join("docs").join("product-vision.md");
    let vision_json_path = scoped.join("docs").join("vision.json");
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

    let requirement = PlanningServiceApi::upsert_requirement(&hub, requirement).await.expect("upsert requirement");

    let refined = PlanningServiceApi::refine_requirements(
        &hub,
        RequirementsRefineInput {
            requirement_ids: vec![requirement.id.clone()],
            focus: Some("validation".to_string()),
        },
    )
    .await
    .expect("refine requirements");

    let refined_requirement =
        refined.iter().find(|item| item.id == requirement.id).expect("requirement should be refined");
    assert!(refined_requirement.tags.iter().any(|tag| tag == "needs-research"));
    assert!(refined_requirement
        .acceptance_criteria
        .iter()
        .any(|criterion| { criterion.to_ascii_lowercase().contains("research findings documented") }));

    let execution = PlanningServiceApi::execute_requirements(
        &hub,
        RequirementsExecutionInput {
            requirement_ids: vec![requirement.id.clone()],
            start_workflows: false,
            workflow_ref: None,
            include_wont: false,
        },
    )
    .await
    .expect("execute requirements");
    let task_id = execution.task_ids_created.first().expect("task should be created");
    let task = TaskServiceApi::get(&hub, task_id).await.expect("task should exist");
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
            workflow_ref: None,
            include_wont: false,
        },
    )
    .await
    .expect("execute requirements");
    assert!(!execution.task_ids_created.is_empty());

    let updated_requirement =
        PlanningServiceApi::get_requirement(&hub, &requirement.id).await.expect("requirement should exist");
    assert_eq!(updated_requirement.status, RequirementStatus::Planned);
    assert!(updated_requirement.tags.iter().any(|tag| tag.eq_ignore_ascii_case("needs-research")));
    assert!(updated_requirement
        .acceptance_criteria
        .iter()
        .any(|criterion| { criterion.to_ascii_lowercase().contains("research findings documented") }));
    assert!(updated_requirement
        .acceptance_criteria
        .iter()
        .any(|criterion| { criterion.to_ascii_lowercase().contains("automated test coverage") }));
    assert!(updated_requirement.comments.iter().any(|comment| comment.phase.as_deref() == Some("po-review")));
    assert!(updated_requirement.comments.iter().any(|comment| comment.phase.as_deref() == Some("em-review")));
    assert!(updated_requirement.comments.iter().any(|comment| comment.phase.as_deref() == Some("rework")));
    assert!(updated_requirement.comments.iter().any(|comment| comment.phase.as_deref() == Some("approved")));

    let created_task_id = execution.task_ids_created.first().expect("task should exist");
    let task = TaskServiceApi::get(&hub, created_task_id).await.expect("task should be loadable");
    assert!(task.workflow_metadata.requires_architecture);
    assert!(!task.checklist.is_empty());
    assert!(task.checklist.iter().any(|item| { item.description.to_ascii_lowercase().contains("code review gate") }));
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
            goals: vec!["Deliver end-to-end workflow".to_string(), "Ship with tests and review gates".to_string()],
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
    let requirement_id = drafted.requirements.first().expect("requirement should exist").id.clone();

    let execution = PlanningServiceApi::execute_requirements(
        &hub,
        RequirementsExecutionInput {
            requirement_ids: vec![requirement_id],
            start_workflows: false,
            workflow_ref: None,
            include_wont: false,
        },
    )
    .await
    .expect("execute requirements");

    assert!(!execution.task_ids_created.is_empty());
    for task_id in execution.task_ids_created {
        let task = TaskServiceApi::get(&hub, &task_id).await.expect("task should exist");
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
    let mut requirement = drafted.requirements.first().cloned().expect("requirement should exist");
    requirement.priority = RequirementPriority::Wont;
    PlanningServiceApi::upsert_requirement(&hub, requirement.clone()).await.expect("upsert requirement");

    let error = PlanningServiceApi::execute_requirements(
        &hub,
        RequirementsExecutionInput {
            requirement_ids: vec![requirement.id],
            start_workflows: false,
            workflow_ref: None,
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
    let mut requirement = drafted.requirements.first().cloned().expect("requirement should exist");
    requirement.priority = RequirementPriority::Wont;
    PlanningServiceApi::upsert_requirement(&hub, requirement.clone()).await.expect("upsert requirement");

    let result = PlanningServiceApi::execute_requirements(
        &hub,
        RequirementsExecutionInput {
            requirement_ids: vec![requirement.id],
            start_workflows: false,
            workflow_ref: None,
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

    for (index, (requirement_priority, expected_task_priority, label)) in cases.into_iter().enumerate() {
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
                acceptance_criteria: vec![format!("Task priority generated for `{label}` is deterministic")],
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
                workflow_ref: None,
                include_wont: true,
            },
        )
        .await
        .expect("execute requirements");
        assert!(!execution.task_ids_created.is_empty(), "expected tasks for case {index} ({label})");

        for task_id in execution.task_ids_created {
            let task = TaskServiceApi::get(&hub, &task_id).await.expect("task should exist");
            assert_eq!(task.priority, expected_task_priority, "unexpected task priority for case {index} ({label})");
        }
    }
}

fn assert_unblocked_task_state(task: &OrchestratorTask, expected_status: TaskStatus) {
    assert_eq!(task.status, expected_status);
    assert!(!task.paused);
    assert!(task.blocked_reason.is_none());
    assert!(task.blocked_at.is_none());
    assert!(task.blocked_phase.is_none());
    assert!(task.blocked_by.is_none());
}

#[tokio::test]
async fn set_status_from_blocked_to_ready_clears_paused_and_blocked_fields() {
    let hub = InMemoryServiceHub::new();
    let created = TaskServiceApi::create(
        &hub,
        TaskCreateInput {
            title: "Paused state reset regression".to_string(),
            description: "Ensure paused flag clears when unblocking".to_string(),
            task_type: Some(TaskType::Bugfix),
            priority: Some(Priority::Medium),
            created_by: Some("tester".to_string()),
            tags: vec![],
            linked_requirements: vec![],
            linked_architecture_entities: vec![],
        },
    )
    .await
    .expect("create task");

    let blocked =
        TaskServiceApi::set_status(&hub, &created.id, TaskStatus::Blocked, false).await.expect("set blocked status");
    assert!(blocked.paused);
    assert!(blocked.blocked_reason.is_some());
    assert!(blocked.blocked_at.is_some());

    let ready =
        TaskServiceApi::set_status(&hub, &created.id, TaskStatus::Ready, false).await.expect("set ready status");
    assert_unblocked_task_state(&ready, TaskStatus::Ready);
}

#[tokio::test]
async fn update_status_from_on_hold_to_in_progress_clears_paused_and_blocked_fields() {
    let hub = InMemoryServiceHub::new();
    let created = TaskServiceApi::create(
        &hub,
        TaskCreateInput {
            title: "Paused state reset via update".to_string(),
            description: "Ensure update path clears paused and blocked fields".to_string(),
            task_type: Some(TaskType::Bugfix),
            priority: Some(Priority::Medium),
            created_by: Some("tester".to_string()),
            tags: vec![],
            linked_requirements: vec![],
            linked_architecture_entities: vec![],
        },
    )
    .await
    .expect("create task");

    // Test that transition from Backlog to InProgress works and clears blocked fields
    // Note: OnHold -> InProgress is now blocked by AC2 validation (must be Ready or Backlog first)
    let in_progress = TaskServiceApi::update(
        &hub,
        &created.id,
        TaskUpdateInput {
            title: None,
            description: None,
            priority: None,
            status: Some(TaskStatus::InProgress),
            assignee: None,
            tags: None,
            updated_by: Some("tester".to_string()),
            deadline: None,
            linked_architecture_entities: None,
        },
    )
    .await
    .expect("update status to in-progress");

    assert_unblocked_task_state(&in_progress, TaskStatus::InProgress);
    assert!(in_progress.metadata.started_at.is_some(), "in-progress transition should set started_at");
}

// ---- v0.4.19 subject plugin fallback regression --------------------------
//
// The v0.4.18 hotfix wired `.with_plugin_fallback()` onto
// `FileServiceHub::subject_resolver()` and `project_adapter()` but the
// `InMemoryServiceHub` variants were left unwired. The tests below assert
// that the InMemoryServiceHub now activates the plugin fallback whenever a
// project_root is supplied — matching the production hub's behavior so
// future regressions stay symmetric.

#[tokio::test]
async fn in_memory_hub_without_project_root_skips_plugin_fallback() {
    let hub = InMemoryServiceHub::new();
    // Without a project root the resolver should not engage the plugin
    // fallback; an unknown subject kind falls through to a hard error
    // from the in-tree adapters instead of attempting plugin discovery.
    let subject = protocol::orchestrator::SubjectRef::task("MISSING-TASK".to_string());
    let err = hub.subject_resolver().resolve_subject_context(&subject, None, None).await.expect_err("expected error");
    let msg = err.to_string();
    assert!(
        !msg.contains("no subject_backend plugin"),
        "hub without project_root must not invoke plugin fallback, got: {msg}"
    );
}

#[tokio::test]
async fn in_memory_hub_with_project_root_engages_plugin_fallback() {
    let temp = tempfile::tempdir().expect("tempdir");
    // No plugins are installed in the temp project, so the fallback runs
    // but finds no candidate. The resulting error must mention the plugin
    // fallback path — that is the proof the wiring took effect.
    let hub = InMemoryServiceHub::new().with_project_root(temp.path());
    let subject = protocol::orchestrator::SubjectRef::task("MISSING-TASK".to_string());
    let err = hub.subject_resolver().resolve_subject_context(&subject, None, None).await.expect_err("expected error");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("no subject_backend plugin") || msg.contains("plugin"),
        "hub with project_root must invoke plugin fallback, got: {msg}"
    );
}

#[tokio::test]
async fn file_hub_subject_resolver_engages_plugin_fallback() {
    let temp = tempfile::tempdir().expect("tempdir");
    let hub = file_hub(temp.path()).expect("create hub");
    let subject = protocol::orchestrator::SubjectRef::task("MISSING-TASK".to_string());
    let err = hub.subject_resolver().resolve_subject_context(&subject, None, None).await.expect_err("expected error");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("no subject_backend plugin") || msg.contains("plugin"),
        "FileServiceHub must invoke plugin fallback for unknown task ids, got: {msg}"
    );
}

fn requirement_fixture(id: &str, title: &str) -> RequirementItem {
    let now = chrono::Utc::now();
    RequirementItem {
        id: id.to_string(),
        title: title.to_string(),
        description: format!("{title} description"),
        body: None,
        legacy_id: None,
        category: None,
        requirement_type: None,
        acceptance_criteria: vec!["done".to_string()],
        priority: RequirementPriority::Should,
        status: RequirementStatus::Draft,
        source: "test".to_string(),
        tags: vec![],
        links: Default::default(),
        comments: Vec::new(),
        relative_path: None,
        linked_task_ids: vec![],
        created_at: now,
        updated_at: now,
    }
}

#[tokio::test]
async fn file_hub_delete_requirement_removes_sqlite_row_and_does_not_resurrect() {
    let temp = tempfile::tempdir().expect("tempdir");
    let hub = file_hub(temp.path()).expect("create hub");

    let requirement = PlanningServiceApi::upsert_requirement(&hub, requirement_fixture("REQ-DEL-1", "Delete me"))
        .await
        .expect("upsert requirement");
    let id = requirement.id.clone();

    PlanningServiceApi::delete_requirement(&hub, &id).await.expect("delete requirement");

    let stored = crate::workflow::load_all_requirements(temp.path()).expect("load requirements from sqlite");
    assert!(!stored.contains_key(&id), "delete must remove the sqlite row");

    PlanningServiceApi::upsert_requirement(&hub, requirement_fixture("REQ-DEL-2", "Keep me"))
        .await
        .expect("upsert second requirement");

    PlanningServiceApi::get_requirement(&hub, &id).await.expect_err("deleted requirement must stay deleted");
    let listed = PlanningServiceApi::list_requirements(&hub).await.expect("list requirements");
    assert!(listed.iter().all(|req| req.id != id), "deleted requirement must not resurrect after later mutations");
}

#[tokio::test]
async fn execute_requirements_skips_tasks_with_active_workflow_on_file_hub() {
    let temp = tempfile::tempdir().expect("tempdir");
    let hub = file_hub(temp.path()).expect("create hub");
    let _config_source_seam =
        orchestrator_config::workflow_config::config_source_client::install_yaml_config_source_base(temp.path());

    let task = TaskServiceApi::create(
        &hub,
        TaskCreateInput {
            title: "Implement signup flow".to_string(),
            description: "Implement user signup with verification and retries.".to_string(),
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

    let now = chrono::Utc::now();
    let requirement = PlanningServiceApi::upsert_requirement(
        &hub,
        RequirementItem {
            id: String::new(),
            title: "User signup flow".to_string(),
            description: "Implement user signup with verification and retries.".to_string(),
            body: None,
            legacy_id: None,
            category: None,
            requirement_type: None,
            acceptance_criteria: vec![
                "Includes automated test coverage for core acceptance criteria.".to_string(),
                "Error handling and retry behavior is defined.".to_string(),
            ],
            priority: RequirementPriority::Should,
            status: RequirementStatus::Draft,
            source: "manual".to_string(),
            tags: vec![],
            links: crate::types::RequirementLinks::default(),
            comments: vec![],
            relative_path: None,
            linked_task_ids: vec![task.id.clone()],
            created_at: now,
            updated_at: now,
        },
    )
    .await
    .expect("upsert requirement");

    let active = WorkflowServiceApi::run(&hub, WorkflowRunInput::for_task(task.id.clone(), None), None)
        .await
        .expect("start workflow for task");
    assert_eq!(active.status, WorkflowStatus::Running);

    let execution = PlanningServiceApi::execute_requirements(
        &hub,
        RequirementsExecutionInput {
            requirement_ids: vec![requirement.id.clone()],
            start_workflows: true,
            workflow_ref: None,
            include_wont: false,
        },
    )
    .await
    .expect("execute requirements");

    assert!(execution.task_ids_reused.contains(&task.id), "existing linked task should be reused");
    assert!(
        execution.workflow_ids_started.is_empty(),
        "task with an active workflow must not get a duplicate workflow, started: {:?}",
        execution.workflow_ids_started
    );
}

#[tokio::test]
async fn manual_phase_approval_resume_clears_task_pause_marker() {
    // Codex P2 (ghost-fix wave): manual approve/reject of a paused workflow
    // resumes it via `hub.workflows().resume(...)` instead of the
    // `WorkflowEvent::Resume` arm, so it must clear the "paused by workflow
    // <id>" annotation written to the task by `workflow pause` as well.
    let temp = tempfile::tempdir().expect("tempdir");

    ensure_test_config_env();
    let mut workflow_config = crate::builtin_workflow_config();
    workflow_config.phase_catalog.insert(
        "qa-signoff".to_string(),
        crate::PhaseUiDefinition {
            label: "QA Signoff".to_string(),
            description: String::new(),
            category: "qa".to_string(),
            icon: None,
            docs_url: None,
            tags: Vec::new(),
            visible: true,
        },
    );
    workflow_config.workflows.push(crate::WorkflowDefinition {
        id: "manual-only".to_string(),
        name: "Manual Only".to_string(),
        description: "single manual gate".to_string(),
        phases: vec!["qa-signoff".to_string().into()],
        variables: Vec::new(),
        worktree: None,
        budget: None,
        environment: None,
        workspace: None,
    });
    crate::write_workflow_config(temp.path(), &workflow_config).expect("write workflow config");

    let mut runtime_config = crate::load_agent_runtime_config_or_default(temp.path());
    // v0.6 kernel-purification: the kernel builtin runtime config is empty, so
    // the seam returns nothing. Seed the minimum a valid runtime config needs.
    runtime_config.tools_allowlist = vec!["cargo".to_string()];
    runtime_config.agents.insert(
        "default".to_string(),
        crate::AgentProfile {
            description: "Default".to_string(),
            system_prompt: "Default agent".to_string(),
            ..Default::default()
        },
    );
    runtime_config.phases.insert(
        "qa-signoff".to_string(),
        crate::PhaseExecutionDefinition {
            mode: crate::PhaseExecutionMode::Manual,
            agent_id: None,
            directive: Some("manual".to_string()),
            system_prompt: None,
            runtime: None,
            capabilities: None,
            output_contract: None,
            output_json_schema: None,
            decision_contract: None,
            retry: None,
            skills: Vec::new(),
            command: None,
            manual: Some(crate::PhaseManualDefinition {
                instructions: "approve qa signoff".to_string(),
                approval_note_required: false,
                timeout_secs: None,
            }),
            default_tool: None,
            idempotency: crate::Idempotency::Unknown,
            worktree: None,
            evals: None,
        },
    );
    crate::write_agent_runtime_config(temp.path(), &runtime_config).expect("write runtime config");

    let hub: std::sync::Arc<dyn ServiceHub> = std::sync::Arc::new(file_hub(temp.path()).expect("create hub"));
    let _config_source_seam =
        orchestrator_config::workflow_config::config_source_client::install_yaml_config_source_base(temp.path());
    let task = hub
        .tasks()
        .create(TaskCreateInput {
            title: "Manual gate".to_string(),
            description: String::new(),
            task_type: None,
            priority: None,
            created_by: None,
            tags: Vec::new(),
            linked_requirements: Vec::new(),
            linked_architecture_entities: Vec::new(),
        })
        .await
        .expect("create task");
    let workflow = hub
        .workflows()
        .run(WorkflowRunInput::for_task(task.id.clone(), Some("manual-only".to_string())), None)
        .await
        .expect("run workflow");
    assert_eq!(workflow.current_phase.as_deref(), Some("qa-signoff"));

    let root = temp.path().to_string_lossy().to_string();
    crate::dispatch_workflow_event(
        hub.clone(),
        &root,
        crate::WorkflowEvent::Pause { workflow_id: workflow.id.clone(), reason_detail: None },
    )
    .await
    .expect("pause workflow");
    let paused_task = hub.tasks().get(&task.id).await.expect("task after pause");
    assert_eq!(
        paused_task.blocked_reason.as_deref(),
        Some(format!("paused by workflow {}", workflow.id).as_str()),
        "pause must annotate the task"
    );

    crate::dispatch_workflow_event(
        hub.clone(),
        &root,
        crate::WorkflowEvent::ApproveManualPhase {
            workflow_id: workflow.id.clone(),
            phase_id: "qa-signoff".to_string(),
            note: None,
        },
    )
    .await
    .expect("approve manual phase");

    let resumed_task = hub.tasks().get(&task.id).await.expect("task after approval");
    assert!(
        resumed_task.blocked_reason.is_none(),
        "manual approval resume must clear the pause annotation, got: {:?}",
        resumed_task.blocked_reason
    );
    assert!(resumed_task.blocked_by.is_none());
}
