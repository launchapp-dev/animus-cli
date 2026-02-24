use std::sync::Arc;

use anyhow::{anyhow, Result};
use orchestrator_core::services::ServiceHub;
use protocol::{AgentRunEvent, AgentRunRequest, ModelId, RunId, PROTOCOL_VERSION};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, BufReader};
use uuid::Uuid;

use super::complexity::assessment_from_proposal;
use super::prompt_template::build_vision_refinement_prompt;
use super::refinement_apply::apply_vision_refinement;
use super::refinement_parse::parse_vision_refinement_from_text;
use super::types::{
    VisionRefineInputPayload, VisionRefineResultOutput, VisionRefinementMeta,
    VisionRefinementProposal,
};
use crate::{
    build_runtime_contract, connect_runner, event_matches_run, runner_config_dir, write_json_line,
};

fn refine_vision_heuristically(
    vision: &orchestrator_core::VisionDocument,
    focus: Option<&str>,
) -> VisionRefinementProposal {
    let mut proposal = VisionRefinementProposal::default();
    let goals_haystack = vision.goals.join(" ").to_ascii_lowercase();
    if !goals_haystack.contains("success metric") && !goals_haystack.contains("kpi") {
        proposal.goals_additions.push(
            "Define measurable success metrics (activation, retention, revenue impact) and decision thresholds for go/no-go."
                .to_string(),
        );
    }
    let constraints_haystack = vision.constraints.join(" ").to_ascii_lowercase();
    if !constraints_haystack.contains("traceable")
        && !constraints_haystack.contains("machine-readable")
    {
        proposal.constraints_additions.push(
            "Requirements, tasks, and workflow artifacts must remain traceable and machine-readable."
                .to_string(),
        );
    }
    if let Some(focus) = focus.map(str::trim).filter(|value| !value.is_empty()) {
        proposal.constraints_additions.push(format!(
            "Refinement focus must be explicitly represented in requirements and acceptance criteria: {focus}."
        ));
    }
    proposal.rationale = Some(
        "Heuristic refinement added missing measurable outcomes and traceability guardrails."
            .to_string(),
    );
    proposal
}

async fn request_ai_vision_refinement(
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
    vision: &orchestrator_core::VisionDocument,
    input: &VisionRefineInputPayload,
) -> Result<VisionRefinementProposal> {
    if input.start_runner {
        hub.daemon().start().await?;
    }

    let prompt = build_vision_refinement_prompt(vision, input);
    let run_id = RunId(format!("vision-refine-{}", Uuid::new_v4()));
    let mut context = serde_json::json!({
        "tool": input.tool,
        "prompt": prompt,
        "cwd": project_root,
        "project_root": project_root,
        "planning_stage": "vision-refine",
    });
    if let Some(timeout_secs) = input.timeout_secs {
        context["timeout_secs"] = Value::from(timeout_secs);
    }
    if let Some(runtime_contract) = build_runtime_contract(&input.tool, &input.model, &prompt) {
        context["runtime_contract"] = runtime_contract;
    }

    let request = AgentRunRequest {
        protocol_version: PROTOCOL_VERSION.to_string(),
        run_id: run_id.clone(),
        model: ModelId(input.model.clone()),
        context,
        timeout_secs: input.timeout_secs,
    };

    let config_dir = runner_config_dir(std::path::Path::new(project_root));
    let stream = connect_runner(&config_dir).await?;
    let (read_half, mut write_half) = tokio::io::split(stream);
    write_json_line(&mut write_half, &request).await?;

    let mut lines = BufReader::new(read_half).lines();
    let mut transcript = String::new();
    while let Some(line) = lines.next_line().await? {
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
                return Err(anyhow!("vision refinement run failed: {error}"));
            }
            AgentRunEvent::Finished { exit_code, .. } => {
                if exit_code.unwrap_or_default() != 0 {
                    return Err(anyhow!(
                        "vision refinement run exited with non-zero code: {:?}",
                        exit_code
                    ));
                }
                break;
            }
            _ => {}
        }
    }

    parse_vision_refinement_from_text(&transcript).ok_or_else(|| {
        anyhow!("vision refinement model output did not include a valid JSON proposal")
    })
}

pub(super) async fn run_vision_refine(
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
    input: VisionRefineInputPayload,
) -> Result<VisionRefineResultOutput> {
    let planning = hub.planning();
    let Some(current) = planning.get_vision().await? else {
        return Err(anyhow!(
            "vision not found; run `ao planning vision draft` first"
        ));
    };

    let (proposal, mode, fallback_reason) = if input.use_ai {
        match request_ai_vision_refinement(hub.clone(), project_root, &current, &input).await {
            Ok(ai_proposal) => (ai_proposal, "ai".to_string(), None),
            Err(error) => {
                if input.allow_heuristic_fallback {
                    (
                        refine_vision_heuristically(&current, input.focus.as_deref()),
                        "heuristic".to_string(),
                        Some(error.to_string()),
                    )
                } else {
                    return Err(anyhow!(
                        "AI vision refinement failed: {error}. Re-run with --allow-heuristic-fallback true to allow heuristic fallback."
                    ));
                }
            }
        }
    } else {
        (
            refine_vision_heuristically(&current, input.focus.as_deref()),
            "heuristic".to_string(),
            None,
        )
    };

    let rationale = proposal.rationale.clone();
    let complexity_assessment =
        assessment_from_proposal(proposal.complexity_assessment.clone(), &current);
    let (refined_input, changes) = apply_vision_refinement(
        &current,
        proposal,
        input.preserve_core,
        complexity_assessment.clone(),
    );
    let updated_vision = planning.draft_vision(refined_input).await?;

    Ok(VisionRefineResultOutput {
        updated_vision,
        refinement: VisionRefinementMeta {
            mode,
            focus: input.focus,
            tool: input.use_ai.then_some(input.tool),
            model: input.use_ai.then_some(input.model),
            rationale,
            fallback_reason,
            complexity_assessment,
            changes,
        },
    })
}
