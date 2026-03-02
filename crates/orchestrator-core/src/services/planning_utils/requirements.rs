use crate::types::{
    CodebaseInsight, RequirementItem, RequirementLinks, RequirementPriority, RequirementStatus,
    VisionDocument,
};
use chrono::Utc;

use super::complexity::effective_complexity_assessment;

fn title_to_slug(title: &str) -> String {
    title
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .split('-')
        .filter(|chunk| !chunk.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

fn requirement_priority_for_index(index: usize) -> RequirementPriority {
    match index {
        0 => RequirementPriority::Must,
        1 | 2 => RequirementPriority::Should,
        _ => RequirementPriority::Could,
    }
}

fn requirement_from_goal(
    goal: &str,
    priority: RequirementPriority,
    source: &str,
    now: chrono::DateTime<Utc>,
) -> RequirementItem {
    let normalized_goal = goal.trim();
    let title = if normalized_goal.is_empty() {
        "Define initial product behavior".to_string()
    } else {
        normalized_goal.to_string()
    };
    let slug = title_to_slug(&title);

    RequirementItem {
        id: String::new(),
        title: format!("{} ({})", title, slug),
        description: format!(
            "Translate the goal into a deliverable requirement: {}",
            if normalized_goal.is_empty() {
                "Define initial product behavior"
            } else {
                normalized_goal
            }
        ),
        body: None,
        legacy_id: None,
        category: None,
        requirement_type: None,
        acceptance_criteria: vec![
            "Repository contains runnable product code that implements the goal end-to-end."
                .to_string(),
            "APIs and data contracts needed by the goal are implemented with persisted run history."
                .to_string(),
            "Automated tests cover the critical user journey for this goal.".to_string(),
        ],
        priority,
        status: RequirementStatus::Draft,
        source: source.to_string(),
        tags: Vec::new(),
        links: RequirementLinks::default(),
        comments: Vec::new(),
        relative_path: None,
        linked_task_ids: Vec::new(),
        created_at: now,
        updated_at: now,
    }
}

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

fn requirement_text_haystack(requirement: &RequirementItem) -> String {
    let mut chunks = vec![
        requirement.title.clone(),
        requirement.description.clone(),
        requirement.source.clone(),
    ];
    if let Some(body) = &requirement.body {
        chunks.push(body.clone());
    }
    chunks.extend(requirement.acceptance_criteria.clone());
    chunks.extend(requirement.tags.clone());
    normalize_text_for_match(&chunks.join(" "))
}

fn constraint_requirement_tags(constraint: &str) -> Vec<String> {
    let normalized = normalize_text_for_match(constraint);
    let mut tags = vec!["constraint".to_string(), "vision-constraint".to_string()];

    if normalized.contains("next js")
        || normalized.contains("nextjs")
        || normalized.contains("app router")
    {
        tags.push("frontend".to_string());
        tags.push("nextjs".to_string());
    }
    if normalized.contains("typescript") {
        tags.push("typescript".to_string());
    }
    if normalized.contains("postgres") || normalized.contains("postgresql") {
        tags.push("database".to_string());
        tags.push("postgres".to_string());
    }
    if normalized.contains("auth") || normalized.contains("authentication") {
        tags.push("auth".to_string());
        tags.push("security".to_string());
    }
    if normalized.contains("workspace") || normalized.contains("multi tenant") {
        tags.push("workspace".to_string());
        tags.push("multi-tenant".to_string());
    }
    if normalized.contains("billing")
        || normalized.contains("credit")
        || normalized.contains("subscription")
        || normalized.contains("stripe")
    {
        tags.push("billing".to_string());
        tags.push("credits".to_string());
    }

    tags.sort();
    tags.dedup();
    tags
}

fn requirement_from_constraint(
    constraint: &str,
    now: chrono::DateTime<Utc>,
) -> Option<RequirementItem> {
    let constraint = constraint.trim();
    if constraint.is_empty() {
        return None;
    }

    Some(RequirementItem {
        id: String::new(),
        title: format!("Satisfy vision constraint: {}", constraint),
        description: format!(
            "Constraint from product vision that must remain enforced across requirements, implementation, and QA: {}",
            constraint
        ),
        body: None,
        legacy_id: None,
        category: None,
        requirement_type: None,
        acceptance_criteria: vec![
            format!(
                "Implementation demonstrates explicit compliance with vision constraint: {}.",
                constraint
            ),
            "Verification artifacts (tests/checklists/review notes) confirm the constraint is enforced."
                .to_string(),
        ],
        priority: RequirementPriority::Must,
        status: RequirementStatus::Draft,
        source: "vision-constraint".to_string(),
        tags: constraint_requirement_tags(constraint),
        links: RequirementLinks::default(),
        comments: Vec::new(),
        relative_path: None,
        linked_task_ids: Vec::new(),
        created_at: now,
        updated_at: now,
    })
}

fn requirements_from_constraints(
    vision: &VisionDocument,
    now: chrono::DateTime<Utc>,
) -> Vec<RequirementItem> {
    let assessment = effective_complexity_assessment(vision);
    let mut seen = std::collections::HashSet::new();
    let mut unique_constraints = Vec::new();
    for constraint in &vision.constraints {
        let normalized = normalize_text_for_match(constraint);
        if normalized.is_empty() || !seen.insert(normalized) {
            continue;
        }
        unique_constraints.push(constraint.trim().to_string());
    }

    let dedicated_limit = assessment.tier.dedicated_requirement_limit();

    let mut requirements = Vec::new();
    let mut remaining = Vec::new();
    for (index, constraint) in unique_constraints.iter().enumerate() {
        if index < dedicated_limit {
            if let Some(requirement) = requirement_from_constraint(constraint, now) {
                requirements.push(requirement);
            }
        } else {
            remaining.push(constraint.clone());
        }
    }

    if !remaining.is_empty() {
        let mut tags = vec![
            "constraint".to_string(),
            "vision-constraint".to_string(),
            "constraint-matrix".to_string(),
        ];
        for constraint in &remaining {
            tags.extend(constraint_requirement_tags(constraint));
        }
        tags.sort();
        tags.dedup();

        requirements.push(RequirementItem {
            id: String::new(),
            title: "Satisfy remaining vision constraints as a compliance matrix".to_string(),
            description: format!(
                "Consolidated enforcement requirement for additional vision constraints: {}",
                remaining.join(" | ")
            ),
            body: None,
            legacy_id: None,
            category: None,
            requirement_type: None,
            acceptance_criteria: remaining
                .iter()
                .map(|constraint| {
                    format!("Implementation demonstrates compliance with: {constraint}.")
                })
                .collect(),
            priority: RequirementPriority::Must,
            status: RequirementStatus::Draft,
            source: "vision-constraint".to_string(),
            tags,
            links: RequirementLinks::default(),
            comments: Vec::new(),
            relative_path: None,
            linked_task_ids: Vec::new(),
            created_at: now,
            updated_at: now,
        });
    }

    requirements
}

pub(crate) fn missing_vision_constraint_coverage(
    vision: Option<&VisionDocument>,
    requirements: &[RequirementItem],
) -> Vec<String> {
    let Some(vision) = vision else {
        return Vec::new();
    };

    if vision.constraints.is_empty() {
        return Vec::new();
    }

    let haystacks = requirements
        .iter()
        .map(requirement_text_haystack)
        .collect::<Vec<_>>();
    let mut missing = Vec::new();
    let mut seen = std::collections::HashSet::new();

    for constraint in &vision.constraints {
        let trimmed = constraint.trim();
        if trimmed.is_empty() {
            continue;
        }

        let normalized = normalize_text_for_match(trimmed);
        if normalized.is_empty() || !seen.insert(normalized.clone()) {
            continue;
        }

        let tokens = significant_constraint_tokens(trimmed);
        let covered = haystacks.iter().any(|haystack| {
            if haystack.contains(&normalized) {
                return true;
            }
            !tokens.is_empty() && tokens.iter().all(|token| haystack.contains(token))
        });

        if !covered {
            missing.push(trimmed.to_string());
        }
    }

    missing
}

fn baseline_requirements_from_context(
    codebase_insight: Option<&CodebaseInsight>,
    now: chrono::DateTime<Utc>,
) -> Vec<RequirementItem> {
    let mut requirements = vec![
        RequirementItem {
            id: String::new(),
            title: "Define MVP user journey".to_string(),
            description:
                "Specify the first complete user journey from entry to successful outcome."
                    .to_string(),
            body: None,
            legacy_id: None,
            category: None,
            requirement_type: None,
            acceptance_criteria: vec![
                "Journey includes entry point, key interaction steps, and success confirmation."
                    .to_string(),
                "Failure and retry behaviors are documented.".to_string(),
            ],
            priority: RequirementPriority::Must,
            status: RequirementStatus::Draft,
            source: "baseline".to_string(),
            tags: Vec::new(),
            links: RequirementLinks::default(),
            comments: Vec::new(),
            relative_path: None,
            linked_task_ids: Vec::new(),
            created_at: now,
            updated_at: now,
        },
        RequirementItem {
            id: String::new(),
            title: "Establish quality and observability guardrails".to_string(),
            description: "Define test, logging, and monitoring expectations before implementation."
                .to_string(),
            body: None,
            legacy_id: None,
            category: None,
            requirement_type: None,
            acceptance_criteria: vec![
                "Each critical workflow has pass/fail validation criteria.".to_string(),
                "Runtime logs and failure surfaces are explicitly defined.".to_string(),
            ],
            priority: RequirementPriority::Should,
            status: RequirementStatus::Draft,
            source: "baseline".to_string(),
            tags: Vec::new(),
            links: RequirementLinks::default(),
            comments: Vec::new(),
            relative_path: None,
            linked_task_ids: Vec::new(),
            created_at: now,
            updated_at: now,
        },
    ];

    if let Some(insight) = codebase_insight {
        let stacks = insight.detected_stacks.join(", ");
        if !stacks.is_empty() {
            requirements.push(RequirementItem {
                id: String::new(),
                title: "Align implementation with existing stack".to_string(),
                description: format!(
                    "Ensure new scope integrates with current stack: {}.",
                    stacks
                ),
                body: None,
                legacy_id: None,
                category: None,
                requirement_type: None,
                acceptance_criteria: vec![
                    "Requirements reference concrete integration points in the existing codebase."
                        .to_string(),
                    "Compatibility constraints are documented before execution.".to_string(),
                ],
                priority: RequirementPriority::Should,
                status: RequirementStatus::Draft,
                source: "codebase".to_string(),
                tags: Vec::new(),
                links: RequirementLinks::default(),
                comments: Vec::new(),
                relative_path: None,
                linked_task_ids: Vec::new(),
                created_at: now,
                updated_at: now,
            });
        }
    }

    requirements
}

fn dedupe_requirements(requirements: Vec<RequirementItem>) -> Vec<RequirementItem> {
    let mut seen = std::collections::HashSet::new();
    requirements
        .into_iter()
        .filter(|requirement| {
            let key = requirement.title.trim().to_ascii_lowercase();
            !key.is_empty() && seen.insert(key)
        })
        .collect()
}

pub(crate) fn build_requirement_candidates(
    vision: &VisionDocument,
    codebase_insight: Option<&CodebaseInsight>,
    max_requirements: usize,
) -> Vec<RequirementItem> {
    let now = Utc::now();
    let assessment = effective_complexity_assessment(vision);
    let mut candidates = Vec::new();

    if !vision.goals.is_empty() {
        for (index, goal) in vision.goals.iter().enumerate() {
            candidates.push(requirement_from_goal(
                goal,
                requirement_priority_for_index(index),
                "vision",
                now,
            ));
        }
    }

    candidates.extend(requirements_from_constraints(vision, now));
    if candidates.is_empty() {
        candidates.extend(baseline_requirements_from_context(codebase_insight, now));
    }
    let mut deduped = dedupe_requirements(candidates);

    let requirement_range = assessment.recommended_requirement_range;
    let target_max = if max_requirements > 0 {
        max_requirements
    } else {
        requirement_range.max
    };
    let target_min = if max_requirements > 0 {
        0
    } else {
        requirement_range.min
    };

    if target_max > 0 && deduped.len() > target_max {
        let mut pinned = Vec::new();
        let mut optional = Vec::new();
        for requirement in deduped {
            if requirement.source.eq_ignore_ascii_case("vision-constraint") {
                pinned.push(requirement);
            } else {
                optional.push(requirement);
            }
        }

        let keep_optional = target_max.saturating_sub(pinned.len());
        optional.truncate(keep_optional);
        deduped = optional;
        deduped.extend(pinned);
    }

    if deduped.len() < target_min {
        let mut extras = baseline_requirements_from_context(codebase_insight, now);
        extras = dedupe_requirements(extras);
        for extra in extras {
            if deduped
                .iter()
                .any(|existing| existing.title.eq_ignore_ascii_case(&extra.title))
            {
                continue;
            }
            deduped.push(extra);
            if deduped.len() >= target_min {
                break;
            }
        }
    }

    dedupe_requirements(deduped)
}

pub(crate) fn requirement_matches_id_filter(
    requirement: &RequirementItem,
    filter_ids: &[String],
) -> bool {
    if filter_ids.is_empty() {
        return true;
    }
    filter_ids
        .iter()
        .any(|id| id.eq_ignore_ascii_case(requirement.id.as_str()))
}
