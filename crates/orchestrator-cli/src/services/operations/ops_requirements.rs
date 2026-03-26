use std::sync::Arc;

use anyhow::{anyhow, Result};
use orchestrator_core::{
    services::ServiceHub, ListPageRequest, RequirementFilter, RequirementQuery, RequirementsExecutionInput,
    REQUIREMENT_TASK_GENERATION_RUN_WORKFLOW_REF, REQUIREMENT_TASK_GENERATION_WORKFLOW_REF,
};

use super::ops_workflow::execute::WorkflowExecuteArgs;
mod graph;
mod mockups;
mod recommendations;
mod state;

use crate::{
    parse_requirement_category_opt, parse_requirement_priority_opt, parse_requirement_query_sort_opt,
    parse_requirement_status_opt, parse_requirement_type_opt, print_ok, print_value, RequirementGraphCommand,
    RequirementsCommand, RequirementsExecuteArgs,
};
use graph::{load_requirements_graph, save_requirements_graph, RequirementsGraphState};
use mockups::handle_requirement_mockups;
use recommendations::handle_requirement_recommendations;
use state::{create_requirement_cli, delete_requirement_cli, update_requirement_cli};

fn build_requirements_query(args: crate::RequirementsListArgs) -> Result<RequirementQuery> {
    Ok(RequirementQuery {
        filter: RequirementFilter {
            status: parse_requirement_status_opt(args.status.as_deref())?,
            priority: parse_requirement_priority_opt(args.priority.as_deref())?,
            category: parse_requirement_category_opt(args.category.as_deref())?,
            requirement_type: parse_requirement_type_opt(args.requirement_type.as_deref())?,
            tags: if args.tag.is_empty() { None } else { Some(args.tag) },
            linked_task_id: args.linked_task_id,
            search_text: args.search,
        },
        page: ListPageRequest { limit: args.limit, offset: args.offset },
        sort: parse_requirement_query_sort_opt(args.sort.as_deref())?.unwrap_or_default(),
    })
}

pub(crate) async fn handle_requirements(
    command: RequirementsCommand,
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
    json: bool,
) -> Result<()> {
    let planning = hub.planning();

    match command {
        RequirementsCommand::Execute(args) => {
            let execute_args = build_requirements_execute_args(args)?;
            super::ops_workflow::execute::handle_workflow_execute(execute_args, hub.clone(), project_root, json).await
        }
        RequirementsCommand::List(args) => {
            let page = planning.query(build_requirements_query(args)?).await?;
            print_value(page.items, json)
        }
        RequirementsCommand::Get(args) => print_value(planning.get_requirement(&args.id).await?, json),
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
        RequirementsCommand::Mockups { command } => handle_requirement_mockups(command, project_root, json).await,
        RequirementsCommand::Recommendations { command } => {
            handle_requirement_recommendations(command, hub.clone(), project_root, json).await
        }
    }
}

fn build_requirements_execute_args(args: RequirementsExecuteArgs) -> Result<WorkflowExecuteArgs> {
    let requirement_id = args.requirement_id.trim().to_owned();

    if requirement_id.is_empty() {
        return Err(anyhow!(
            "missing --id value for `requirements execute`; pass a requirement id to execute the requirement"
        ));
    }
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
        REQUIREMENT_TASK_GENERATION_RUN_WORKFLOW_REF.to_string()
    } else {
        REQUIREMENT_TASK_GENERATION_WORKFLOW_REF.to_string()
    };

    Ok(WorkflowExecuteArgs {
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
        vars: Vec::new(),
    })
}
