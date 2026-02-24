use std::sync::Arc;

use anyhow::{anyhow, Result};
use chrono::Utc;
use orchestrator_core::{
    services::ServiceHub, ComplexityAssessment, VisionDocument, VisionDraftInput,
};
use protocol::{AgentRunEvent, AgentRunRequest, ModelId, RunId, PROTOCOL_VERSION};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::time::{Duration, Instant};
use uuid::Uuid;

use super::complexity::{assessment_from_proposal, infer_complexity_from_vision};
use super::refinement_parse::parse_vision_refinement_from_text;
use crate::{
    build_runtime_contract, connect_runner, event_matches_run, runner_config_dir, write_json_line,
};

const VISION_COMPLEXITY_PROMPT_TEMPLATE: &str =
    include_str!("../../../../prompts/ops_planning/vision_complexity_assess.prompt");

#[derive(Debug, Clone)]
pub(super) struct VisionDraftAiOptions {
    pub(super) use_ai_complexity: bool,
    pub(super) tool: String,
    pub(super) model: String,
    pub(super) timeout_secs: Option<u64>,
    pub(super) start_runner: bool,
    pub(super) allow_heuristic_fallback: bool,
}

pub(super) fn is_ai_complexity_source(source: Option<&str>) -> bool {
    source
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.to_ascii_lowercase())
        .is_some_and(|value| value.starts_with("ai"))
}

fn build_context_vision(project_root: &str, input: &VisionDraftInput) -> VisionDocument {
    VisionDocument {
        id: "vision-draft-context".to_string(),
        project_root: project_root.to_string(),
        markdown: String::new(),
        problem_statement: input.problem_statement.clone(),
        target_users: input.target_users.clone(),
        goals: input.goals.clone(),
        constraints: input.constraints.clone(),
        value_proposition: input.value_proposition.clone(),
        complexity_assessment: input.complexity_assessment.clone(),
        created_at: Utc::now(),
        updated_at: Utc::now(),
    }
}

fn build_complexity_assessment_prompt(input: &VisionDraftInput) -> String {
    let vision_json = serde_json::to_string_pretty(input).unwrap_or_else(|_| "{}".to_string());
    VISION_COMPLEXITY_PROMPT_TEMPLATE.replace("{vision_json}", &vision_json)
}

async fn request_ai_complexity_assessment(
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
    input: &VisionDraftInput,
    options: &VisionDraftAiOptions,
) -> Result<ComplexityAssessment> {
    if options.start_runner {
        hub.daemon().start().await?;
    }

    let prompt = build_complexity_assessment_prompt(input);
    let run_id = RunId(format!("vision-draft-complexity-{}", Uuid::new_v4()));
    let timeout_secs = options.timeout_secs.unwrap_or(240).max(30);
    let deadline = Instant::now() + Duration::from_secs(timeout_secs);
    let mut context = serde_json::json!({
        "tool": options.tool,
        "prompt": prompt,
        "cwd": project_root,
        "project_root": project_root,
        "planning_stage": "vision-draft-complexity",
    });
    context["timeout_secs"] = Value::from(timeout_secs);
    if let Some(runtime_contract) = build_runtime_contract(&options.tool, &options.model, &prompt) {
        context["runtime_contract"] = runtime_contract;
    }

    let request = AgentRunRequest {
        protocol_version: PROTOCOL_VERSION.to_string(),
        run_id: run_id.clone(),
        model: ModelId(options.model.clone()),
        context,
        timeout_secs: Some(timeout_secs),
    };

    let config_dir = runner_config_dir(std::path::Path::new(project_root));
    let stream = connect_runner(&config_dir).await?;
    let (read_half, mut write_half) = tokio::io::split(stream);
    write_json_line(&mut write_half, &request).await?;

    let mut lines = BufReader::new(read_half).lines();
    let mut transcript = String::new();
    loop {
        let next_line = tokio::time::timeout_at(deadline, lines.next_line())
            .await
            .map_err(|_| {
                anyhow!(
                    "vision-draft-complexity timed out after {}s for model {}",
                    timeout_secs,
                    options.model
                )
            })??;
        let Some(line) = next_line else {
            break;
        };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(event) = serde_json::from_str::<AgentRunEvent>(line) else {
            continue;
        };
        if !event_matches_run(&event, &run_id) {
            continue;
        }
        match event {
            AgentRunEvent::OutputChunk { text, .. } => {
                transcript.push_str(&text);
                transcript.push('\n');
            }
            AgentRunEvent::Thinking { content, .. } => {
                transcript.push_str(&content);
                transcript.push('\n');
            }
            AgentRunEvent::Error { error, .. } => {
                return Err(anyhow!("vision-draft-complexity failed: {error}"));
            }
            AgentRunEvent::Finished { exit_code, .. } => {
                if exit_code.unwrap_or_default() != 0 {
                    return Err(anyhow!(
                        "vision-draft-complexity run exited with non-zero code: {:?}",
                        exit_code
                    ));
                }
                break;
            }
            _ => {}
        }
    }

    let proposal = parse_vision_refinement_from_text(&transcript).ok_or_else(|| {
        anyhow!("vision-draft-complexity output did not include a valid JSON payload")
    })?;
    let complexity_proposal = proposal.complexity_assessment.ok_or_else(|| {
        anyhow!("vision-draft-complexity output did not include complexity_assessment")
    })?;
    let context_vision = build_context_vision(project_root, input);
    let mut assessment = assessment_from_proposal(Some(complexity_proposal), &context_vision);
    assessment.source = Some("ai-vision-draft".to_string());
    Ok(assessment)
}

pub(super) async fn draft_vision_with_ai_complexity(
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
    mut input: VisionDraftInput,
    options: VisionDraftAiOptions,
) -> Result<VisionDocument> {
    let needs_complexity = input.complexity_assessment.is_none()
        || input
            .complexity_assessment
            .as_ref()
            .and_then(|value| value.source.as_deref())
            .is_some_and(|source| source.eq_ignore_ascii_case("heuristic"));

    if needs_complexity {
        if options.use_ai_complexity {
            match request_ai_complexity_assessment(hub.clone(), project_root, &input, &options)
                .await
            {
                Ok(assessment) => input.complexity_assessment = Some(assessment),
                Err(error) => {
                    if options.allow_heuristic_fallback {
                        let mut fallback = infer_complexity_from_vision(&build_context_vision(
                            project_root,
                            &input,
                        ));
                        fallback.source = Some("heuristic-fallback".to_string());
                        fallback.rationale = Some(format!(
                            "{} (AI complexity unavailable; fallback applied)",
                            fallback.rationale.unwrap_or_default()
                        ));
                        input.complexity_assessment = Some(fallback);
                    } else {
                        return Err(anyhow!(
                            "AI complexity assessment failed: {error}. Re-run with --allow-heuristic-fallback true to allow fallback."
                        ));
                    }
                }
            }
        } else {
            let mut fallback =
                infer_complexity_from_vision(&build_context_vision(project_root, &input));
            fallback.source = Some("heuristic".to_string());
            input.complexity_assessment = Some(fallback);
        }
    }

    let drafted = hub.planning().draft_vision(input).await?;
    if options.use_ai_complexity && !options.allow_heuristic_fallback {
        let is_ai_source = drafted
            .complexity_assessment
            .as_ref()
            .and_then(|assessment| assessment.source.as_deref())
            .map(|source| is_ai_complexity_source(Some(source)))
            .unwrap_or(false);
        if !is_ai_source {
            return Err(anyhow!(
                "vision complexity assessment was not AI-sourced. Re-run with a reachable runner/model or pass --allow-heuristic-fallback true."
            ));
        }
    }
    Ok(drafted)
}
