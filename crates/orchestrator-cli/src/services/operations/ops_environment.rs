use std::path::Path;

use anyhow::Result;
use orchestrator_core::{EnvironmentClient, EnvironmentNode};
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
    let report = client.reap(all, force, dry_run, older_than_secs)?;
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

    use super::{EnvironmentGetView, EnvironmentListView, EnvironmentReapView, EnvironmentTeardownView};

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
}
