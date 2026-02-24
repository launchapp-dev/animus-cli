use std::sync::Arc;

use anyhow::Result;
use orchestrator_core::{services::ServiceHub, ProjectCreateInput};

use crate::{parse_input_json_or, parse_project_type_opt, print_ok, print_value, ProjectCommand};

pub(crate) async fn handle_project(
    command: ProjectCommand,
    hub: Arc<dyn ServiceHub>,
    json: bool,
) -> Result<()> {
    let projects = hub.projects();

    match command {
        ProjectCommand::List => print_value(projects.list().await?, json),
        ProjectCommand::Active => print_value(projects.active().await?, json),
        ProjectCommand::Get(args) => print_value(projects.get(&args.id).await?, json),
        ProjectCommand::Create(args) => {
            let input = parse_input_json_or(args.input_json, || {
                Ok(ProjectCreateInput {
                    name: args.name,
                    path: args.path,
                    project_type: parse_project_type_opt(args.project_type.as_deref())?,
                    description: None,
                    tech_stack: Vec::new(),
                    metadata: None,
                })
            })?;
            print_value(projects.create(input).await?, json)
        }
        ProjectCommand::Load(args) => print_value(projects.load(&args.id).await?, json),
        ProjectCommand::Rename(args) => {
            print_value(projects.rename(&args.id, &args.name).await?, json)
        }
        ProjectCommand::Archive(args) => print_value(projects.archive(&args.id).await?, json),
        ProjectCommand::Remove(args) => {
            projects.remove(&args.id).await?;
            print_ok("project removed", json);
            Ok(())
        }
    }
}
