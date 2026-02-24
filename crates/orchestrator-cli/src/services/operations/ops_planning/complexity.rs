use orchestrator_core::{
    ComplexityAssessment, ComplexityTier, RequirementRange, TaskDensity, VisionDocument,
};

use super::types::ComplexityAssessmentProposal;

fn parse_complexity_tier(value: &str) -> Option<ComplexityTier> {
    match value.trim().to_ascii_lowercase().as_str() {
        "simple" | "low" => Some(ComplexityTier::Simple),
        "medium" | "moderate" => Some(ComplexityTier::Medium),
        "complex" | "high" => Some(ComplexityTier::Complex),
        _ => None,
    }
}

fn parse_task_density(value: &str) -> Option<TaskDensity> {
    match value.trim().to_ascii_lowercase().as_str() {
        "low" | "sparse" => Some(TaskDensity::Low),
        "medium" | "balanced" => Some(TaskDensity::Medium),
        "high" | "dense" => Some(TaskDensity::High),
        _ => None,
    }
}

fn complexity_defaults_for_tier(tier: ComplexityTier) -> (RequirementRange, TaskDensity) {
    match tier {
        ComplexityTier::Simple => (RequirementRange { min: 4, max: 8 }, TaskDensity::Low),
        ComplexityTier::Medium => (RequirementRange { min: 8, max: 14 }, TaskDensity::Medium),
        ComplexityTier::Complex => (RequirementRange { min: 14, max: 30 }, TaskDensity::High),
    }
}

fn normalize_requirement_range(mut range: RequirementRange) -> RequirementRange {
    if range.min == 0 {
        range.min = 1;
    }
    if range.max == 0 {
        range.max = range.min.max(1);
    }
    if range.max < range.min {
        range.max = range.min;
    }
    range
}

fn clamp_requirement_range_for_tier(
    tier: ComplexityTier,
    range: RequirementRange,
) -> RequirementRange {
    let bounds = complexity_defaults_for_tier(tier).0;
    let mut clamped = normalize_requirement_range(range);
    clamped.min = clamped.min.clamp(bounds.min, bounds.max);
    clamped.max = clamped.max.clamp(bounds.min, bounds.max);
    if clamped.max < clamped.min {
        clamped.max = clamped.min;
    }
    clamped
}

fn complexity_rank(tier: ComplexityTier) -> u8 {
    match tier {
        ComplexityTier::Simple => 0,
        ComplexityTier::Medium => 1,
        ComplexityTier::Complex => 2,
    }
}

pub(super) fn infer_complexity_from_vision(vision: &VisionDocument) -> ComplexityAssessment {
    let joined = format!(
        "{} {} {} {}",
        vision.problem_statement,
        vision.target_users.join(" "),
        vision.goals.join(" "),
        vision.constraints.join(" ")
    )
    .to_ascii_lowercase();
    let mut score = 0i32;
    for needle in [
        "enterprise",
        "compliance",
        "audit",
        "multi tenant",
        "multi-region",
        "throughput",
        "role-based",
        "rbac",
        "immutable",
        "forecast",
        "erp",
        "governance",
        "data residency",
        "saml",
        "oidc",
        "disaster recovery",
        "high availability",
        "usage based billing",
        "event sourcing",
        "stream processing",
        "zero trust",
    ] {
        if joined.contains(needle) {
            score += 2;
        }
    }
    for needle in [
        "platform",
        "workflow",
        "pipeline",
        "integration",
        "webhook",
        "approval",
        "review",
        "phase gate",
        "analytics",
        "dashboard",
        "background job",
        "queue",
        "multi-step",
        "postgres",
        "redis",
        "search",
        "billing",
        "subscription",
        "credits",
    ] {
        if joined.contains(needle) {
            score += 1;
        }
    }
    for needle in [
        "simple",
        "solo",
        "no-code",
        "mvp",
        "one-click",
        "minimal setup",
        "single user",
        "single page",
        "internal tool",
        "manual process",
        "no auth",
    ] {
        if joined.contains(needle) {
            score -= 1;
        }
    }
    score += (vision.goals.len() as i32).saturating_sub(4);
    score += (vision.constraints.len() as i32).saturating_sub(4);
    if vision.goals.len() >= 8 {
        score += 1;
    }
    if vision.constraints.len() >= 8 {
        score += 1;
    }

    let tier = if score <= 2 {
        ComplexityTier::Simple
    } else if score >= 9 {
        ComplexityTier::Complex
    } else {
        ComplexityTier::Medium
    };
    let (recommended_requirement_range, task_density) = complexity_defaults_for_tier(tier);
    let distance = match tier {
        ComplexityTier::Simple => (2 - score).max(0) as f64,
        ComplexityTier::Complex => (score - 9).max(0) as f64,
        ComplexityTier::Medium => {
            let center = 5.5f64;
            (score as f64 - center).abs() / 2.0
        }
    };
    let confidence = (0.55 + distance * 0.04).clamp(0.55, 0.9) as f32;
    ComplexityAssessment {
        tier,
        confidence,
        rationale: Some(
            "Complexity inferred from vision scope, constraints, and delivery expectations."
                .to_string(),
        ),
        recommended_requirement_range,
        task_density,
        source: Some("heuristic".to_string()),
    }
}

pub(super) fn assessment_from_proposal(
    proposal: Option<ComplexityAssessmentProposal>,
    current: &VisionDocument,
) -> ComplexityAssessment {
    let inferred = infer_complexity_from_vision(current);
    let mut fallback = current
        .complexity_assessment
        .clone()
        .unwrap_or_else(|| inferred.clone());
    if fallback
        .source
        .as_deref()
        .map(|value| value.eq_ignore_ascii_case("heuristic"))
        .unwrap_or(true)
        && complexity_rank(inferred.tier) > complexity_rank(fallback.tier)
    {
        fallback = inferred;
    }
    fallback.recommended_requirement_range =
        clamp_requirement_range_for_tier(fallback.tier, fallback.recommended_requirement_range);
    fallback.confidence = fallback.confidence.clamp(0.0, 1.0);

    let Some(proposal) = proposal else {
        return fallback;
    };

    let proposed_tier = proposal
        .tier
        .as_deref()
        .and_then(parse_complexity_tier)
        .unwrap_or(fallback.tier);
    let mut tier = proposed_tier;
    let (default_range, _default_density) = complexity_defaults_for_tier(tier);
    let proposed_range = proposal
        .recommended_requirement_range
        .and_then(|raw| {
            let min = raw.min?;
            let max = raw.max?;
            Some(RequirementRange { min, max })
        })
        .unwrap_or_else(|| default_range.clone());
    let confidence = proposal
        .confidence
        .or(Some(fallback.confidence))
        .unwrap_or(0.6)
        .clamp(0.0, 1.0);
    let tier_gap = complexity_rank(tier).abs_diff(complexity_rank(fallback.tier));
    let mut used_fallback_tier = false;
    if (tier_gap >= 2 && confidence < 0.8) || (tier_gap == 1 && confidence < 0.55) {
        tier = fallback.tier;
        used_fallback_tier = true;
    }
    let task_density = proposal
        .task_density
        .as_deref()
        .and_then(parse_task_density)
        .unwrap_or(fallback.task_density);
    let range_seed = if used_fallback_tier {
        fallback.recommended_requirement_range.clone()
    } else {
        proposed_range
    };
    let range = clamp_requirement_range_for_tier(tier, range_seed);
    let rationale = proposal.rationale.or(fallback.rationale);

    ComplexityAssessment {
        tier,
        confidence,
        rationale,
        recommended_requirement_range: range,
        task_density,
        source: Some("ai-vision-refine".to_string()),
    }
}
