mod complexity;
mod draft_runtime;
mod prompt_template;
mod refinement_apply;
mod refinement_parse;
mod refinement_runtime;
mod requirements_parse;
mod requirements_prompt;
mod requirements_runtime;
mod types;

use std::sync::Arc;

use super::ops_history::write_history_execution_event;
use anyhow::Result;
use orchestrator_core::{services::ServiceHub, RequirementsExecutionInput, VisionDraftInput};

use crate::{
    ensure_ai_generated_tasks_for_requirements, parse_input_json_or, print_value, ExecuteCommand,
    PlanningCommand, PlanningRequirementsCommand, PlanningVisionCommand, VisionCommand,
};

use self::draft_runtime::{draft_vision_with_ai_complexity, VisionDraftAiOptions};
use self::refinement_runtime::run_vision_refine;
use self::types::VisionRefineInputPayload;

pub(crate) use self::requirements_runtime::run_requirements_draft;
pub(crate) use self::requirements_runtime::run_requirements_refine;
pub(crate) use self::types::{RequirementsDraftInputPayload, RequirementsRefineInputPayload};

async fn maybe_generate_ai_tasks_for_requirements(
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
    requirement_ids: &[String],
    enabled: bool,
) -> Result<()> {
    if !enabled {
        return Ok(());
    }

    ensure_ai_generated_tasks_for_requirements(hub, project_root, requirement_ids).await?;
    Ok(())
}

pub(crate) async fn handle_vision(
    command: VisionCommand,
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
    json: bool,
) -> Result<()> {
    let planning = hub.planning();

    match command {
        VisionCommand::Draft(args) => {
            let input = parse_input_json_or(args.input_json, || {
                Ok(VisionDraftInput {
                    project_name: args.project_name,
                    problem_statement: args.problem,
                    target_users: args.target_user,
                    goals: args.goal,
                    constraints: args.constraint,
                    value_proposition: args.value_proposition,
                    complexity_assessment: None,
                })
            })?;
            let options = VisionDraftAiOptions {
                use_ai_complexity: args.use_ai_complexity,
                tool: args.tool,
                model: args.model,
                timeout_secs: args.timeout_secs,
                start_runner: args.start_runner,
                allow_heuristic_fallback: args.allow_heuristic_fallback,
            };
            print_value(
                draft_vision_with_ai_complexity(hub.clone(), project_root, input, options).await?,
                json,
            )
        }
        VisionCommand::Refine(args) => {
            let input = parse_input_json_or(args.input_json, || {
                Ok(VisionRefineInputPayload {
                    focus: args.focus,
                    use_ai: args.use_ai,
                    tool: args.tool,
                    model: args.model,
                    timeout_secs: args.timeout_secs,
                    start_runner: args.start_runner,
                    allow_heuristic_fallback: args.allow_heuristic_fallback,
                    preserve_core: args.preserve_core,
                })
            })?;
            print_value(run_vision_refine(hub, project_root, input).await?, json)
        }
        VisionCommand::Get => print_value(planning.get_vision().await?, json),
    }
}

pub(crate) async fn handle_execute(
    command: ExecuteCommand,
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
    json: bool,
) -> Result<()> {
    let planning = hub.planning();

    match command {
        ExecuteCommand::Plan(args) => {
            let input = parse_input_json_or(args.input_json, || {
                Ok(RequirementsExecutionInput {
                    requirement_ids: args.requirement_ids,
                    start_workflows: false,
                    pipeline_id: args.pipeline_id,
                    include_wont: args.include_wont,
                })
            })?;
            maybe_generate_ai_tasks_for_requirements(
                hub.clone(),
                project_root,
                &input.requirement_ids,
                args.ai_task_generation,
            )
            .await?;
            print_value(planning.execute_requirements(input).await?, json)
        }
        ExecuteCommand::Run(args) => {
            let input = parse_input_json_or(args.input_json, || {
                Ok(RequirementsExecutionInput {
                    requirement_ids: args.requirement_ids,
                    start_workflows: true,
                    pipeline_id: args.pipeline_id,
                    include_wont: args.include_wont,
                })
            })?;
            maybe_generate_ai_tasks_for_requirements(
                hub.clone(),
                project_root,
                &input.requirement_ids,
                args.ai_task_generation,
            )
            .await?;
            print_value(planning.execute_requirements(input).await?, json)
        }
    }
}

pub(crate) async fn handle_planning(
    command: PlanningCommand,
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
    json: bool,
) -> Result<()> {
    let planning = hub.planning();
    match command {
        PlanningCommand::Vision { command } => match command {
            PlanningVisionCommand::Draft(args) => {
                let input = parse_input_json_or(args.input_json, || {
                    Ok(VisionDraftInput {
                        project_name: args.project_name,
                        problem_statement: args.problem,
                        target_users: args.target_user,
                        goals: args.goal,
                        constraints: args.constraint,
                        value_proposition: args.value_proposition,
                        complexity_assessment: None,
                    })
                })?;
                let options = VisionDraftAiOptions {
                    use_ai_complexity: args.use_ai_complexity,
                    tool: args.tool,
                    model: args.model,
                    timeout_secs: args.timeout_secs,
                    start_runner: args.start_runner,
                    allow_heuristic_fallback: args.allow_heuristic_fallback,
                };
                print_value(
                    draft_vision_with_ai_complexity(hub.clone(), project_root, input, options)
                        .await?,
                    json,
                )
            }
            PlanningVisionCommand::Refine(args) => {
                let input = parse_input_json_or(args.input_json, || {
                    Ok(VisionRefineInputPayload {
                        focus: args.focus,
                        use_ai: args.use_ai,
                        tool: args.tool,
                        model: args.model,
                        timeout_secs: args.timeout_secs,
                        start_runner: args.start_runner,
                        allow_heuristic_fallback: args.allow_heuristic_fallback,
                        preserve_core: args.preserve_core,
                    })
                })?;
                print_value(
                    run_vision_refine(hub.clone(), project_root, input).await?,
                    json,
                )
            }
            PlanningVisionCommand::Get => print_value(planning.get_vision().await?, json),
        },
        PlanningCommand::Requirements { command } => match command {
            PlanningRequirementsCommand::Draft(args) => {
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
            PlanningRequirementsCommand::List => {
                print_value(planning.list_requirements().await?, json)
            }
            PlanningRequirementsCommand::Get(args) => {
                print_value(planning.get_requirement(&args.id).await?, json)
            }
            PlanningRequirementsCommand::Refine(args) => {
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
            PlanningRequirementsCommand::Execute(args) => {
                let input = parse_input_json_or(args.input_json, || {
                    Ok(RequirementsExecutionInput {
                        requirement_ids: args.requirement_ids,
                        start_workflows: args.start_workflows,
                        pipeline_id: args.pipeline_id,
                        include_wont: args.include_wont,
                    })
                })?;
                maybe_generate_ai_tasks_for_requirements(
                    hub.clone(),
                    project_root,
                    &input.requirement_ids,
                    args.ai_task_generation,
                )
                .await?;
                let result = planning.execute_requirements(input).await?;
                let _ = write_history_execution_event(
                    project_root,
                    "planning-execute",
                    None,
                    "completed",
                    serde_json::to_value(&result).unwrap_or_else(|_| serde_json::json!({})),
                );
                print_value(result, json)
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use orchestrator_core::{
        ComplexityAssessment, ComplexityTier, RequirementRange, TaskDensity, VisionDocument,
    };

    use super::complexity::assessment_from_proposal;
    use super::refinement_parse::{
        parse_vision_refinement_from_payload, parse_vision_refinement_from_text,
    };

    #[test]
    fn parse_vision_refinement_from_codex_item_completed_payload() {
        let wrapped = r#"{"type":"item.completed","item":{"id":"item_1","type":"agent_message","text":"{\"rationale\":\"Add better metrics alignment\",\"goals_additions\":[\"Define onboarding KPI targets\"]}"}}"#;
        let parsed = parse_vision_refinement_from_text(wrapped).expect("proposal should parse");
        assert_eq!(
            parsed.rationale.as_deref(),
            Some("Add better metrics alignment")
        );
        assert_eq!(
            parsed.goals_additions,
            vec!["Define onboarding KPI targets".to_string()]
        );
    }

    #[test]
    fn parse_vision_refinement_from_markdown_fence() {
        let text = r#"
Model suggestion:
```json
{
  "constraints_additions": ["Add explicit data retention limits"],
  "rationale": "Introduce concrete governance guardrails."
}
```
"#;
        let parsed = parse_vision_refinement_from_text(text).expect("proposal should parse");
        assert_eq!(
            parsed.constraints_additions,
            vec!["Add explicit data retention limits".to_string()]
        );
        assert_eq!(
            parsed.rationale.as_deref(),
            Some("Introduce concrete governance guardrails.")
        );
    }

    #[test]
    fn parse_vision_refinement_from_nested_data_wrapper() {
        let payload = json!({
            "data": {
                "refinement": {
                    "target_users_additions": ["Mid-market finance teams"],
                    "rationale": "Sharper ICP focus."
                }
            }
        });
        let parsed = parse_vision_refinement_from_payload(&payload).expect("proposal should parse");
        assert_eq!(
            parsed.target_users_additions,
            vec!["Mid-market finance teams".to_string()]
        );
        assert_eq!(parsed.rationale.as_deref(), Some("Sharper ICP focus."));
    }

    #[test]
    fn parse_complexity_assessment_from_ai_payload() {
        let payload = json!({
            "complexity_assessment": {
                "tier": "complex",
                "confidence": 0.87,
                "recommended_requirement_range": { "min": 14, "max": 26 },
                "task_density": "high",
                "rationale": "Multiple integrations and governance controls require deeper scope."
            }
        });
        let proposal =
            parse_vision_refinement_from_payload(&payload).expect("proposal should parse");
        let current = VisionDocument {
            id: "vision-1".to_string(),
            project_root: "/tmp/test".to_string(),
            markdown: "# Product Vision".to_string(),
            problem_statement: "Need complex workflow".to_string(),
            target_users: vec!["Ops".to_string()],
            goals: vec!["Goal".to_string()],
            constraints: vec!["Constraint".to_string()],
            value_proposition: Some("Value".to_string()),
            complexity_assessment: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        let assessment = assessment_from_proposal(proposal.complexity_assessment, &current);
        assert_eq!(assessment.tier, ComplexityTier::Complex);
        assert_eq!(assessment.task_density, TaskDensity::High);
        assert_eq!(assessment.recommended_requirement_range.min, 14);
        assert_eq!(assessment.recommended_requirement_range.max, 26);
    }

    #[test]
    fn clamp_simple_range_from_ai_payload() {
        let payload = json!({
            "complexity_assessment": {
                "tier": "simple",
                "confidence": 0.7,
                "recommended_requirement_range": { "min": 1, "max": 40 },
                "task_density": "low",
                "rationale": "Small scoped utility."
            }
        });
        let proposal =
            parse_vision_refinement_from_payload(&payload).expect("proposal should parse");
        let current = VisionDocument {
            id: "vision-1".to_string(),
            project_root: "/tmp/test".to_string(),
            markdown: "# Product Vision".to_string(),
            problem_statement: "Need simple utility".to_string(),
            target_users: vec!["Ops".to_string()],
            goals: vec!["Goal".to_string()],
            constraints: vec!["Constraint".to_string()],
            value_proposition: Some("Value".to_string()),
            complexity_assessment: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        let assessment = assessment_from_proposal(proposal.complexity_assessment, &current);
        assert_eq!(assessment.tier, ComplexityTier::Simple);
        assert_eq!(assessment.recommended_requirement_range.min, 4);
        assert_eq!(assessment.recommended_requirement_range.max, 8);
    }

    #[test]
    fn heuristic_inference_can_upgrade_stale_simple_assessment() {
        let current = VisionDocument {
            id: "vision-1".to_string(),
            project_root: "/tmp/test".to_string(),
            markdown: "# Product Vision".to_string(),
            problem_statement:
                "Enterprise platform with compliance audit, RBAC, and multi-tenant governance."
                    .to_string(),
            target_users: vec!["Security and operations teams".to_string()],
            goals: vec![
                "Deliver role-based approval workflows".to_string(),
                "Provide audit-ready evidence exports".to_string(),
                "Enable high-availability service tiers".to_string(),
                "Integrate with existing ERP and SSO".to_string(),
            ],
            constraints: vec![
                "SOC2 controls are mandatory".to_string(),
                "All critical events must be immutable and traceable".to_string(),
                "Support enterprise SAML SSO and data residency".to_string(),
            ],
            value_proposition: Some("Reduce governance risk and rollout time.".to_string()),
            complexity_assessment: Some(ComplexityAssessment {
                tier: ComplexityTier::Simple,
                confidence: 0.51,
                rationale: Some("stale".to_string()),
                recommended_requirement_range: RequirementRange { min: 4, max: 8 },
                task_density: TaskDensity::Low,
                source: Some("heuristic".to_string()),
            }),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };

        let assessment = assessment_from_proposal(None, &current);
        assert_ne!(assessment.tier, ComplexityTier::Simple);
    }
}
