use std::sync::Arc;

use anyhow::Result;
use orchestrator_core::{services::ServiceHub, RequirementFilter};

mod graph;
mod mockups;
mod recommendations;
mod state;

use super::ops_planning::{
    run_requirements_draft, run_requirements_refine, RequirementsDraftInputPayload,
    RequirementsRefineInputPayload,
};
use crate::{
    parse_input_json_or, print_ok, print_value, RequirementGraphCommand, RequirementsCommand,
};
use graph::{load_requirements_graph, save_requirements_graph, RequirementsGraphState};
use mockups::handle_requirement_mockups;
use recommendations::handle_requirement_recommendations;
use state::{
    create_requirement_cli, delete_requirement_cli, parse_requirement_category_opt,
    parse_requirement_priority_opt, parse_requirement_status_opt, parse_requirement_type_opt,
    update_requirement_cli,
};

pub(crate) async fn handle_requirements(
    command: RequirementsCommand,
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
    json: bool,
) -> Result<()> {
    let planning = hub.planning();

    match command {
        RequirementsCommand::Draft(args) => {
            let input = parse_input_json_or(args.input_json, || {
                Ok(RequirementsDraftInputPayload {
                    include_codebase_scan: args.include_codebase_scan,
                    append_only: args.append_only,
                    max_requirements: args.max_requirements,
                    draft_strategy: args.draft_strategy,
                    po_parallelism: args.po_parallelism,
                    quality_repair_attempts: args.quality_repair_attempts,
                    allow_heuristic_complexity: args.allow_heuristic_complexity,
                    tool: args.tool,
                    model: args.model,
                    timeout_secs: args.timeout_secs,
                    start_runner: args.start_runner,
                })
            })?;
            print_value(
                run_requirements_draft(hub.clone(), project_root, input).await?,
                json,
            )
        }
        RequirementsCommand::List(args) => {
            let filter = RequirementFilter {
                status: parse_requirement_status_opt(args.status.as_deref())?,
                priority: parse_requirement_priority_opt(args.priority.as_deref())?,
                requirement_type: parse_requirement_type_opt(args.requirement_type.as_deref())?,
                category: parse_requirement_category_opt(args.category.as_deref())?,
                tags: if args.tag.is_empty() {
                    None
                } else {
                    Some(args.tag)
                },
                labels: if args.label.is_empty() {
                    None
                } else {
                    Some(args.label)
                },
                area: args.area,
                source: args.source,
                linked_epic_id: args.epic_id,
                linked_task_id: args.linked_task_id.first().cloned(),
                search_text: args.search,
            };
            print_value(planning.list_requirements_filtered(filter).await?, json)
        }
        RequirementsCommand::Get(args) => {
            print_value(planning.get_requirement(&args.id).await?, json)
        }
        RequirementsCommand::Refine(args) => {
            let input = parse_input_json_or(args.input_json, || {
                Ok(RequirementsRefineInputPayload {
                    requirement_ids: args.requirement_ids,
                    focus: args.focus,
                    use_ai: args.use_ai,
                    tool: args.tool,
                    model: args.model,
                    timeout_secs: args.timeout_secs,
                    start_runner: args.start_runner,
                })
            })?;
            print_value(
                run_requirements_refine(hub.clone(), project_root, input).await?,
                json,
            )
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
