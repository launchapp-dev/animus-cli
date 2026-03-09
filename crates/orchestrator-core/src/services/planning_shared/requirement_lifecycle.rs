use super::*;
use crate::state_machines::{
    builtin_compiled_state_machines, CompiledRequirementLifecycleMachine, CompiledStateMachines,
    RequirementLifecycleEvent,
};

fn add_requirement_comment(requirement: &mut RequirementItem, phase: &str, content: String) {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return;
    }

    let already_recorded = requirement.comments.iter().rev().take(20).any(|comment| {
        comment
            .phase
            .as_deref()
            .is_some_and(|value| value.eq_ignore_ascii_case(phase))
            && comment.content.trim().eq_ignore_ascii_case(trimmed)
    });
    if already_recorded {
        return;
    }

    requirement.comments.push(crate::types::RequirementComment {
        author: "ao-requirements-state-machine".to_string(),
        content: trimmed.to_string(),
        timestamp: Utc::now(),
        phase: Some(phase.to_string()),
    });
}

fn ensure_requirement_tag(requirement: &mut RequirementItem, tag: &str) {
    if requirement
        .tags
        .iter()
        .any(|existing| existing.eq_ignore_ascii_case(tag))
    {
        return;
    }
    requirement.tags.push(tag.to_string());
}

fn ensure_acceptance_criterion_contains(
    requirement: &mut RequirementItem,
    token: &str,
    text: &str,
) {
    let normalized_token = token.trim().to_ascii_lowercase();
    let exists = requirement.acceptance_criteria.iter().any(|criterion| {
        criterion
            .to_ascii_lowercase()
            .contains(normalized_token.as_str())
    });
    if !exists {
        requirement.acceptance_criteria.push(text.to_string());
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

fn significant_vision_tokens(value: &str) -> Vec<String> {
    const STOP_WORDS: &[&str] = &[
        "the", "and", "for", "with", "that", "must", "should", "from", "this", "have", "has",
        "are", "our", "your", "their", "into", "using", "include", "supports", "support",
    ];

    normalize_text_for_match(value)
        .split_whitespace()
        .filter(|token| token.len() > 3 && !STOP_WORDS.contains(token))
        .map(|token| token.to_string())
        .collect()
}

fn requirement_review_haystack(requirement: &RequirementItem) -> String {
    let mut chunks = vec![requirement.title.clone(), requirement.description.clone()];
    chunks.extend(requirement.acceptance_criteria.clone());
    chunks.extend(requirement.tags.clone());
    normalize_text_for_match(&chunks.join(" "))
}

fn collect_po_review_issues(
    requirement: &RequirementItem,
    vision: Option<&VisionDocument>,
) -> Vec<String> {
    let mut issues = Vec::new();
    if requirement.description.trim().is_empty() {
        issues.push("description is empty".to_string());
    }
    if requirement.acceptance_criteria.len() < 3 {
        issues.push("fewer than 3 acceptance criteria".to_string());
    }
    if requirement.tags.is_empty() {
        issues.push("tags are missing for routing".to_string());
    }

    if let Some(vision) = vision {
        let haystack = requirement_review_haystack(requirement);
        let goal_tokens = vision
            .goals
            .iter()
            .flat_map(|goal| significant_vision_tokens(goal))
            .collect::<Vec<_>>();
        let user_tokens = vision
            .target_users
            .iter()
            .flat_map(|user| significant_vision_tokens(user))
            .collect::<Vec<_>>();
        let has_goal_alignment =
            !goal_tokens.is_empty() && goal_tokens.iter().any(|token| haystack.contains(token));
        let has_user_alignment =
            !user_tokens.is_empty() && user_tokens.iter().any(|token| haystack.contains(token));
        if !(has_goal_alignment || has_user_alignment) {
            issues.push("missing explicit alignment to vision goals or target users".to_string());
        }
    }

    issues
}

fn collect_em_review_issues(requirement: &RequirementItem) -> Vec<String> {
    let mut issues = Vec::new();
    if !requirement.acceptance_criteria.iter().any(|criterion| {
        criterion
            .to_ascii_lowercase()
            .contains("automated test coverage")
    }) {
        issues.push("missing automated test coverage criterion".to_string());
    }
    if !requirement.acceptance_criteria.iter().any(|criterion| {
        let normalized = criterion.to_ascii_lowercase();
        normalized.contains("error")
            || normalized.contains("failure")
            || normalized.contains("retry")
            || normalized.contains("fallback")
            || normalized.contains("observability")
            || normalized.contains("monitor")
            || normalized.contains("logging")
    }) {
        issues.push("missing reliability/observability criterion".to_string());
    }
    issues
}

fn apply_requirement_rework(
    requirement: &mut RequirementItem,
    vision: Option<&VisionDocument>,
    issues: &[String],
    reviewer_phase: &str,
) {
    requirement.status = RequirementStatus::NeedsRework;
    add_requirement_comment(
        requirement,
        "rework",
        format!(
            "Rework requested by {}: {}",
            reviewer_phase,
            issues.join("; ")
        ),
    );

    if super::requirement_needs_research(requirement) {
        ensure_requirement_tag(requirement, "needs-research");
        ensure_acceptance_criterion_contains(
            requirement,
            "research findings documented",
            "Research findings documented with sources and confidence notes.",
        );
        ensure_acceptance_criterion_contains(
            requirement,
            "research outputs are reflected",
            "Requirement validation confirms research outputs are reflected in acceptance criteria.",
        );
        add_requirement_comment(
            requirement,
            "research",
            "Research sub-phase required before implementation; requirement enriched with research evidence criteria."
                .to_string(),
        );
    }

    ensure_acceptance_criterion_contains(
        requirement,
        "automated test coverage",
        "Includes automated test coverage for core acceptance criteria.",
    );
    ensure_acceptance_criterion_contains(
        requirement,
        "error handling",
        "Error handling, retries, and observability are validated for this requirement.",
    );
    ensure_acceptance_criterion_contains(
        requirement,
        "po and em review",
        "PO and EM review confirms requirement scope, tradeoffs, and implementation readiness.",
    );

    if let Some(vision) = vision {
        let alignment_target = format!(
            "Primary alignment: user '{}' and goal '{}'.",
            vision
                .target_users
                .first()
                .map(|value| value.trim())
                .filter(|value| !value.is_empty())
                .unwrap_or("core user segment"),
            vision
                .goals
                .first()
                .map(|value| value.trim())
                .filter(|value| !value.is_empty())
                .unwrap_or("core delivery outcome"),
        );
        let has_alignment_section = requirement
            .description
            .to_ascii_lowercase()
            .contains("vision alignment");
        if has_alignment_section {
            if !requirement
                .description
                .to_ascii_lowercase()
                .contains(&alignment_target.to_ascii_lowercase())
            {
                requirement.description = format!(
                    "{}\n- {}",
                    requirement.description.trim_end(),
                    alignment_target
                );
            }
        } else {
            requirement.description = format!(
                "{}\n\n## Vision Alignment\n- {}",
                requirement.description.trim_end(),
                alignment_target
            )
            .trim()
            .to_string();
        }
    }

    requirement.updated_at = Utc::now();
}

pub(super) fn run_requirement_lifecycle_state_machine(
    requirement: &mut RequirementItem,
    vision: Option<&VisionDocument>,
    state_machines: Option<&CompiledStateMachines>,
) -> Result<()> {
    let effective_state_machines = state_machines
        .cloned()
        .unwrap_or_else(builtin_compiled_state_machines);
    let machine = effective_state_machines.requirements_lifecycle;

    if machine.is_terminal(requirement.status) {
        return Ok(());
    }

    let mut rework_rounds = 0usize;
    let max_rework_rounds = machine.max_rework_rounds();

    loop {
        let refined = machine.apply(
            requirement.status,
            RequirementLifecycleEvent::Refine,
            |guard| requirement_guard_result(guard, rework_rounds, max_rework_rounds),
        );
        requirement.status = refined.to;
        apply_requirement_lifecycle_actions(
            requirement,
            vision,
            &machine,
            refined.from,
            refined.event,
            refined.to,
            &[],
            "refine",
        );
        if requirement.status != RequirementStatus::Refined {
            return Err(anyhow!(
                "requirement {} lifecycle stalled before PO review (state: {:?})",
                requirement.id,
                requirement.status
            ));
        }

        let enter_po = machine.apply(
            requirement.status,
            RequirementLifecycleEvent::PoPass,
            |guard| requirement_guard_result(guard, rework_rounds, max_rework_rounds),
        );
        requirement.status = enter_po.to;
        if requirement.status != RequirementStatus::PoReview {
            return Err(anyhow!(
                "requirement {} failed to enter PO review (state: {:?})",
                requirement.id,
                requirement.status
            ));
        }

        let po_issues = collect_po_review_issues(requirement, vision);
        if !po_issues.is_empty() {
            let po_fail = machine.apply(
                requirement.status,
                RequirementLifecycleEvent::PoFail,
                |guard| requirement_guard_result(guard, rework_rounds, max_rework_rounds),
            );

            if !po_fail.matched {
                return Err(anyhow!(
                    "requirement {} exceeded PO rework budget: {}",
                    requirement.id,
                    po_issues.join("; ")
                ));
            }

            requirement.status = po_fail.to;
            apply_requirement_lifecycle_actions(
                requirement,
                vision,
                &machine,
                po_fail.from,
                po_fail.event,
                po_fail.to,
                &po_issues,
                "po-review",
            );
            rework_rounds = rework_rounds.saturating_add(1);
            continue;
        }

        let po_pass = machine.apply(
            requirement.status,
            RequirementLifecycleEvent::PoPass,
            |guard| requirement_guard_result(guard, rework_rounds, max_rework_rounds),
        );
        if !po_pass.matched {
            return Err(anyhow!(
                "requirement {} lifecycle stalled after PO review (state: {:?})",
                requirement.id,
                requirement.status
            ));
        }

        requirement.status = po_pass.to;
        apply_requirement_lifecycle_actions(
            requirement,
            vision,
            &machine,
            po_pass.from,
            po_pass.event,
            po_pass.to,
            &[],
            "po-review",
        );
        if requirement.status != RequirementStatus::EmReview {
            return Err(anyhow!(
                "requirement {} failed to enter EM review (state: {:?})",
                requirement.id,
                requirement.status
            ));
        }

        let em_issues = collect_em_review_issues(requirement);
        if !em_issues.is_empty() {
            let em_fail = machine.apply(
                requirement.status,
                RequirementLifecycleEvent::EmFail,
                |guard| requirement_guard_result(guard, rework_rounds, max_rework_rounds),
            );

            if !em_fail.matched {
                return Err(anyhow!(
                    "requirement {} exceeded EM rework budget: {}",
                    requirement.id,
                    em_issues.join("; ")
                ));
            }

            requirement.status = em_fail.to;
            apply_requirement_lifecycle_actions(
                requirement,
                vision,
                &machine,
                em_fail.from,
                em_fail.event,
                em_fail.to,
                &em_issues,
                "em-review",
            );
            rework_rounds = rework_rounds.saturating_add(1);
            continue;
        }

        let em_pass = machine.apply(
            requirement.status,
            RequirementLifecycleEvent::EmPass,
            |guard| requirement_guard_result(guard, rework_rounds, max_rework_rounds),
        );
        if !em_pass.matched {
            return Err(anyhow!(
                "requirement {} lifecycle stalled after EM review (state: {:?})",
                requirement.id,
                requirement.status
            ));
        }

        requirement.status = em_pass.to;
        apply_requirement_lifecycle_actions(
            requirement,
            vision,
            &machine,
            em_pass.from,
            em_pass.event,
            em_pass.to,
            &[],
            "em-review",
        );
        requirement.updated_at = Utc::now();
        add_requirement_comment(
            requirement,
            "approved",
            requirement_comment_template(
                &machine,
                "approved",
                "Requirement approved for task materialization and workflow execution.",
            ),
        );
        return Ok(());
    }
}

fn requirement_guard_result(
    guard_id: &str,
    rework_rounds: usize,
    max_rework_rounds: usize,
) -> bool {
    match guard_id {
        "rework_budget_available" => rework_rounds < max_rework_rounds,
        _ => false,
    }
}

fn requirement_comment_template(
    machine: &CompiledRequirementLifecycleMachine,
    key: &str,
    fallback: &str,
) -> String {
    machine
        .comment_template(key)
        .unwrap_or(fallback)
        .to_string()
}

fn apply_requirement_lifecycle_actions(
    requirement: &mut RequirementItem,
    vision: Option<&VisionDocument>,
    machine: &CompiledRequirementLifecycleMachine,
    from: RequirementStatus,
    event: RequirementLifecycleEvent,
    to: RequirementStatus,
    issues: &[String],
    reviewer_phase: &str,
) {
    for action in machine.actions_for(from, event, to) {
        match action {
            "add_refine_comment" => {
                add_requirement_comment(
                    requirement,
                    "refine",
                    requirement_comment_template(
                        machine,
                        "refine",
                        "Requirement refined and prepared for PO/EM review pipeline.",
                    ),
                );
            }
            "add_po_approval_comment" => {
                add_requirement_comment(
                    requirement,
                    "po-review",
                    requirement_comment_template(
                        machine,
                        "po_approved",
                        "PO review approved requirement scope and outcome alignment.",
                    ),
                );
            }
            "add_em_approval_comment" => {
                add_requirement_comment(
                    requirement,
                    "em-review",
                    requirement_comment_template(
                        machine,
                        "em_approved",
                        "EM review approved implementation readiness and quality gates.",
                    ),
                );
            }
            "add_rework_comment" => {
                apply_requirement_rework(requirement, vision, issues, reviewer_phase);
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{RequirementLinks, RequirementPriority};

    fn sample_requirement(
        id: &str,
        title: &str,
        description: &str,
        acceptance_criteria: Vec<&str>,
        tags: Vec<&str>,
    ) -> RequirementItem {
        let now = Utc::now();
        RequirementItem {
            id: id.to_string(),
            title: title.to_string(),
            description: description.to_string(),
            body: None,
            legacy_id: None,
            category: None,
            requirement_type: None,
            acceptance_criteria: acceptance_criteria
                .into_iter()
                .map(ToString::to_string)
                .collect(),
            priority: RequirementPriority::Should,
            status: RequirementStatus::Draft,
            source: "test".to_string(),
            tags: tags.into_iter().map(ToString::to_string).collect(),
            labels: Vec::new(),
            area: None,
            external_ref: None,
            linked_epic_ids: Vec::new(),
            links: RequirementLinks::default(),
            comments: Vec::new(),
            relative_path: None,
            linked_task_ids: Vec::new(),
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn requirement_lifecycle_happy_path_approves() {
        let mut requirement = sample_requirement(
            "REQ-1",
            "User signup flow",
            "Implement user signup with verification and retries.",
            vec![
                "Includes automated test coverage for core acceptance criteria.",
                "Error handling and retry behavior is defined.",
                "Observability and monitoring are included for signup failures.",
            ],
            vec!["auth", "backend"],
        );

        run_requirement_lifecycle_state_machine(&mut requirement, None, None)
            .expect("lifecycle should approve");

        assert_eq!(requirement.status, RequirementStatus::Approved);
        assert!(requirement
            .comments
            .iter()
            .any(|comment| comment.phase.as_deref() == Some("refine")));
        assert!(requirement
            .comments
            .iter()
            .any(|comment| comment.phase.as_deref() == Some("po-review")));
        assert!(requirement
            .comments
            .iter()
            .any(|comment| comment.phase.as_deref() == Some("em-review")));
        assert!(requirement
            .comments
            .iter()
            .any(|comment| comment.phase.as_deref() == Some("approved")));
    }

    #[test]
    fn requirement_lifecycle_can_rework_then_approve() {
        let mut requirement = sample_requirement(
            "REQ-2",
            "Route tasks intelligently",
            "Route tasks according to complexity and risk.",
            vec!["Includes automated test coverage for core acceptance criteria."],
            vec![],
        );

        run_requirement_lifecycle_state_machine(&mut requirement, None, None)
            .expect("rework path should eventually approve");

        assert_eq!(requirement.status, RequirementStatus::Approved);
        assert!(requirement
            .comments
            .iter()
            .any(|comment| comment.phase.as_deref() == Some("rework")));
    }

    #[test]
    fn requirement_lifecycle_respects_rework_budget() {
        let mut requirement = sample_requirement(
            "REQ-3",
            "Thin placeholder requirement",
            "",
            vec!["only one criterion"],
            vec![],
        );

        let error = run_requirement_lifecycle_state_machine(&mut requirement, None, None)
            .expect_err("missing description should exhaust PO rework budget");

        assert!(error.to_string().contains("exceeded PO rework budget"));
        assert_eq!(requirement.status, RequirementStatus::PoReview);
    }
}
