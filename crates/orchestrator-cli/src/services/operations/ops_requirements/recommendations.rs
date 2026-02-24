use crate::cli_types::RecommendationCommand;
use crate::print_value;
use anyhow::{anyhow, Result};
use chrono::Utc;
use orchestrator_core::{RequirementItem, RequirementStatus, ServiceHub};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use uuid::Uuid;

use super::state::{
    load_requirements_map_from_core_state, project_state_dir, read_json_or_default,
    save_requirements_map_to_core_state, write_json_pretty,
};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
enum RecommendationMode {
    #[default]
    ReportOnly,
    SafeApply,
    FullApply,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RecommendationConfigState {
    enabled: bool,
    mode: RecommendationMode,
}

impl Default for RecommendationConfigState {
    fn default() -> Self {
        Self {
            enabled: true,
            mode: RecommendationMode::ReportOnly,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RecommendationFinding {
    id: String,
    #[serde(default)]
    requirement_id: Option<String>,
    category: String,
    severity: String,
    summary: String,
    suggested_action: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RecommendationReportState {
    id: String,
    source: String,
    mode: RecommendationMode,
    findings: Vec<RecommendationFinding>,
    #[serde(default)]
    applied_actions: Vec<String>,
    created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct RecommendationReportsState {
    #[serde(default)]
    reports: Vec<RecommendationReportState>,
}

fn recommendations_config_path(project_root: &str) -> PathBuf {
    project_state_dir(project_root).join("recommendation-config.json")
}

fn recommendations_reports_path(project_root: &str) -> PathBuf {
    project_state_dir(project_root).join("recommendation-reports.json")
}

fn load_recommendation_config(project_root: &str) -> Result<RecommendationConfigState> {
    read_json_or_default(&recommendations_config_path(project_root))
}

fn save_recommendation_config(
    project_root: &str,
    config: &RecommendationConfigState,
) -> Result<()> {
    write_json_pretty(&recommendations_config_path(project_root), config)
}

fn load_recommendation_reports(project_root: &str) -> Result<RecommendationReportsState> {
    read_json_or_default(&recommendations_reports_path(project_root))
}

fn save_recommendation_reports(
    project_root: &str,
    reports: &RecommendationReportsState,
) -> Result<()> {
    write_json_pretty(&recommendations_reports_path(project_root), reports)
}

fn parse_recommendation_mode(value: &str) -> Result<RecommendationMode> {
    let mode = match value.trim().to_ascii_lowercase().as_str() {
        "report_only" | "report-only" => RecommendationMode::ReportOnly,
        "safe_apply" | "safe-apply" => RecommendationMode::SafeApply,
        "full_apply" | "full-apply" => RecommendationMode::FullApply,
        _ => anyhow::bail!("invalid recommendation mode: {value}"),
    };
    Ok(mode)
}

fn scan_requirement_recommendations(
    requirements: &[RequirementItem],
    mode: RecommendationMode,
) -> RecommendationReportState {
    let mut findings = Vec::new();
    for requirement in requirements {
        if requirement.acceptance_criteria.is_empty() {
            findings.push(RecommendationFinding {
                id: format!("F-{}", Uuid::new_v4().simple()),
                requirement_id: Some(requirement.id.clone()),
                category: "acceptance_criteria_missing".to_string(),
                severity: "high".to_string(),
                summary: format!("Requirement {} has no acceptance criteria", requirement.id),
                suggested_action: "Add measurable acceptance criteria".to_string(),
            });
        }
        if requirement.status == RequirementStatus::Draft {
            findings.push(RecommendationFinding {
                id: format!("F-{}", Uuid::new_v4().simple()),
                requirement_id: Some(requirement.id.clone()),
                category: "requirement_still_draft".to_string(),
                severity: "medium".to_string(),
                summary: format!("Requirement {} is still in draft status", requirement.id),
                suggested_action: "Refine requirement before execution".to_string(),
            });
        }
    }

    RecommendationReportState {
        id: format!("REC-{}", Uuid::new_v4().simple()),
        source: "on-demand".to_string(),
        mode,
        findings,
        applied_actions: Vec::new(),
        created_at: Utc::now().to_rfc3339(),
    }
}

pub(super) async fn handle_requirement_recommendations(
    command: RecommendationCommand,
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
    json: bool,
) -> Result<()> {
    match command {
        RecommendationCommand::Scan(args) => {
            let config = load_recommendation_config(project_root)?;
            let mode = args
                .mode
                .as_deref()
                .map(parse_recommendation_mode)
                .transpose()?
                .unwrap_or(config.mode);
            let mut requirements = hub.planning().list_requirements().await.unwrap_or_default();
            if requirements.is_empty() {
                requirements = load_requirements_map_from_core_state(project_root)?
                    .into_values()
                    .collect();
            }
            let report = scan_requirement_recommendations(&requirements, mode);
            let mut reports = load_recommendation_reports(project_root)?;
            reports.reports.push(report.clone());
            save_recommendation_reports(project_root, &reports)?;
            print_value(report, json)
        }
        RecommendationCommand::List => {
            let reports = load_recommendation_reports(project_root)?;
            print_value(reports.reports, json)
        }
        RecommendationCommand::Apply(args) => {
            let config = load_recommendation_config(project_root)?;
            let mode = args
                .mode
                .as_deref()
                .map(parse_recommendation_mode)
                .transpose()?
                .unwrap_or(config.mode);
            let mut reports = load_recommendation_reports(project_root)?;
            let report = reports
                .reports
                .iter_mut()
                .find(|report| report.id == args.report_id)
                .ok_or_else(|| anyhow!("recommendation report not found: {}", args.report_id))?;

            let mut requirements = load_requirements_map_from_core_state(project_root)?;
            let mut applied_actions = Vec::new();
            if mode != RecommendationMode::ReportOnly {
                for finding in &report.findings {
                    let Some(requirement_id) = finding.requirement_id.as_ref() else {
                        continue;
                    };
                    let Some(requirement) = requirements.get_mut(requirement_id) else {
                        continue;
                    };
                    match finding.category.as_str() {
                        "acceptance_criteria_missing" => {
                            if requirement.acceptance_criteria.is_empty() {
                                requirement.acceptance_criteria = vec![
                                    "Requirement has objective acceptance criteria".to_string(),
                                    "Requirement includes automated validation".to_string(),
                                ];
                                requirement.updated_at = Utc::now();
                                applied_actions.push(format!(
                                    "added acceptance criteria for {}",
                                    requirement.id
                                ));
                            }
                        }
                        "requirement_still_draft" => {
                            if mode == RecommendationMode::FullApply
                                && requirement.status == RequirementStatus::Draft
                            {
                                requirement.status = RequirementStatus::Refined;
                                requirement.updated_at = Utc::now();
                                applied_actions
                                    .push(format!("marked {} as refined", requirement.id));
                            }
                        }
                        _ => {}
                    }
                }
            }
            if !applied_actions.is_empty() {
                save_requirements_map_to_core_state(project_root, &requirements)?;
            }
            report.mode = mode;
            report.applied_actions.extend(applied_actions);
            let updated = report.clone();
            save_recommendation_reports(project_root, &reports)?;
            print_value(updated, json)
        }
        RecommendationCommand::ConfigGet => {
            print_value(load_recommendation_config(project_root)?, json)
        }
        RecommendationCommand::ConfigUpdate(args) => {
            let mut config = load_recommendation_config(project_root)?;
            if let Some(input_json) = args.input_json {
                config = serde_json::from_str::<RecommendationConfigState>(&input_json)?;
            } else {
                if let Some(mode) = args.mode {
                    config.mode = parse_recommendation_mode(&mode)?;
                }
                if let Some(enabled) = args.enabled {
                    config.enabled = enabled;
                }
            }
            save_recommendation_config(project_root, &config)?;
            print_value(config, json)
        }
    }
}
