use crate::types::{
    ComplexityAssessment, ComplexityTier, RequirementRange, TaskDensity, VisionDocument,
};

fn complexity_keywords_score(text: &str) -> i32 {
    let normalized = text.to_ascii_lowercase();
    let high_weight = [
        "enterprise",
        "multi-region",
        "multi tenant",
        "compliance",
        "audit",
        "governance",
        "role-based",
        "rbac",
        "sso",
        "soc2",
        "hipaa",
        "iso27001",
        "forecast",
        "erp",
        "immutable",
        "throughput",
        "scale",
        "data residency",
        "saml",
        "oidc",
        "disaster recovery",
        "high availability",
        "usage based billing",
        "event sourcing",
        "stream processing",
        "zero trust",
    ];
    let medium_weight = [
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
    ];
    let low_weight = [
        "simple",
        "mvp",
        "solo",
        "no-code",
        "one-click",
        "lightweight",
        "minimal setup",
        "local data",
        "single user",
        "single page",
        "internal tool",
        "manual process",
        "no auth",
    ];

    let mut score = 0i32;
    for needle in &high_weight {
        if normalized.contains(needle) {
            score += 2;
        }
    }
    for needle in &medium_weight {
        if normalized.contains(needle) {
            score += 1;
        }
    }
    for needle in &low_weight {
        if normalized.contains(needle) {
            score -= 1;
        }
    }
    score
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

fn defaults_for_tier(tier: ComplexityTier) -> (RequirementRange, TaskDensity) {
    let (min, max) = tier.requirement_range_defaults();
    let density = match tier {
        ComplexityTier::Simple => TaskDensity::Low,
        ComplexityTier::Medium => TaskDensity::Medium,
        ComplexityTier::Complex => TaskDensity::High,
    };
    (RequirementRange { min, max }, density)
}

fn clamp_requirement_range_for_tier(
    tier: ComplexityTier,
    range: RequirementRange,
) -> RequirementRange {
    let bounds = defaults_for_tier(tier).0;
    let mut clamped = normalize_requirement_range(range);
    clamped.min = clamped.min.clamp(bounds.min, bounds.max);
    clamped.max = clamped.max.clamp(bounds.min, bounds.max);
    if clamped.max < clamped.min {
        clamped.max = clamped.min;
    }
    clamped
}

pub(crate) fn infer_complexity_assessment(
    problem_statement: &str,
    target_users: &[String],
    goals: &[String],
    constraints: &[String],
) -> ComplexityAssessment {
    let mut score = 0i32;
    score += complexity_keywords_score(problem_statement);
    score += goals
        .iter()
        .map(|goal| complexity_keywords_score(goal))
        .sum::<i32>();
    score += constraints
        .iter()
        .map(|constraint| complexity_keywords_score(constraint))
        .sum::<i32>();
    score += target_users
        .iter()
        .map(|target| complexity_keywords_score(target))
        .sum::<i32>()
        / 2;
    score += (goals.len() as i32).saturating_sub(4);
    score += (constraints.len() as i32).saturating_sub(4);
    if goals.len() >= 8 {
        score += 1;
    }
    if constraints.len() >= 8 {
        score += 1;
    }

    let tier = if score <= 2 {
        ComplexityTier::Simple
    } else if score >= 9 {
        ComplexityTier::Complex
    } else {
        ComplexityTier::Medium
    };
    let distance = match tier {
        ComplexityTier::Simple => (2 - score).max(0) as f64,
        ComplexityTier::Complex => (score - 9).max(0) as f64,
        ComplexityTier::Medium => {
            let center = 5.5f64;
            (score as f64 - center).abs() / 2.0
        }
    };
    let confidence = (0.55 + distance * 0.04).clamp(0.55, 0.9) as f32;
    let (recommended_requirement_range, task_density) = defaults_for_tier(tier);

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

pub(crate) fn effective_complexity_assessment(vision: &VisionDocument) -> ComplexityAssessment {
    let mut assessment = vision.complexity_assessment.clone().unwrap_or_else(|| {
        infer_complexity_assessment(
            &vision.problem_statement,
            &vision.target_users,
            &vision.goals,
            &vision.constraints,
        )
    });
    assessment.recommended_requirement_range =
        clamp_requirement_range_for_tier(assessment.tier, assessment.recommended_requirement_range);
    assessment.confidence = assessment.confidence.clamp(0.0, 1.0);
    if assessment.source.is_none() {
        assessment.source = Some("vision".to_string());
    }
    assessment
}
