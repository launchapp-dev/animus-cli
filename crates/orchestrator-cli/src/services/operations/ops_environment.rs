use std::path::Path;

use anyhow::Result;
use orchestrator_core::EnvironmentClient;
use serde_json::json;

use crate::{print_value, EnvironmentCommand};

/// Handle `animus environment {list,get,teardown,reap}` by driving the installed
/// environment plugin's node-management surface. Each verb resolves the plugin
/// (the sole installed one, or `--environment <id>`), issues the role call, and
/// emits the `animus.cli.v1` envelope.
pub async fn handle_environment(command: EnvironmentCommand, project_root: &str, json_output: bool) -> Result<()> {
    let project_root = Path::new(project_root);
    match command {
        EnvironmentCommand::List(args) => {
            let client = EnvironmentClient::resolve(project_root, args.environment.as_deref().unwrap_or(""))?;
            let nodes = client.list_nodes()?;
            print_value(json!({ "environment": client.plugin_name(), "nodes": nodes }), json_output)
        }
        EnvironmentCommand::Get(args) => {
            let client = EnvironmentClient::resolve(project_root, args.environment.as_deref().unwrap_or(""))?;
            let node = client.get_node(&args.id)?;
            print_value(json!({ "environment": client.plugin_name(), "node": node }), json_output)
        }
        EnvironmentCommand::Teardown(args) => {
            let client = EnvironmentClient::resolve(project_root, args.environment.as_deref().unwrap_or(""))?;
            let deleted = client.teardown_node(&args.id)?;
            print_value(json!({ "environment": client.plugin_name(), "deleted": deleted }), json_output)
        }
        EnvironmentCommand::Reap(args) => {
            let client = EnvironmentClient::resolve(project_root, args.environment.as_deref().unwrap_or(""))?;
            let report = client.reap(args.all, args.force, args.dry_run, args.older_than_secs)?;
            print_value(
                json!({
                    "environment": client.plugin_name(),
                    "deleted": report.deleted,
                    "kept": report.kept,
                    "dry_run": report.dry_run,
                }),
                json_output,
            )
        }
    }
}
