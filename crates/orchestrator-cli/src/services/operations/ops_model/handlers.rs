use super::super::{read_json_or_default, write_json_pretty};
use crate::cli_types::{ModelCommand, ModelEvalCommand, ModelRosterCommand};
use crate::{parse_input_json_or, print_value};
use anyhow::Result;
use chrono::Utc;
use orchestrator_core::ServiceHub;
use std::sync::Arc;
use uuid::Uuid;

use super::state::{model_eval_report_path, model_roster_path, ModelEvaluationReportCli, ModelRosterStoreCli};
use super::status::{default_model_specs, evaluate_model_status, parse_model_specs, summarize_model_statuses};

pub(crate) async fn handle_model(
    command: ModelCommand,
    _hub: Arc<dyn ServiceHub>,
    project_root: &str,
    json: bool,
) -> Result<()> {
    match command {
        ModelCommand::Availability(args) => {
            let specs = parse_input_json_or(args.input_json, || Ok(args.model))?;
            let statuses: Vec<_> = parse_model_specs(&specs)
                .into_iter()
                .map(|(model_id, cli_tool)| evaluate_model_status(&model_id, &cli_tool))
                .collect();
            print_value(summarize_model_statuses(&statuses), json)
        }
        ModelCommand::Status(args) => {
            let spec = format!("{}:{}", args.model_id, args.cli_tool);
            let (model_id, cli_tool) =
                parse_model_specs(&[spec]).into_iter().next().unwrap_or((args.model_id, args.cli_tool));
            let status = evaluate_model_status(&model_id, &cli_tool);
            print_value(status, json)
        }
        ModelCommand::Validate(args) => {
            let specs = if args.model.is_empty() { default_model_specs() } else { parse_model_specs(&args.model) };
            let statuses: Vec<_> =
                specs.into_iter().map(|(model_id, cli_tool)| evaluate_model_status(&model_id, &cli_tool)).collect();
            print_value(
                serde_json::json!({
                    "task_id": args.task_id,
                    "validation": summarize_model_statuses(&statuses),
                }),
                json,
            )
        }
        ModelCommand::Roster { command } => match command {
            ModelRosterCommand::Refresh => {
                let statuses: Vec<_> = default_model_specs()
                    .into_iter()
                    .map(|(model_id, cli_tool)| evaluate_model_status(&model_id, &cli_tool))
                    .collect();
                let roster =
                    ModelRosterStoreCli { refreshed_at: Utc::now().to_rfc3339(), candidates: statuses.clone() };
                write_json_pretty(&model_roster_path(project_root), &roster)?;
                print_value(roster, json)
            }
            ModelRosterCommand::Get => {
                let roster = read_json_or_default::<ModelRosterStoreCli>(&model_roster_path(project_root))?;
                print_value(roster, json)
            }
        },
        ModelCommand::Eval { command } => match command {
            ModelEvalCommand::Run(args) => {
                let specs = if args.model.is_empty() { default_model_specs() } else { parse_model_specs(&args.model) };
                let statuses: Vec<_> =
                    specs.into_iter().map(|(model_id, cli_tool)| evaluate_model_status(&model_id, &cli_tool)).collect();
                let available = statuses.iter().filter(|status| status.availability == "available").count();
                let report = ModelEvaluationReportCli {
                    report_id: format!("model-eval-{}", Uuid::new_v4().simple()),
                    generated_at: Utc::now().to_rfc3339(),
                    total: statuses.len(),
                    available,
                    unavailable: statuses.len().saturating_sub(available),
                    statuses,
                };
                write_json_pretty(&model_eval_report_path(project_root), &report)?;
                print_value(report, json)
            }
            ModelEvalCommand::Report => {
                let report = read_json_or_default::<ModelEvaluationReportCli>(&model_eval_report_path(project_root))?;
                print_value(report, json)
            }
        },
    }
}
