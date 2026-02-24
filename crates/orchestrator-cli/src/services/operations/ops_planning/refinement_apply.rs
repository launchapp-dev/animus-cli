use orchestrator_core::{ComplexityAssessment, VisionDocument, VisionDraftInput};

use super::types::{VisionRefinementChanges, VisionRefinementProposal};

fn parse_project_name_from_markdown(markdown: &str) -> Option<String> {
    markdown
        .lines()
        .find_map(|line| line.trim().strip_prefix("- Name:"))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn normalize_non_empty(values: Vec<String>) -> Vec<String> {
    values
        .into_iter()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .collect()
}

fn merge_unique(base: &mut Vec<String>, additions: Vec<String>) -> usize {
    let mut seen = base
        .iter()
        .map(|value| value.trim().to_ascii_lowercase())
        .collect::<std::collections::BTreeSet<_>>();
    let mut added = 0usize;
    for value in normalize_non_empty(additions) {
        let key = value.to_ascii_lowercase();
        if seen.insert(key) {
            base.push(value);
            added = added.saturating_add(1);
        }
    }
    added
}

pub(super) fn apply_vision_refinement(
    current: &VisionDocument,
    proposal: VisionRefinementProposal,
    preserve_core: bool,
    complexity_assessment: ComplexityAssessment,
) -> (VisionDraftInput, VisionRefinementChanges) {
    let mut problem_statement = current.problem_statement.clone();
    let mut problem_statement_enriched = false;
    if let Some(refinement) = proposal
        .problem_statement_refinement
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        if preserve_core {
            if !problem_statement
                .to_ascii_lowercase()
                .contains(&refinement.to_ascii_lowercase())
            {
                problem_statement = format!(
                    "{}\n\nRefinement context: {}",
                    current.problem_statement.trim(),
                    refinement
                );
                problem_statement_enriched = true;
            }
        } else {
            problem_statement = refinement.to_string();
            problem_statement_enriched = true;
        }
    }

    let mut target_users = current.target_users.clone();
    let target_users_added = merge_unique(&mut target_users, proposal.target_users_additions);

    let mut goals = current.goals.clone();
    let goals_added = merge_unique(&mut goals, proposal.goals_additions);

    let mut constraints = current.constraints.clone();
    let constraints_added = merge_unique(&mut constraints, proposal.constraints_additions);

    let mut value_proposition = current.value_proposition.clone();
    let mut value_proposition_changed = false;
    if let Some(refinement) = proposal
        .value_proposition_refinement
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        if preserve_core {
            let merged = match &value_proposition {
                Some(current_value)
                    if current_value
                        .to_ascii_lowercase()
                        .contains(&refinement.to_ascii_lowercase()) =>
                {
                    current_value.clone()
                }
                Some(current_value) => format!("{} {}", current_value.trim(), refinement),
                None => refinement.to_string(),
            };
            value_proposition_changed = value_proposition.as_deref() != Some(merged.as_str());
            value_proposition = Some(merged);
        } else {
            value_proposition_changed = value_proposition.as_deref() != Some(refinement);
            value_proposition = Some(refinement.to_string());
        }
    }

    (
        VisionDraftInput {
            project_name: parse_project_name_from_markdown(&current.markdown),
            problem_statement,
            target_users,
            goals,
            constraints,
            value_proposition,
            complexity_assessment: Some(complexity_assessment),
        },
        VisionRefinementChanges {
            target_users_added,
            goals_added,
            constraints_added,
            problem_statement_enriched,
            value_proposition_changed,
        },
    )
}
