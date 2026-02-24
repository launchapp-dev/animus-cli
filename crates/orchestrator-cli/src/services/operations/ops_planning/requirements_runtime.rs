use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use anyhow::{anyhow, Result};
use chrono::Utc;
use orchestrator_core::{
    services::ServiceHub, RequirementItem, RequirementPriority, RequirementStatus,
};
use protocol::{
    default_fallback_models_for_phase, tool_for_model_id, AgentRunEvent, AgentRunRequest, ModelId,
    RunId, PROTOCOL_VERSION,
};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::Semaphore;
use tokio::task::JoinSet;
use tokio::time::{Duration, Instant};
use uuid::Uuid;

use super::draft_runtime::is_ai_complexity_source;
use super::requirements_parse::parse_requirements_draft_from_text;
use super::requirements_prompt::{
    build_requirement_po_draft_prompt, build_requirements_draft_prompt,
    build_requirements_refine_prompt, build_requirements_repair_prompt,
};
use super::types::{
    default_requirement_links, default_requirement_status, parse_requirement_priority,
    parse_requirement_type, PerspectiveSummary, RequirementDraftCandidate,
    RequirementsDraftInputPayload, RequirementsDraftProposal, RequirementsDraftResultOutput,
    RequirementsRefineInputPayload,
};
use crate::{
    build_runtime_contract, connect_runner, event_matches_run, runner_config_dir, write_json_line,
};

fn requirements_debug_enabled() -> bool {
    std::env::var("AO_DEBUG_REQUIREMENTS_DRAFT")
        .ok()
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}

fn requirements_debug(message: &str) {
    if requirements_debug_enabled() {
        eprintln!("[ao:req-draft] {message}");
    }
}

const PERSPECTIVE_PRODUCT_TAG: &str = "lens-product";
const PERSPECTIVE_ENGINEERING_TAG: &str = "lens-engineering";
const PERSPECTIVE_OPS_TAG: &str = "lens-ops";

fn normalize_text_for_match(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch.is_ascii_whitespace() {
                ch.to_ascii_lowercase()
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn significant_constraint_tokens(constraint: &str) -> Vec<String> {
    const STOP_WORDS: &[&str] = &[
        "the",
        "and",
        "for",
        "with",
        "that",
        "must",
        "should",
        "into",
        "from",
        "this",
        "have",
        "has",
        "are",
        "our",
        "your",
        "their",
        "using",
        "use",
        "include",
        "supports",
        "support",
        "ready",
        "readiness",
        "mvp",
        "system",
    ];

    normalize_text_for_match(constraint)
        .split_whitespace()
        .filter(|token| token.len() > 3 && !STOP_WORDS.contains(token))
        .map(|token| token.to_string())
        .collect()
}

fn candidate_haystack(candidate: &RequirementDraftCandidate) -> String {
    let mut chunks = vec![candidate.title.clone(), candidate.description.clone()];
    chunks.extend(candidate.acceptance_criteria.clone());
    chunks.extend(candidate.tags.clone());
    normalize_text_for_match(&chunks.join(" "))
}

fn candidate_covers_constraint(candidate: &RequirementDraftCandidate, constraint: &str) -> bool {
    let normalized = normalize_text_for_match(constraint);
    if normalized.is_empty() {
        return true;
    }
    let haystack = candidate_haystack(candidate);
    if haystack.contains(&normalized) {
        return true;
    }
    let tokens = significant_constraint_tokens(constraint);
    !tokens.is_empty() && tokens.iter().all(|token| haystack.contains(token))
}

fn has_tag_ignore_case(tags: &[String], expected: &str) -> bool {
    tags.iter().any(|tag| tag.eq_ignore_ascii_case(expected))
}

fn infer_candidate_perspective_tags(candidate: &RequirementDraftCandidate) -> Vec<&'static str> {
    let mut inferred = Vec::new();
    let haystack = candidate_haystack(candidate);
    let requirement_type = candidate
        .requirement_type
        .as_deref()
        .unwrap_or("")
        .to_ascii_lowercase();

    if requirement_type == "product"
        || haystack.contains("user journey")
        || haystack.contains("screen")
        || haystack.contains("workspace")
        || haystack.contains("out of scope")
        || haystack.contains("out-of-scope")
    {
        inferred.push(PERSPECTIVE_PRODUCT_TAG);
    }

    if requirement_type == "technical"
        || requirement_type == "non-functional"
        || haystack.contains("domain model")
        || haystack.contains("api contract")
        || haystack.contains("integration contract")
        || haystack.contains("schema")
        || haystack.contains("state transition")
        || haystack.contains("security")
        || haystack.contains("compliance")
    {
        inferred.push(PERSPECTIVE_ENGINEERING_TAG);
    }

    if haystack.contains("admin")
        || haystack.contains("billing")
        || haystack.contains("tenant")
        || haystack.contains("operations")
        || haystack.contains("runbook")
        || haystack.contains("incident")
        || haystack.contains("support")
        || haystack.contains("observability")
    {
        inferred.push(PERSPECTIVE_OPS_TAG);
    }

    inferred
}

fn normalize_candidate(
    mut candidate: RequirementDraftCandidate,
    index: usize,
) -> RequirementDraftCandidate {
    let fallback_title = format!("Requirement {}", index + 1);
    candidate.title = candidate.title.trim().to_string();
    if candidate.title.is_empty() {
        candidate.title = fallback_title.clone();
    }

    candidate.description = candidate.description.trim().to_string();
    if candidate.description.is_empty() {
        candidate.description = format!(
            "Deliver a complete and testable implementation for: {}.",
            candidate.title
        );
    }

    let mut acceptance_criteria = candidate
        .acceptance_criteria
        .into_iter()
        .map(|criterion| criterion.trim().to_string())
        .filter(|criterion| !criterion.is_empty())
        .collect::<Vec<_>>();
    if acceptance_criteria.is_empty() {
        acceptance_criteria = vec![
            format!(
                "Given the requirement '{}', when implemented, the core user flow succeeds.",
                candidate.title
            ),
            "Validation includes automated test coverage for the critical path.".to_string(),
            "Operational and error handling behavior is explicitly verified.".to_string(),
        ];
    }
    let mut acceptance_seen = HashSet::new();
    acceptance_criteria.retain(|criterion| {
        let key = normalize_text_for_match(criterion);
        !key.is_empty() && acceptance_seen.insert(key)
    });
    candidate.acceptance_criteria = acceptance_criteria;

    candidate.tags = candidate
        .tags
        .into_iter()
        .map(|tag| tag.trim().to_string())
        .filter(|tag| !tag.is_empty())
        .collect::<Vec<_>>();
    candidate.tags.sort();
    candidate.tags.dedup();
    for perspective_tag in infer_candidate_perspective_tags(&candidate) {
        if !has_tag_ignore_case(&candidate.tags, perspective_tag) {
            candidate.tags.push(perspective_tag.to_string());
        }
    }
    candidate.tags.sort();
    candidate.tags.dedup();

    if candidate
        .source
        .as_deref()
        .map(str::trim)
        .unwrap_or("")
        .is_empty()
    {
        candidate.source = Some("ai-draft".to_string());
    }
    candidate.id = candidate
        .id
        .map(|id| id.trim().to_string())
        .filter(|id| !id.is_empty());

    candidate
}

fn priority_weight(priority: Option<&str>) -> u8 {
    match priority.unwrap_or("").trim().to_ascii_lowercase().as_str() {
        "must" => 4,
        "should" => 3,
        "could" => 2,
        "wont" | "won't" => 1,
        _ => 0,
    }
}

fn parse_priority_strict(priority: Option<&str>) -> Option<RequirementPriority> {
    match priority.unwrap_or("").trim().to_ascii_lowercase().as_str() {
        "must" => Some(RequirementPriority::Must),
        "should" => Some(RequirementPriority::Should),
        "could" => Some(RequirementPriority::Could),
        "wont" | "won't" => Some(RequirementPriority::Wont),
        _ => None,
    }
}

fn stronger_priority(left: Option<String>, right: Option<String>) -> Option<String> {
    let left_weight = priority_weight(left.as_deref());
    let right_weight = priority_weight(right.as_deref());
    if right_weight > left_weight {
        right
    } else {
        left
    }
}

fn merge_candidate(primary: &mut RequirementDraftCandidate, incoming: RequirementDraftCandidate) {
    if incoming.description.len() > primary.description.len() {
        primary.description = incoming.description;
    }
    if primary
        .category
        .as_deref()
        .map(str::trim)
        .unwrap_or("")
        .is_empty()
    {
        primary.category = incoming.category;
    }
    if primary
        .requirement_type
        .as_deref()
        .map(str::trim)
        .unwrap_or("")
        .is_empty()
    {
        primary.requirement_type = incoming.requirement_type;
    }
    primary.priority = stronger_priority(primary.priority.clone(), incoming.priority);

    let mut merged_criteria = primary.acceptance_criteria.clone();
    merged_criteria.extend(incoming.acceptance_criteria);
    let mut criteria_seen = HashSet::new();
    merged_criteria.retain(|criterion| {
        let key = normalize_text_for_match(criterion);
        !key.is_empty() && criteria_seen.insert(key)
    });
    primary.acceptance_criteria = merged_criteria;

    primary.tags.extend(incoming.tags);
    primary.tags.sort();
    primary.tags.dedup();

    if primary
        .source
        .as_deref()
        .map(str::trim)
        .unwrap_or("")
        .is_empty()
    {
        primary.source = incoming.source;
    }
}

fn token_set(value: &str) -> HashSet<String> {
    normalize_text_for_match(value)
        .split_whitespace()
        .map(ToOwned::to_owned)
        .collect()
}

fn candidate_token_set(candidate: &RequirementDraftCandidate) -> HashSet<String> {
    let mut chunks = vec![candidate.title.clone(), candidate.description.clone()];
    chunks.extend(candidate.acceptance_criteria.clone());
    token_set(&chunks.join(" "))
}

fn jaccard_similarity(left: &HashSet<String>, right: &HashSet<String>) -> f32 {
    if left.is_empty() || right.is_empty() {
        return 0.0;
    }
    let intersection = left.intersection(right).count() as f32;
    let union = left.union(right).count() as f32;
    if union <= f32::EPSILON {
        0.0
    } else {
        intersection / union
    }
}

fn is_semantic_duplicate(
    left: &RequirementDraftCandidate,
    right: &RequirementDraftCandidate,
) -> bool {
    let left_title = normalize_text_for_match(&left.title);
    let right_title = normalize_text_for_match(&right.title);
    if !left_title.is_empty() && left_title == right_title {
        return true;
    }

    let left_tokens = candidate_token_set(left);
    let right_tokens = candidate_token_set(right);
    let similarity = jaccard_similarity(&left_tokens, &right_tokens);
    if similarity >= 0.82 {
        return true;
    }
    let shared = left_tokens.intersection(&right_tokens).count();
    similarity >= 0.74 && shared >= 12
}

fn normalize_and_dedupe_candidates(
    candidates: Vec<RequirementDraftCandidate>,
) -> Vec<RequirementDraftCandidate> {
    let mut deduped: Vec<RequirementDraftCandidate> = Vec::new();
    for (index, raw) in candidates.into_iter().enumerate() {
        let candidate = normalize_candidate(raw, index);
        if candidate.is_empty() {
            continue;
        }
        if let Some(existing_index) = deduped
            .iter()
            .position(|existing| is_semantic_duplicate(existing, &candidate))
        {
            if let Some(existing) = deduped.get_mut(existing_index) {
                merge_candidate(existing, candidate);
            }
        } else {
            deduped.push(candidate);
        }
    }
    deduped
}

fn missing_coverage_statements(
    statements: &[String],
    candidates: &[RequirementDraftCandidate],
) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut missing = Vec::new();
    let aggregate_haystack = candidates
        .iter()
        .map(candidate_haystack)
        .collect::<Vec<_>>()
        .join(" ");
    for statement in statements {
        let normalized = normalize_text_for_match(statement);
        if normalized.is_empty() || !seen.insert(normalized) {
            continue;
        }
        let full_coverage = candidates
            .iter()
            .any(|candidate| candidate_covers_constraint(candidate, statement));
        let tokens = significant_constraint_tokens(statement);
        let covered_token_count = tokens
            .iter()
            .filter(|token| aggregate_haystack.contains(token.as_str()))
            .count();
        let token_ratio = if tokens.is_empty() {
            1.0
        } else {
            covered_token_count as f32 / tokens.len() as f32
        };
        let covered = full_coverage
            || aggregate_haystack.contains(&normalize_text_for_match(statement))
            || covered_token_count >= 4
            || token_ratio >= 0.60;
        if !covered {
            missing.push(statement.clone());
        }
    }
    missing
}

fn has_perspective_tag(candidate: &RequirementDraftCandidate) -> bool {
    has_tag_ignore_case(&candidate.tags, PERSPECTIVE_PRODUCT_TAG)
        || has_tag_ignore_case(&candidate.tags, PERSPECTIVE_ENGINEERING_TAG)
        || has_tag_ignore_case(&candidate.tags, PERSPECTIVE_OPS_TAG)
}

fn criterion_has_negation(value: &str) -> bool {
    let normalized = normalize_text_for_match(value);
    normalized.contains("must not")
        || normalized.contains("should not")
        || normalized.contains("cannot")
        || normalized.contains("never")
        || normalized.contains(" no ")
        || normalized.starts_with("no ")
}

fn normalize_criterion_core(value: &str) -> String {
    let drop_tokens = [
        "must",
        "must not",
        "should",
        "should not",
        "shall",
        "shall not",
        "can",
        "cannot",
        "not",
        "no",
        "never",
        "always",
        "be",
    ];
    let normalized = normalize_text_for_match(value);
    normalized
        .split_whitespace()
        .filter(|token| !drop_tokens.contains(token))
        .collect::<Vec<_>>()
        .join(" ")
}

fn criteria_conflict(left: &str, right: &str) -> bool {
    let left_core = normalize_criterion_core(left);
    let right_core = normalize_criterion_core(right);
    if left_core.is_empty() || right_core.is_empty() || left_core != right_core {
        return false;
    }
    criterion_has_negation(left) != criterion_has_negation(right)
}

fn collect_quality_issues(
    vision: &orchestrator_core::VisionDocument,
    candidates: &[RequirementDraftCandidate],
) -> Vec<String> {
    let mut issues = Vec::new();
    if candidates.is_empty() {
        issues.push("no requirements were produced".to_string());
        return issues;
    }

    for left_index in 0..candidates.len() {
        for right_index in (left_index + 1)..candidates.len() {
            let left = &candidates[left_index];
            let right = &candidates[right_index];
            if is_semantic_duplicate(left, right) {
                issues.push(format!(
                    "semantic overlap detected between '{}' and '{}'",
                    left.title, right.title
                ));
            }
        }
    }

    let missing_goals = missing_coverage_statements(&vision.goals, candidates);
    if !missing_goals.is_empty() {
        issues.push(format!(
            "vision goals missing requirement coverage: {}",
            missing_goals.join(" | ")
        ));
    }

    let missing_constraints = missing_coverage_statements(&vision.constraints, candidates);
    if !missing_constraints.is_empty() {
        issues.push(format!(
            "vision constraints missing requirement coverage: {}",
            missing_constraints.join(" | ")
        ));
    }

    let total = candidates.len();
    let must_count = candidates
        .iter()
        .filter(|candidate| priority_weight(candidate.priority.as_deref()) == 4)
        .count();
    let should_or_could_count = candidates
        .iter()
        .filter(|candidate| matches!(priority_weight(candidate.priority.as_deref()), 2 | 3))
        .count();
    if total >= 4 && must_count * 10 > total * 8 {
        issues.push(format!(
            "priority distribution is imbalanced: {must_count}/{total} requirements are must"
        ));
    }
    if total >= 4 && should_or_could_count == 0 {
        issues.push("priority distribution missing should/could requirements".to_string());
    }

    for candidate in candidates {
        if !has_perspective_tag(candidate) {
            issues.push(format!(
                "requirement '{}' is missing a perspective tag",
                candidate.title
            ));
        }
        if candidate.acceptance_criteria.len() < 3 || candidate.acceptance_criteria.len() > 8 {
            issues.push(format!(
                "requirement '{}' has {} acceptance criteria; expected 3-8",
                candidate.title,
                candidate.acceptance_criteria.len()
            ));
        }

        let mut criteria_seen = HashSet::new();
        for criterion in &candidate.acceptance_criteria {
            let normalized = normalize_text_for_match(criterion);
            if normalized.is_empty() {
                issues.push(format!(
                    "requirement '{}' includes an empty acceptance criterion",
                    candidate.title
                ));
                continue;
            }
            if !criteria_seen.insert(normalized.clone()) {
                issues.push(format!(
                    "requirement '{}' has duplicate acceptance criteria",
                    candidate.title
                ));
            }
        }

        for left_index in 0..candidate.acceptance_criteria.len() {
            for right_index in (left_index + 1)..candidate.acceptance_criteria.len() {
                let left = &candidate.acceptance_criteria[left_index];
                let right = &candidate.acceptance_criteria[right_index];
                if criteria_conflict(left, right) {
                    issues.push(format!(
                        "requirement '{}' has conflicting acceptance criteria",
                        candidate.title
                    ));
                }
            }
        }
    }

    issues
}

fn collect_refine_quality_issues(candidates: &[RequirementDraftCandidate]) -> Vec<String> {
    let mut issues = Vec::new();
    for left_index in 0..candidates.len() {
        for right_index in (left_index + 1)..candidates.len() {
            let left = &candidates[left_index];
            let right = &candidates[right_index];
            if is_semantic_duplicate(left, right) {
                issues.push(format!(
                    "semantic overlap detected between '{}' and '{}'",
                    left.title, right.title
                ));
            }
        }
    }

    for candidate in candidates {
        if !has_perspective_tag(candidate) {
            issues.push(format!(
                "requirement '{}' is missing a perspective tag",
                candidate.title
            ));
        }
        if candidate.acceptance_criteria.len() < 3 || candidate.acceptance_criteria.len() > 8 {
            issues.push(format!(
                "requirement '{}' has {} acceptance criteria; expected 3-8",
                candidate.title,
                candidate.acceptance_criteria.len()
            ));
        }
        for left_index in 0..candidate.acceptance_criteria.len() {
            for right_index in (left_index + 1)..candidate.acceptance_criteria.len() {
                if criteria_conflict(
                    &candidate.acceptance_criteria[left_index],
                    &candidate.acceptance_criteria[right_index],
                ) {
                    issues.push(format!(
                        "requirement '{}' has conflicting acceptance criteria",
                        candidate.title
                    ));
                }
            }
        }
    }
    issues
}

fn normalized_draft_strategy(raw: &str) -> &'static str {
    match raw.trim().to_ascii_lowercase().as_str() {
        "multi-agent" | "multi_agent" | "multi" | "po-fanout" => "multi-agent",
        _ => "single-agent",
    }
}

fn normalized_quality_repair_attempts(input: &RequirementsDraftInputPayload) -> usize {
    input.quality_repair_attempts.min(4)
}

fn is_required_surface_source(source: &str) -> bool {
    source.eq_ignore_ascii_case("vision-constraint")
        || source.eq_ignore_ascii_case("vision-product-surface")
        || source.eq_ignore_ascii_case("vision-technical-surface")
        || source.eq_ignore_ascii_case("vision-admin-ops-surface")
        || source.eq_ignore_ascii_case("vision-scope-boundary")
        || source
            .to_ascii_lowercase()
            .starts_with("vision-perspective-")
}

fn apply_requirement_count_target(
    vision: &orchestrator_core::VisionDocument,
    input: &RequirementsDraftInputPayload,
    candidates: Vec<RequirementDraftCandidate>,
) -> Vec<RequirementDraftCandidate> {
    let target_max = if input.max_requirements > 0 {
        input.max_requirements
    } else {
        vision
            .complexity_assessment
            .as_ref()
            .map(|assessment| assessment.recommended_requirement_range.max)
            .unwrap_or(16)
    };

    if target_max == 0 || candidates.len() <= target_max {
        return candidates;
    }

    let mut pinned = Vec::new();
    let mut optional = Vec::new();
    for candidate in candidates {
        if candidate
            .source
            .as_deref()
            .is_some_and(is_required_surface_source)
        {
            pinned.push(candidate);
        } else {
            optional.push(candidate);
        }
    }

    let keep_optional = target_max.saturating_sub(pinned.len());
    optional.truncate(keep_optional);
    optional.extend(pinned);
    optional
}

async fn request_ai_requirements_draft(
    project_root: &str,
    input: &RequirementsDraftInputPayload,
    vision: &orchestrator_core::VisionDocument,
    existing_requirements: &[RequirementItem],
) -> Result<RequirementsDraftProposal> {
    let existing_requirements_json =
        serde_json::to_string_pretty(existing_requirements).unwrap_or_else(|_| "[]".to_string());
    let prompt = build_requirements_draft_prompt(vision, &existing_requirements_json, input);
    let transcript = request_agent_transcript(
        project_root,
        &input.tool,
        &input.model,
        input.timeout_secs,
        "requirements-draft",
        &prompt,
    )
    .await?;

    let proposal = parse_requirements_draft_from_text(&transcript).ok_or_else(|| {
        anyhow!("requirements draft model output did not include a valid JSON proposal")
    })?;
    if proposal.requirements.is_empty() {
        return Err(anyhow!(
            "requirements draft model output returned zero requirements"
        ));
    }
    Ok(proposal)
}

async fn request_ai_requirements_repair(
    project_root: &str,
    input: &RequirementsDraftInputPayload,
    vision: &orchestrator_core::VisionDocument,
    candidates: &[RequirementDraftCandidate],
    quality_issues: &[String],
    attempt: usize,
) -> Result<RequirementsDraftProposal> {
    let candidates_json =
        serde_json::to_string_pretty(candidates).unwrap_or_else(|_| "[]".to_string());
    let issues_json =
        serde_json::to_string_pretty(quality_issues).unwrap_or_else(|_| "[]".to_string());
    let prompt =
        build_requirements_repair_prompt(vision, &candidates_json, &issues_json, input, attempt);
    let transcript = request_agent_transcript(
        project_root,
        &input.tool,
        &input.model,
        input.timeout_secs,
        "requirements-draft-repair",
        &prompt,
    )
    .await?;
    let proposal = parse_requirements_draft_from_text(&transcript).ok_or_else(|| {
        anyhow!("requirements repair model output did not include a valid JSON proposal")
    })?;
    if proposal.requirements.is_empty() {
        return Err(anyhow!(
            "requirements repair model output returned zero requirements"
        ));
    }
    Ok(proposal)
}

async fn repair_candidates_until_quality_passes(
    project_root: &str,
    input: &RequirementsDraftInputPayload,
    vision: &orchestrator_core::VisionDocument,
    initial_candidates: Vec<RequirementDraftCandidate>,
) -> Result<(Vec<RequirementDraftCandidate>, usize, usize)> {
    let mut candidates = normalize_and_dedupe_candidates(initial_candidates);
    let mut issues = collect_quality_issues(vision, &candidates);
    let initial_issue_count = issues.len();
    let max_attempts = normalized_quality_repair_attempts(input);
    let mut attempts = 0usize;

    while !issues.is_empty() && attempts < max_attempts {
        attempts = attempts.saturating_add(1);
        requirements_debug(&format!(
            "run: quality issues detected (attempt {attempts}/{max_attempts}): {}",
            issues.join(" | ")
        ));
        let proposal = request_ai_requirements_repair(
            project_root,
            input,
            vision,
            &candidates,
            &issues,
            attempts,
        )
        .await?;
        candidates = normalize_and_dedupe_candidates(proposal.requirements);
        issues = collect_quality_issues(vision, &candidates);
    }

    if !issues.is_empty() {
        return Err(anyhow!(
            "AI requirement drafting failed quality gates: {}",
            issues.join(" | ")
        ));
    }

    Ok((candidates, initial_issue_count, attempts))
}

fn requirement_draft_model_candidates(primary_model: &str) -> Vec<String> {
    let mut models = vec![primary_model.to_string()];
    models.extend(
        default_fallback_models_for_phase("requirements", None)
            .into_iter()
            .map(|model| model.to_string()),
    );
    let mut seen = HashSet::new();
    models
        .into_iter()
        .filter(|model| seen.insert(model.to_ascii_lowercase()))
        .collect()
}

async fn request_agent_transcript(
    project_root: &str,
    tool: &str,
    model: &str,
    timeout_secs: Option<u64>,
    planning_stage: &str,
    prompt: &str,
) -> Result<String> {
    let run_id = RunId(format!("{planning_stage}-{}", Uuid::new_v4()));
    let hard_timeout_secs = timeout_secs.unwrap_or(1200).max(30);
    let deadline = Instant::now() + Duration::from_secs(hard_timeout_secs);
    requirements_debug(&format!(
        "{planning_stage}: begin model={model} tool={tool} timeout={}s",
        hard_timeout_secs
    ));

    let mut context = serde_json::json!({
        "tool": tool,
        "prompt": prompt,
        "cwd": project_root,
        "project_root": project_root,
        "planning_stage": planning_stage,
        "allowed_tools": ["Read", "Glob", "Grep", "WebSearch"],
    });
    if let Some(timeout_secs) = timeout_secs {
        context["timeout_secs"] = Value::from(timeout_secs);
    }
    if let Some(runtime_contract) = build_runtime_contract(tool, model, prompt) {
        context["runtime_contract"] = runtime_contract;
    }

    let request = AgentRunRequest {
        protocol_version: PROTOCOL_VERSION.to_string(),
        run_id: run_id.clone(),
        model: ModelId(model.to_string()),
        context,
        timeout_secs,
    };

    let config_dir = runner_config_dir(std::path::Path::new(project_root));
    requirements_debug(&format!(
        "{planning_stage}: connecting runner via {}",
        config_dir.display()
    ));
    let stream = connect_runner(&config_dir).await?;
    requirements_debug(&format!("{planning_stage}: runner connected"));
    let (read_half, mut write_half) = tokio::io::split(stream);
    write_json_line(&mut write_half, &request).await?;

    let mut lines = BufReader::new(read_half).lines();
    let mut transcript = String::new();
    loop {
        let next_line = tokio::time::timeout_at(deadline, lines.next_line())
            .await
            .map_err(|_| {
                anyhow!(
                    "{planning_stage} run timed out after {}s for model {model}",
                    hard_timeout_secs
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
                return Err(anyhow!(
                    "{planning_stage} run failed for model {model}: {error}"
                ));
            }
            AgentRunEvent::Finished { exit_code, .. } => {
                if exit_code.unwrap_or_default() != 0 {
                    return Err(anyhow!(
                        "{planning_stage} run exited with non-zero code for model {model}: {:?}",
                        exit_code
                    ));
                }
                requirements_debug(&format!(
                    "{planning_stage}: finished model={model} exit={:?}",
                    exit_code
                ));
                break;
            }
            _ => {}
        }
    }

    if transcript.trim().is_empty() {
        return Err(anyhow!(
            "{planning_stage} run for model {model} produced empty output"
        ));
    }

    Ok(transcript)
}

async fn request_po_requirement_draft(
    project_root: &str,
    input: &RequirementsDraftInputPayload,
    vision: &orchestrator_core::VisionDocument,
    seed_candidate: &RequirementDraftCandidate,
    index: usize,
    total: usize,
) -> Result<RequirementDraftCandidate> {
    let model_candidates = requirement_draft_model_candidates(&input.model);
    let po_timeout_secs = input
        .timeout_secs
        .map(|secs| secs.clamp(90, 300))
        .or(Some(180));
    let prompt = build_requirement_po_draft_prompt(vision, seed_candidate, index, total);
    let mut last_error = None;

    for model in model_candidates {
        let tool = if model.eq_ignore_ascii_case(input.model.as_str()) {
            input.tool.clone()
        } else {
            tool_for_model_id(&model).to_string()
        };

        match request_agent_transcript(
            project_root,
            &tool,
            &model,
            po_timeout_secs,
            "requirements-draft-po",
            &prompt,
        )
        .await
        {
            Ok(transcript) => {
                if let Some(proposal) = parse_requirements_draft_from_text(&transcript) {
                    if let Some(mut candidate) = proposal.requirements.into_iter().next() {
                        if candidate
                            .source
                            .as_deref()
                            .map(str::trim)
                            .unwrap_or("")
                            .is_empty()
                        {
                            candidate.source = Some("po-agent-draft".to_string());
                        }
                        return Ok(candidate);
                    }
                }
                last_error = Some(anyhow!(
                    "PO requirement draft output for model {model} did not parse valid requirement payload"
                ));
            }
            Err(error) => {
                last_error = Some(error);
            }
        }
    }

    Err(last_error.unwrap_or_else(|| {
        anyhow!(
            "failed to draft requirement {} with PO agent",
            seed_candidate.title
        )
    }))
}

fn normalized_po_parallelism(input: &RequirementsDraftInputPayload) -> usize {
    input.po_parallelism.clamp(1, 8)
}

async fn draft_requirements_with_po_agents(
    project_root: &str,
    input: &RequirementsDraftInputPayload,
    vision: &orchestrator_core::VisionDocument,
    candidates: &[RequirementDraftCandidate],
) -> Result<Vec<RequirementDraftCandidate>> {
    let parallelism = normalized_po_parallelism(input);
    requirements_debug(&format!(
        "run: drafting {} requirements with PO agent (parallelism={})",
        candidates.len(),
        parallelism
    ));

    if parallelism <= 1 {
        let mut drafted_candidates = Vec::new();
        for (index, candidate) in candidates.iter().enumerate() {
            requirements_debug(&format!(
                "run: PO draft {}/{} title={}",
                index + 1,
                candidates.len(),
                candidate.title
            ));
            let drafted = request_po_requirement_draft(
                project_root,
                input,
                vision,
                candidate,
                index,
                candidates.len(),
            )
            .await?;
            let normalized = normalize_candidate(drafted, index);
            if !normalized.is_empty() {
                drafted_candidates.push(normalized);
            }
        }
        return Ok(drafted_candidates);
    }

    let semaphore = Arc::new(Semaphore::new(parallelism));
    let mut join_set = JoinSet::new();
    let total = candidates.len();
    for (index, candidate) in candidates.iter().cloned().enumerate() {
        requirements_debug(&format!(
            "run: queue PO draft {}/{} title={}",
            index + 1,
            total,
            candidate.title
        ));
        let permit = semaphore
            .clone()
            .acquire_owned()
            .await
            .map_err(|error| anyhow!("failed to acquire PO draft permit: {error}"))?;
        let project_root_owned = project_root.to_string();
        let input_owned = input.clone();
        let vision_owned = vision.clone();
        join_set.spawn(async move {
            let _permit = permit;
            let result = request_po_requirement_draft(
                &project_root_owned,
                &input_owned,
                &vision_owned,
                &candidate,
                index,
                total,
            )
            .await;
            (index, result)
        });
    }

    let mut ordered: Vec<Option<RequirementDraftCandidate>> = vec![None; total];
    while let Some(joined) = join_set.join_next().await {
        let (index, result) =
            joined.map_err(|error| anyhow!("PO draft task join failure: {error}"))?;
        let drafted = result?;
        requirements_debug(&format!("run: completed PO draft {}/{}", index + 1, total));
        let normalized = normalize_candidate(drafted, index);
        if !normalized.is_empty() {
            ordered[index] = Some(normalized);
        }
    }

    Ok(ordered.into_iter().flatten().collect())
}

fn summarize_requirement_perspectives(requirements: &[RequirementItem]) -> PerspectiveSummary {
    let mut summary = PerspectiveSummary::default();
    for requirement in requirements {
        let mut tagged = false;
        if has_tag_ignore_case(&requirement.tags, PERSPECTIVE_PRODUCT_TAG) {
            summary.product = summary.product.saturating_add(1);
            tagged = true;
        }
        if has_tag_ignore_case(&requirement.tags, PERSPECTIVE_ENGINEERING_TAG) {
            summary.engineering = summary.engineering.saturating_add(1);
            tagged = true;
        }
        if has_tag_ignore_case(&requirement.tags, PERSPECTIVE_OPS_TAG) {
            summary.ops = summary.ops.saturating_add(1);
            tagged = true;
        }
        if !tagged {
            summary.unknown = summary.unknown.saturating_add(1);
        }
    }
    summary
}

pub(crate) async fn run_requirements_draft(
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
    input: RequirementsDraftInputPayload,
) -> Result<RequirementsDraftResultOutput> {
    requirements_debug("run: begin");
    let planning = hub.planning();
    requirements_debug("run: loading vision");
    let Some(vision) = planning.get_vision().await? else {
        return Err(anyhow!(
            "vision not found; run `ao planning vision draft` first"
        ));
    };
    let complexity_source = vision
        .complexity_assessment
        .as_ref()
        .and_then(|assessment| assessment.source.as_deref());
    if !is_ai_complexity_source(complexity_source) && !input.allow_heuristic_complexity {
        return Err(anyhow!(
            "vision complexity is not AI-sourced (source: {}). Re-run `ao planning vision draft` with AI complexity enabled or pass `--allow-heuristic-complexity true`.",
            complexity_source.unwrap_or("unknown")
        ));
    }
    if input.start_runner {
        requirements_debug("run: starting daemon/runner");
        hub.daemon().start().await?;
    }
    requirements_debug("run: listing existing requirements");
    let existing_before = planning.list_requirements().await?;

    requirements_debug("run: requesting AI seed requirements");
    let proposal =
        request_ai_requirements_draft(project_root, &input, &vision, &existing_before).await?;
    let mut candidates = normalize_and_dedupe_candidates(proposal.requirements);
    candidates = apply_requirement_count_target(&vision, &input, candidates);
    let draft_strategy = normalized_draft_strategy(&input.draft_strategy);
    if draft_strategy == "multi-agent" {
        requirements_debug("run: executing multi-agent PO fanout drafting strategy");
        candidates =
            draft_requirements_with_po_agents(project_root, &input, &vision, &candidates).await?;
    } else {
        requirements_debug("run: executing single-agent drafting strategy");
    }
    let (candidates, quality_issues_detected, quality_repair_rounds) =
        repair_candidates_until_quality_passes(project_root, &input, &vision, candidates).await?;

    if !input.append_only {
        requirements_debug("run: clearing existing requirements (append_only=false)");
        for requirement in existing_before {
            planning.delete_requirement(&requirement.id).await?;
        }
    }
    let existing_titles = planning
        .list_requirements()
        .await?
        .into_iter()
        .map(|item| item.title.trim().to_ascii_lowercase())
        .collect::<HashSet<_>>();

    let mut appended = Vec::new();
    requirements_debug("run: persisting drafted requirements");
    for (index, candidate) in candidates.into_iter().enumerate() {
        let title_key = candidate.title.trim().to_ascii_lowercase();
        if input.append_only && existing_titles.contains(&title_key) {
            continue;
        }
        let now = Utc::now();
        let item = RequirementItem {
            id: String::new(),
            title: candidate.title,
            description: candidate.description.clone(),
            body: Some(candidate.description),
            legacy_id: None,
            category: candidate.category.clone(),
            requirement_type: parse_requirement_type(candidate.requirement_type.as_deref()),
            acceptance_criteria: candidate.acceptance_criteria,
            priority: parse_requirement_priority(candidate.priority.as_deref(), index),
            status: default_requirement_status(),
            source: candidate.source.unwrap_or_else(|| "ai-draft".to_string()),
            tags: candidate.tags,
            links: default_requirement_links(),
            comments: Vec::new(),
            relative_path: None,
            linked_task_ids: Vec::new(),
            created_at: now,
            updated_at: now,
        };
        let stored = planning.upsert_requirement(item).await?;
        appended.push(stored);
    }

    if appended.is_empty() {
        return Err(anyhow!(
            "AI requirement drafting produced no new requirements. Re-run with `--append-only false` to replace existing drafts."
        ));
    }
    requirements_debug(&format!("run: completed appended_count={}", appended.len()));
    let perspective_summary = summarize_requirement_perspectives(&appended);

    Ok(RequirementsDraftResultOutput {
        requirements: appended.clone(),
        appended_count: appended.len(),
        codebase_insight: None,
        mode: "ai-planning-requirements".to_string(),
        draft_strategy: draft_strategy.to_string(),
        rationale: proposal.rationale,
        perspective_summary,
        quality_issues_detected,
        quality_repair_rounds,
        tool: input.tool,
        model: input.model,
    })
}

pub(crate) async fn run_requirements_refine(
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
    input: RequirementsRefineInputPayload,
) -> Result<Vec<RequirementItem>> {
    if !input.use_ai {
        return Err(anyhow!(
            "deterministic refinement is disabled for this flow; re-run with `--use-ai true`"
        ));
    }

    let planning = hub.planning();
    let Some(vision) = planning.get_vision().await? else {
        return Err(anyhow!(
            "vision not found; run `ao planning vision draft` first"
        ));
    };

    if input.start_runner {
        hub.daemon().start().await?;
    }

    let all_requirements = planning.list_requirements().await?;
    let mut selected = all_requirements
        .iter()
        .filter(|requirement| {
            input.requirement_ids.is_empty()
                || input
                    .requirement_ids
                    .iter()
                    .any(|id| id.eq_ignore_ascii_case(requirement.id.as_str()))
        })
        .cloned()
        .collect::<Vec<_>>();
    selected.sort_by(|a, b| a.id.cmp(&b.id));

    if selected.is_empty() {
        return Err(anyhow!(
            "no requirements matched refine selection; run `ao requirements list` first"
        ));
    }

    let selected_json =
        serde_json::to_string_pretty(&selected).unwrap_or_else(|_| "[]".to_string());
    let prompt = build_requirements_refine_prompt(&vision, &selected_json, &input);
    let transcript = request_agent_transcript(
        project_root,
        &input.tool,
        &input.model,
        input.timeout_secs,
        "requirements-refine",
        &prompt,
    )
    .await?;
    let proposal = parse_requirements_draft_from_text(&transcript).ok_or_else(|| {
        anyhow!("requirements refine model output did not include a valid JSON proposal")
    })?;
    if proposal.requirements.is_empty() {
        return Err(anyhow!(
            "requirements refine model output returned zero requirement updates"
        ));
    }

    let normalized_updates = normalize_and_dedupe_candidates(proposal.requirements);
    let selected_by_id = selected
        .iter()
        .map(|requirement| (requirement.id.clone(), requirement.clone()))
        .collect::<HashMap<_, _>>();
    let selected_title_to_id = selected
        .iter()
        .map(|requirement| {
            (
                normalize_text_for_match(&requirement.title),
                requirement.id.clone(),
            )
        })
        .collect::<HashMap<_, _>>();

    let mut updates_by_id: HashMap<String, RequirementDraftCandidate> = HashMap::new();
    let mut unresolved_updates = Vec::new();
    for update in normalized_updates {
        let resolved_id = update
            .id
            .as_ref()
            .filter(|id| selected_by_id.contains_key(*id))
            .cloned()
            .or_else(|| {
                selected_title_to_id
                    .get(&normalize_text_for_match(&update.title))
                    .cloned()
            });
        let Some(resolved_id) = resolved_id else {
            unresolved_updates.push(update.title.clone());
            continue;
        };
        updates_by_id.insert(resolved_id, update);
    }

    if !unresolved_updates.is_empty() {
        return Err(anyhow!(
            "requirements refine output contained updates that could not be mapped to selected IDs: {}",
            unresolved_updates.join(" | ")
        ));
    }

    let mut missing_ids = Vec::new();
    for requirement in &selected {
        if !updates_by_id.contains_key(&requirement.id) {
            missing_ids.push(requirement.id.clone());
        }
    }
    if !missing_ids.is_empty() {
        return Err(anyhow!(
            "requirements refine output omitted selected requirement IDs: {}",
            missing_ids.join(" | ")
        ));
    }

    let refine_candidates = updates_by_id.values().cloned().collect::<Vec<_>>();
    let refine_quality_issues = collect_refine_quality_issues(&refine_candidates);
    if !refine_quality_issues.is_empty() {
        return Err(anyhow!(
            "requirements refine failed quality gates: {}",
            refine_quality_issues.join(" | ")
        ));
    }

    let mut updated_requirements = Vec::new();
    for selected_item in selected {
        let Some(candidate) = updates_by_id.remove(&selected_item.id) else {
            continue;
        };
        let now = Utc::now();
        let mut updated = selected_item.clone();
        updated.title = candidate.title;
        updated.description = candidate.description.clone();
        updated.body = Some(candidate.description);
        if let Some(category) = candidate.category {
            updated.category = Some(category);
        }
        if let Some(requirement_type_raw) = candidate.requirement_type.as_deref() {
            if let Some(requirement_type) = parse_requirement_type(Some(requirement_type_raw)) {
                updated.requirement_type = Some(requirement_type);
            }
        }
        if let Some(priority) = parse_priority_strict(candidate.priority.as_deref()) {
            updated.priority = priority;
        }
        if !candidate.acceptance_criteria.is_empty() {
            updated.acceptance_criteria = candidate.acceptance_criteria;
        }
        if !candidate.tags.is_empty() {
            updated.tags = candidate.tags;
        }
        updated.status = RequirementStatus::Refined;
        updated.source = candidate.source.unwrap_or_else(|| "ai-refine".to_string());
        updated.updated_at = now;

        let stored = planning.upsert_requirement(updated).await?;
        updated_requirements.push(stored);
    }

    updated_requirements.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(updated_requirements)
}
