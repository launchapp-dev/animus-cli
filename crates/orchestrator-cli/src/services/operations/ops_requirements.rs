use std::sync::Arc;

use anyhow::{anyhow, Result};
use orchestrator_core::{
    services::ServiceHub, RequirementsExecutionInput, REQUIREMENT_TASK_GENERATION_WORKFLOW_REF,
};

mod graph;
mod mockups;
mod recommendations;
mod state;

use crate::{
    print_ok, print_value, RequirementGraphCommand, RequirementsCommand, RequirementsExecuteArgs,
    WorkflowCommand, WorkflowExecuteArgs,
};
use graph::{load_requirements_graph, save_requirements_graph, RequirementsGraphState};
use mockups::handle_requirement_mockups;
use recommendations::handle_requirement_recommendations;
use state::{create_requirement_cli, delete_requirement_cli, update_requirement_cli};

const BUILTIN_REQUIREMENTS_EXECUTE_WORKFLOW_REF: &str = "builtin/requirements-execute";

pub(crate) async fn handle_requirements(
    command: RequirementsCommand,
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
    json: bool,
) -> Result<()> {
    let planning = hub.planning();

    match command {
        RequirementsCommand::Execute(args) => {
            let workflow_command = build_requirements_execute_workflow_command(args)?;
            super::handle_workflow(workflow_command, hub.clone(), project_root, json).await
        }
        RequirementsCommand::List => print_value(planning.list_requirements().await?, json),
        RequirementsCommand::Get(args) => {
            print_value(planning.get_requirement(&args.id).await?, json)
        }
        RequirementsCommand::Create(args) => {
            let created = create_requirement_cli(project_root, args)?;
            print_value(created, json)
        }
        RequirementsCommand::Update(args) => {
            let updated = update_requirement_cli(project_root, args)?;
            print_value(updated, json)
        }
        RequirementsCommand::Delete(args) => {
            delete_requirement_cli(project_root, &args.id)?;
            print_ok("requirement deleted", json);
            Ok(())
        }
        RequirementsCommand::Graph { command } => match command {
            RequirementGraphCommand::Get => {
                let graph = load_requirements_graph(project_root)?;
                print_value(graph, json)
            }
            RequirementGraphCommand::Save(args) => {
                let graph = serde_json::from_str::<RequirementsGraphState>(&args.input_json)?;
                save_requirements_graph(project_root, &graph)?;
                print_value(graph, json)
            }
        },
        RequirementsCommand::Mockups { command } => {
            handle_requirement_mockups(command, project_root, json).await
        }
        RequirementsCommand::Recommendations { command } => {
            handle_requirement_recommendations(command, hub.clone(), project_root, json).await
        }
    }
}

fn build_requirements_execute_workflow_command(
    args: RequirementsExecuteArgs,
) -> Result<WorkflowCommand> {
    let mut requirement_ids = args
        .requirement_ids
        .into_iter()
        .filter(|id| !id.trim().is_empty())
        .collect::<Vec<_>>();

    if requirement_ids.is_empty() {
        return Err(anyhow!(
            "missing --id value for `requirements execute`; pass a requirement id to delegate to `workflow execute --requirement-id`"
        ));
    }

    if requirement_ids.len() > 1 {
        return Err(anyhow!(
            "`requirements execute` currently supports a single --id because it delegates to `workflow execute --requirement-id`"
        ));
    }

    let requirement_id = requirement_ids.remove(0);
    let input_json = match args.input_json {
        Some(raw) => Some(raw),
        None => Some(serde_json::to_string(&RequirementsExecutionInput {
            requirement_ids: vec![requirement_id.clone()],
            start_workflows: args.start_workflows,
            workflow_ref: args.workflow_ref.clone(),
            include_wont: args.include_wont,
        })?),
    };
    let workflow_ref = if args.start_workflows {
        BUILTIN_REQUIREMENTS_EXECUTE_WORKFLOW_REF.to_string()
    } else {
        REQUIREMENT_TASK_GENERATION_WORKFLOW_REF.to_string()
    };

    Ok(WorkflowCommand::Execute(WorkflowExecuteArgs {
        workflow_id: None,
        task_id: None,
        requirement_id: Some(requirement_id),
        title: None,
        description: None,
        workflow_ref: Some(workflow_ref),
        phase: None,
        model: None,
        tool: None,
        phase_timeout_secs: None,
        input_json,
        quiet: false,
        verbose: false,
        vars: Vec::new(),
    }))
}
