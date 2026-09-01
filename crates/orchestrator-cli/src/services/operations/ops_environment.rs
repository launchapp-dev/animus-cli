use std::path::Path;

use anyhow::{Context, Result};
use orchestrator_core::{EnvironmentClient, EnvironmentNode, FileServiceHub, OrchestratorWorkflow, ServiceHub};
use serde::Serialize;

use crate::{print_value, EnvironmentCommand};

#[derive(Debug, Serialize)]
pub(crate) struct EnvironmentListView {
    environment: String,
    nodes: Vec<EnvironmentNode>,
}

#[derive(Debug, Serialize)]
pub(crate) struct EnvironmentGetView {
    environment: String,
    node: Option<EnvironmentNode>,
}

#[derive(Debug, Serialize)]
pub(crate) struct EnvironmentTeardownView {
    environment: String,
    deleted: Vec<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct EnvironmentReapView {
    environment: String,
    deleted: Vec<String>,
    kept: Vec<EnvironmentNode>,
    dry_run: bool,
}

fn resolve_environment_client(project_root: &str, environment: Option<&str>) -> Result<EnvironmentClient> {
    EnvironmentClient::resolve(Path::new(project_root), environment.unwrap_or(""))
}

/// Bridge an async future into a sync call (the environment ops are sync but the
/// hub's workflow service is async). Mirrors `environment_client::run_blocking`.
fn run_blocking<F: std::future::Future>(fut: F) -> Result<F::Output> {
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => Ok(tokio::task::block_in_place(|| handle.block_on(fut))),
        Err(_) => {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .context("building tokio runtime for journal liveness read")?;
            Ok(rt.block_on(fut))
        }
    }
}

/// The journal's live (Pending/Running/Paused) run ids, for the reap plugin's
/// owner-known mode. `Some` ONLY when the journal read succeeded — on ANY error
/// returns `None` so the plugin keeps its dead-only default instead of reaping
/// healthy nodes against a possibly-incomplete liveness set. An EMPTY vec is a
/// trustworthy "no live runs" signal and is passed through on purpose.
fn live_run_ids_from_journal(workflows: Result<Vec<OrchestratorWorkflow>>) -> Option<Vec<String>> {
    match workflows {
        Ok(workflows) => Some(workflows.into_iter().map(|workflow| workflow.id).collect()),
        Err(error) => {
            tracing::warn!(error = %error, "failed to read live workflow ids for environment reap; falling back to dead-only reap");
            None
        }
    }
}

/// Build a hub for `project_root` and read the live run-id set. Both hub
/// construction and the journal read fall back to `None` on error.
fn live_run_ids_for_project(project_root: &str) -> Option<Vec<String>> {
    let hub = match FileServiceHub::new(project_root) {
        Ok(hub) => hub,
        Err(error) => {
            tracing::warn!(error = %error, "failed to open service hub for environment reap liveness; falling back to dead-only reap");
            return None;
        }
    };
    let workflows = run_blocking(async { hub.workflows().list_active().await }).and_then(|result| result);
    live_run_ids_from_journal(workflows)
}

pub(crate) fn environment_list_application(
    project_root: &str,
    environment: Option<&str>,
) -> Result<EnvironmentListView> {
    let client = resolve_environment_client(project_root, environment)?;
    Ok(EnvironmentListView { environment: client.plugin_name().to_string(), nodes: client.list_nodes()? })
}

pub(crate) fn environment_get_application(
    project_root: &str,
    environment: Option<&str>,
    id: &str,
) -> Result<EnvironmentGetView> {
    let client = resolve_environment_client(project_root, environment)?;
    Ok(EnvironmentGetView { environment: client.plugin_name().to_string(), node: client.get_node(id)? })
}

pub(crate) fn environment_teardown_application(
    project_root: &str,
    environment: Option<&str>,
    id: &str,
) -> Result<EnvironmentTeardownView> {
    let client = resolve_environment_client(project_root, environment)?;
    Ok(EnvironmentTeardownView { environment: client.plugin_name().to_string(), deleted: client.teardown_node(id)? })
}

pub(crate) fn environment_reap_application(
    project_root: &str,
    environment: Option<&str>,
    all: bool,
    force: bool,
    dry_run: bool,
    older_than_secs: Option<u64>,
) -> Result<EnvironmentReapView> {
    let client = resolve_environment_client(project_root, environment)?;
    // Owner-known mode (TASK-1466): the DEFAULT reap (no --all) hands the
    // plugin the journal's live run-id set so it can also reap healthy (SUCCESS)
    // nodes whose owning workflow is terminal. With --all we preserve the
    // existing force-guarded healthy-orphan semantics exactly (no liveness set).
    // Dry-run gets the same ids so the preview matches the real reap. Any
    // journal-read failure yields `None` (dead-only reap), never `Some([])`.
    let live_run_ids = if all { None } else { live_run_ids_for_project(project_root) };
    let report = client.reap(all, force, dry_run, older_than_secs, live_run_ids)?;
    Ok(EnvironmentReapView {
        environment: client.plugin_name().to_string(),
        deleted: report.deleted,
        kept: report.kept,
        dry_run: report.dry_run,
    })
}

/// Handle `animus environment {list,get,teardown,reap}` by driving the installed
/// environment plugin's node-management surface. Each verb resolves the plugin
/// (the sole installed one, or `--environment <id>`), issues the role call, and
/// emits the `animus.cli.v1` envelope.
pub async fn handle_environment(command: EnvironmentCommand, project_root: &str, json_output: bool) -> Result<()> {
    match command {
        EnvironmentCommand::List(args) => {
            print_value(environment_list_application(project_root, args.environment.as_deref())?, json_output)
        }
        EnvironmentCommand::Get(args) => {
            print_value(environment_get_application(project_root, args.environment.as_deref(), &args.id)?, json_output)
        }
        EnvironmentCommand::Teardown(args) => print_value(
            environment_teardown_application(project_root, args.environment.as_deref(), &args.id)?,
            json_output,
        ),
        EnvironmentCommand::Reap(args) => print_value(
            environment_reap_application(
                project_root,
                args.environment.as_deref(),
                args.all,
                args.force,
                args.dry_run,
                args.older_than_secs,
            )?,
            json_output,
        ),
    }
}

#[cfg(test)]
mod tests {
    use orchestrator_core::EnvironmentNode;
    use serde_json::Value;

    use super::{
        live_run_ids_for_project, live_run_ids_from_journal, EnvironmentGetView, EnvironmentListView,
        EnvironmentReapView, EnvironmentTeardownView,
    };

    fn node() -> EnvironmentNode {
        EnvironmentNode {
            id: "node-1".to_string(),
            name: "animus-run-1".to_string(),
            state: "SUCCESS".to_string(),
            run_id: Some("run-1".to_string()),
            image: Some("animus:latest".to_string()),
            created_at: Some("2026-07-29T00:00:00Z".to_string()),
            orphan: false,
        }
    }

    #[test]
    fn typed_environment_views_preserve_the_existing_json_contract() {
        let list =
            serde_json::to_value(EnvironmentListView { environment: "fake-env".to_string(), nodes: vec![node()] })
                .expect("serialize list");
        assert_eq!(list.pointer("/nodes/0/run_id").and_then(Value::as_str), Some("run-1"));
        assert_eq!(list.pointer("/nodes/0/orphan").and_then(Value::as_bool), Some(false));

        let get = serde_json::to_value(EnvironmentGetView { environment: "fake-env".to_string(), node: Some(node()) })
            .expect("serialize get");
        assert_eq!(get.pointer("/node/image").and_then(Value::as_str), Some("animus:latest"));

        let teardown = serde_json::to_value(EnvironmentTeardownView {
            environment: "fake-env".to_string(),
            deleted: vec!["node-1".to_string()],
        })
        .expect("serialize teardown");
        assert_eq!(teardown.pointer("/deleted/0").and_then(Value::as_str), Some("node-1"));

        let reap = serde_json::to_value(EnvironmentReapView {
            environment: "fake-env".to_string(),
            deleted: vec!["node-2".to_string()],
            kept: vec![node()],
            dry_run: true,
        })
        .expect("serialize reap");
        assert_eq!(reap.get("dry_run").and_then(Value::as_bool), Some(true));
        assert_eq!(reap.pointer("/kept/0/name").and_then(Value::as_str), Some("animus-run-1"));
    }

    #[test]
    fn live_run_ids_from_journal_falls_back_to_none_on_read_error() {
        assert!(live_run_ids_from_journal(Err(anyhow::anyhow!("journal unavailable"))).is_none());
        // An empty-but-successful read is a TRUSTWORTHY "no live runs" signal:
        // it must stay Some([]) so owner-known mode can reap ownerless nodes.
        assert_eq!(live_run_ids_from_journal(Ok(Vec::new())), Some(Vec::new()));
    }

    /// A hub that cannot even be constructed must degrade to `None` (dead-only
    /// reap), never to owner-known mode with an empty liveness set.
    #[test]
    fn live_run_ids_for_project_returns_none_when_hub_unavailable() {
        let temp = tempfile::tempdir().expect("temp dir");
        // A regular FILE as project root makes FileServiceHub::new fail.
        let bogus_root = temp.path().join("not-a-dir");
        std::fs::write(&bogus_root, "x").expect("bogus root file");
        assert!(live_run_ids_for_project(bogus_root.to_string_lossy().as_ref()).is_none());
    }

    /// End-to-end over a real FileServiceHub: Pending/Running/Paused runs are in
    /// the liveness set; terminal (Cancelled) runs are excluded. This is the
    /// same id namespace the broker stamps into `spec.metadata.animus_run_id`
    /// (the journal workflow id — see `ProcessManager::register_run`).
    #[test]
    fn live_run_ids_for_project_includes_active_and_excludes_terminal_workflows() {
        use orchestrator_core::{FileServiceHub, Priority, ServiceHub, TaskCreateInput, TaskType, WorkflowRunInput};
        use protocol::test_utils::EnvVarGuard;
        use std::process::Command as ProcessCommand;
        use tempfile::TempDir;

        let _lock = crate::shared::test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let temp = TempDir::new().expect("temp dir");
        let _home = EnvVarGuard::set("HOME", Some(temp.path().to_string_lossy().as_ref()));
        let init = ProcessCommand::new("git")
            .args(["init", "-b", "main"])
            .current_dir(temp.path())
            .status()
            .expect("git init should run");
        assert!(init.success(), "git init should succeed");
        let project_root = temp.path().to_string_lossy().to_string();
        let hub = FileServiceHub::new(&project_root).expect("file service hub");
        // v0.6: the kernel sources its base workflow config from a config_source
        // plugin; in tests, stand in for it after the hub scaffolds .animus/.
        let _config_source_seam =
            orchestrator_config::workflow_config::config_source_client::install_yaml_config_source_base(temp.path());

        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("test runtime");
        let (active_id, terminal_id) = rt.block_on(async {
            let task = hub
                .tasks()
                .create(TaskCreateInput {
                    title: "reap liveness active".to_string(),
                    description: "owner-known reap liveness test".to_string(),
                    task_type: Some(TaskType::Feature),
                    priority: Some(Priority::Medium),
                    created_by: Some("test".to_string()),
                    tags: Vec::new(),
                    linked_requirements: Vec::new(),
                    linked_architecture_entities: Vec::new(),
                })
                .await
                .expect("active task should be created");
            let terminal_task = hub
                .tasks()
                .create(TaskCreateInput {
                    title: "reap liveness terminal".to_string(),
                    description: "owner-known reap liveness test".to_string(),
                    task_type: Some(TaskType::Feature),
                    priority: Some(Priority::Medium),
                    created_by: Some("test".to_string()),
                    tags: Vec::new(),
                    linked_requirements: Vec::new(),
                    linked_architecture_entities: Vec::new(),
                })
                .await
                .expect("terminal task should be created");
            let active = hub
                .workflows()
                .run(WorkflowRunInput::for_task(task.id.clone(), None), None)
                .await
                .expect("active workflow should start");
            let terminal = hub
                .workflows()
                .run(WorkflowRunInput::for_task(terminal_task.id.clone(), None), None)
                .await
                .expect("terminal workflow should start");
            hub.workflows().cancel(&terminal.id).await.expect("workflow should cancel");
            (active.id, terminal.id)
        });

        let ids = live_run_ids_for_project(&project_root).expect("liveness read should succeed");
        assert!(ids.contains(&active_id), "active run {active_id} must be protected, got {ids:?}");
        assert!(!ids.contains(&terminal_id), "terminal run {terminal_id} must be excluded, got {ids:?}");
    }
}
