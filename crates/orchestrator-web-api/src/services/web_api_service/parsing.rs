use anyhow::anyhow;
use orchestrator_core::{
    DependencyType, HandoffTargetRole, Priority, ProjectType, RequirementFilter,
    RequirementPriority, RequirementStatus, RequirementType, RiskLevel, TaskFilter, TaskStatus,
    TaskType, VisionDocument, VisionDraftInput,
};
use serde::de::DeserializeOwned;
use serde_json::{json, Value};

use crate::models::WebApiError;

pub(super) fn parse_json_body<T: DeserializeOwned>(body: Value) -> Result<T, WebApiError> {
    serde_json::from_value(body).map_err(|error| {
        WebApiError::new("invalid_input", format!("invalid JSON body: {error}"), 2)
    })
}

pub(super) fn enum_as_string<T: serde::Serialize>(value: &T) -> Result<String, WebApiError> {
    let serialized = serde_json::to_value(value)
        .map_err(|error| WebApiError::from(anyhow!("failed to serialize enum: {error}")))?;
    Ok(serialized
        .as_str()
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| "unknown".to_string()))
}

pub(super) fn default_true_flag() -> bool {
    true
}

pub(super) fn normalize_optional_string(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let trimmed = value.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_string())
    })
}

pub(super) fn normalize_string_list(values: Vec<String>) -> Vec<String> {
    let mut normalized = Vec::new();

    for value in values {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            continue;
        }
        if !normalized.iter().any(|existing| existing == trimmed) {
            normalized.push(trimmed.to_string());
        }
    }

    normalized
}

pub(super) fn extract_project_name_from_markdown(markdown: &str) -> Option<String> {
    for line in markdown.lines() {
        let trimmed = line.trim();
        if let Some(name) = trimmed.strip_prefix("- Name:") {
            let name = name.trim();
            if !name.is_empty() {
                return Some(name.to_string());
            }
        }
    }

    None
}

pub(super) fn normalize_vision_input(input: &mut VisionDraftInput) {
    input.project_name = normalize_optional_string(input.project_name.take());
    input.problem_statement = input.problem_statement.trim().to_string();
    input.target_users = normalize_string_list(std::mem::take(&mut input.target_users));
    input.goals = normalize_string_list(std::mem::take(&mut input.goals));
    input.constraints = normalize_string_list(std::mem::take(&mut input.constraints));
    input.value_proposition = normalize_optional_string(input.value_proposition.take());
}

pub(super) fn refine_vision_heuristically(
    current: &VisionDocument,
    focus: Option<&str>,
) -> (VisionDraftInput, Value, String) {
    let mut goals = current.goals.clone();
    let mut constraints = current.constraints.clone();
    let mut goals_added = Vec::new();
    let mut constraints_added = Vec::new();
    let mut notes = Vec::new();

    let goals_haystack = goals.join(" ").to_ascii_lowercase();
    if !goals_haystack.contains("success metric") && !goals_haystack.contains("kpi") {
        let addition = "Define measurable success metrics (activation, retention, and business impact) with explicit go/no-go thresholds.".to_string();
        goals.push(addition.clone());
        goals_added.push(addition);
        notes.push("added measurable success metric guidance".to_string());
    }

    let constraints_haystack = constraints.join(" ").to_ascii_lowercase();
    if !constraints_haystack.contains("traceable")
        && !constraints_haystack.contains("machine-readable")
    {
        let addition =
            "Requirements, tasks, and workflow artifacts must remain traceable and machine-readable."
                .to_string();
        constraints.push(addition.clone());
        constraints_added.push(addition);
        notes.push("added traceability constraint".to_string());
    }

    let mut normalized_focus = None;
    if let Some(focus) = focus {
        let focus = focus.trim();
        if !focus.is_empty() {
            normalized_focus = Some(focus.to_string());
            let focus_lower = focus.to_ascii_lowercase();
            let already_present = constraints
                .iter()
                .any(|constraint| constraint.to_ascii_lowercase().contains(&focus_lower));

            if !already_present {
                let addition = format!(
                    "Refinement focus must be reflected in requirement acceptance criteria: {focus}."
                );
                constraints.push(addition.clone());
                constraints_added.push(addition);
            }
            notes.push("captured requested refinement focus".to_string());
        }
    }

    let project_name = extract_project_name_from_markdown(&current.markdown);
    let value_proposition = match current.value_proposition.clone() {
        Some(value) => Some(value),
        None => {
            notes.push("filled missing value proposition".to_string());
            Some(
                "Deliver measurable value for target users while preserving deterministic execution quality."
                    .to_string(),
            )
        }
    };

    let mut refined_input = VisionDraftInput {
        project_name,
        problem_statement: current.problem_statement.clone(),
        target_users: current.target_users.clone(),
        goals,
        constraints,
        value_proposition,
        complexity_assessment: current.complexity_assessment.clone(),
    };
    normalize_vision_input(&mut refined_input);

    let rationale = if notes.is_empty() {
        "No heuristic deltas were required; retained current vision content.".to_string()
    } else {
        format!(
            "Heuristic refinement {}.",
            notes
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>()
                .join(", ")
        )
    };

    (
        refined_input,
        json!({
            "goals_added": goals_added,
            "constraints_added": constraints_added,
            "focus": normalized_focus,
        }),
        rationale,
    )
}

pub(super) fn build_task_filter(
    task_type: Option<String>,
    status: Option<String>,
    priority: Option<String>,
    risk: Option<String>,
    assignee_type: Option<String>,
    tags: Vec<String>,
    labels: Vec<String>,
    linked_requirements: Vec<String>,
    requirement_search: Option<String>,
    epic_id: Option<String>,
    parent_task_id: Option<String>,
    has_parent: Option<bool>,
    has_children: Option<bool>,
    area: Option<String>,
    related_task_id: Option<String>,
    relation_type: Option<String>,
    linked_architecture_entity: Option<String>,
    search: Option<String>,
) -> Result<TaskFilter, WebApiError> {
    let normalized_tags = normalize_string_list(tags);
    let normalized_labels = normalize_string_list(labels);
    let normalized_linked_requirements = normalize_string_list(linked_requirements);

    Ok(TaskFilter {
        task_type: parse_task_type_opt(task_type.as_deref())?,
        status: status
            .as_deref()
            .map(parse_task_status)
            .transpose()
            .map_err(WebApiError::from)?,
        priority: parse_priority_opt(priority.as_deref())?,
        risk: parse_risk_opt(risk.as_deref())?,
        assignee_type: parse_assignee_type_opt(assignee_type)?,
        tags: if normalized_tags.is_empty() {
            None
        } else {
            Some(normalized_tags)
        },
        labels: if normalized_labels.is_empty() {
            None
        } else {
            Some(normalized_labels)
        },
        area: normalize_optional_string(area),
        linked_requirements: normalized_linked_requirements,
        requirement_search_text: normalize_optional_string(requirement_search),
        epic_id: normalize_optional_string(epic_id),
        parent_task_id: normalize_optional_string(parent_task_id),
        has_parent,
        has_children,
        related_task_id: normalize_optional_string(related_task_id),
        relation_type: relation_type
            .as_deref()
            .map(parse_dependency_type)
            .transpose()?,
        linked_architecture_entity: normalize_optional_string(linked_architecture_entity),
        search_text: normalize_optional_string(search),
    })
}

pub(super) fn build_requirement_filter(
    status: Option<String>,
    priority: Option<String>,
    requirement_type: Option<String>,
    category: Option<String>,
    tags: Vec<String>,
    labels: Vec<String>,
    area: Option<String>,
    source: Option<String>,
    epic_id: Option<String>,
    linked_task_id: Option<String>,
    search: Option<String>,
) -> Result<RequirementFilter, WebApiError> {
    let normalized_tags = normalize_string_list(tags);
    let normalized_labels = normalize_string_list(labels);

    Ok(RequirementFilter {
        status: status
            .as_deref()
            .map(parse_requirement_status)
            .transpose()?,
        priority: parse_requirement_priority_opt(priority.as_deref())?,
        requirement_type: parse_requirement_type_opt(requirement_type.as_deref())?,
        category: normalize_optional_string(category),
        tags: if normalized_tags.is_empty() {
            None
        } else {
            Some(normalized_tags)
        },
        labels: if normalized_labels.is_empty() {
            None
        } else {
            Some(normalized_labels)
        },
        area: normalize_optional_string(area),
        source: normalize_optional_string(source),
        linked_epic_id: normalize_optional_string(epic_id),
        linked_task_id: normalize_optional_string(linked_task_id),
        search_text: normalize_optional_string(search),
    })
}

pub(super) fn is_empty_task_filter(filter: &TaskFilter) -> bool {
    filter.task_type.is_none()
        && filter.status.is_none()
        && filter.priority.is_none()
        && filter.risk.is_none()
        && filter.assignee_type.is_none()
        && filter.tags.is_none()
        && filter.labels.is_none()
        && filter.area.is_none()
        && filter.linked_requirements.is_empty()
        && filter.requirement_search_text.is_none()
        && filter.epic_id.is_none()
        && filter.parent_task_id.is_none()
        && filter.has_parent.is_none()
        && filter.has_children.is_none()
        && filter.related_task_id.is_none()
        && filter.relation_type.is_none()
        && filter.linked_architecture_entity.is_none()
        && filter.search_text.is_none()
}

pub(super) fn parse_task_status(value: &str) -> Result<TaskStatus, WebApiError> {
    value
        .parse()
        .map_err(|_| WebApiError::new("invalid_input", format!("invalid status: {value}"), 2))
}

pub(super) fn parse_requirement_priority(value: &str) -> Result<RequirementPriority, WebApiError> {
    let parsed = match value.trim().to_ascii_lowercase().as_str() {
        "must" => RequirementPriority::Must,
        "should" => RequirementPriority::Should,
        "could" => RequirementPriority::Could,
        "wont" | "won't" => RequirementPriority::Wont,
        _ => {
            return Err(WebApiError::new(
                "invalid_input",
                format!("invalid requirement priority: {value}"),
                2,
            ))
        }
    };

    Ok(parsed)
}

pub(super) fn parse_requirement_priority_opt(
    value: Option<&str>,
) -> Result<Option<RequirementPriority>, WebApiError> {
    let Some(value) = value else {
        return Ok(None);
    };

    Ok(Some(parse_requirement_priority(value)?))
}

pub(super) fn parse_requirement_status(value: &str) -> Result<RequirementStatus, WebApiError> {
    value.parse().map_err(|_| {
        WebApiError::new(
            "invalid_input",
            format!("invalid requirement status: {value}"),
            2,
        )
    })
}

pub(super) fn parse_requirement_status_opt(
    value: Option<&str>,
) -> Result<Option<RequirementStatus>, WebApiError> {
    let Some(value) = value else {
        return Ok(None);
    };

    Ok(Some(parse_requirement_status(value)?))
}

pub(super) fn parse_requirement_type_opt(
    value: Option<&str>,
) -> Result<Option<RequirementType>, WebApiError> {
    let Some(value) = value else {
        return Ok(None);
    };

    let normalized = value.trim().to_ascii_lowercase().replace('_', "-");
    let parsed = match normalized.as_str() {
        "product" => RequirementType::Product,
        "functional" => RequirementType::Functional,
        "non-functional" => RequirementType::NonFunctional,
        "technical" => RequirementType::Technical,
        "other" => RequirementType::Other,
        _ => {
            return Err(WebApiError::new(
                "invalid_input",
                format!("invalid requirement_type: {value}"),
                2,
            ))
        }
    };

    Ok(Some(parsed))
}

pub(super) fn parse_handoff_target_role(value: &str) -> Result<HandoffTargetRole, WebApiError> {
    HandoffTargetRole::try_from(value)
        .map_err(|error| WebApiError::new("invalid_input", format!("{error}"), 2))
}

pub(super) fn parse_task_type_opt(value: Option<&str>) -> Result<Option<TaskType>, WebApiError> {
    let Some(value) = value else {
        return Ok(None);
    };

    let normalized = normalize_enum_key(value);
    let parsed = match normalized.as_str() {
        "story" => TaskType::Feature,
        "feature" => TaskType::Feature,
        "bugfix" => TaskType::Bugfix,
        "hotfix" => TaskType::Hotfix,
        "refactor" => TaskType::Refactor,
        "docs" => TaskType::Docs,
        "test" => TaskType::Test,
        "chore" => TaskType::Chore,
        "experiment" => TaskType::Experiment,
        "spike" => TaskType::Spike,
        _ => {
            return Err(WebApiError::new(
                "invalid_input",
                format!("invalid task_type: {value}"),
                2,
            ))
        }
    };

    Ok(Some(parsed))
}

pub(super) fn parse_priority_opt(value: Option<&str>) -> Result<Option<Priority>, WebApiError> {
    let Some(value) = value else {
        return Ok(None);
    };

    let normalized = normalize_enum_key(value);
    let parsed = match normalized.as_str() {
        "critical" => Priority::Critical,
        "high" => Priority::High,
        "medium" => Priority::Medium,
        "low" => Priority::Low,
        _ => {
            return Err(WebApiError::new(
                "invalid_input",
                format!("invalid priority: {value}"),
                2,
            ))
        }
    };

    Ok(Some(parsed))
}

pub(super) fn parse_risk_opt(value: Option<&str>) -> Result<Option<RiskLevel>, WebApiError> {
    let Some(value) = value else {
        return Ok(None);
    };

    let normalized = normalize_enum_key(value);
    let parsed = match normalized.as_str() {
        "high" => RiskLevel::High,
        "medium" => RiskLevel::Medium,
        "low" => RiskLevel::Low,
        _ => {
            return Err(WebApiError::new(
                "invalid_input",
                format!("invalid risk: {value}"),
                2,
            ))
        }
    };

    Ok(Some(parsed))
}

pub(super) fn parse_dependency_type(value: &str) -> Result<DependencyType, WebApiError> {
    let normalized = normalize_enum_key(value);
    let parsed = match normalized.as_str() {
        "blocks-by" | "blocksby" => DependencyType::BlocksBy,
        "blocked-by" | "blockedby" => DependencyType::BlockedBy,
        "related-to" | "relatedto" => DependencyType::RelatedTo,
        "parent-child" | "parentchild" => DependencyType::ParentChild,
        "duplicate" => DependencyType::Duplicate,
        "caused-by" | "causedby" => DependencyType::CausedBy,
        "split-from" | "splitfrom" => DependencyType::SplitFrom,
        _ => {
            return Err(WebApiError::new(
                "invalid_input",
                format!("invalid dependency_type: {value}"),
                2,
            ))
        }
    };

    Ok(parsed)
}

pub(super) fn parse_project_type_opt(
    value: Option<&str>,
) -> Result<Option<ProjectType>, WebApiError> {
    let Some(value) = value else {
        return Ok(Some(ProjectType::Other));
    };

    let normalized = value.trim().to_ascii_lowercase();
    let parsed = match normalized.as_str() {
        "web-app" | "web_app" | "webapp" => ProjectType::WebApp,
        "mobile-app" | "mobile_app" | "mobileapp" => ProjectType::MobileApp,
        "desktop-app" | "desktop_app" | "desktopapp" => ProjectType::DesktopApp,
        "full-stack-platform"
        | "full_stack_platform"
        | "fullstackplatform"
        | "full-stack"
        | "full_stack"
        | "fullstack"
        | "saas" => ProjectType::FullStackPlatform,
        "library" => ProjectType::Library,
        "infrastructure" => ProjectType::Infrastructure,
        "other" | "greenfield" | "existing" => ProjectType::Other,
        _ => {
            return Err(WebApiError::new(
                "invalid_input",
                format!("invalid project_type: {}", value.trim()),
                2,
            ))
        }
    };

    Ok(Some(parsed))
}

fn parse_assignee_type_opt(value: Option<String>) -> Result<Option<String>, WebApiError> {
    let Some(value) = normalize_optional_string(value) else {
        return Ok(None);
    };

    let normalized = value.to_ascii_lowercase();
    if matches!(normalized.as_str(), "agent" | "human" | "unassigned") {
        return Ok(Some(normalized));
    }

    Err(WebApiError::new(
        "invalid_input",
        format!("invalid assignee_type: {value}"),
        2,
    ))
}

fn normalize_enum_key(value: &str) -> String {
    value.trim().to_ascii_lowercase().replace('_', "-")
}

#[cfg(test)]
mod tests {
    use super::*;
    use orchestrator_core::{
        DependencyType, Priority, ProjectType, RequirementStatus, RiskLevel, TaskStatus, TaskType,
    };

    #[test]
    fn parse_task_status_accepts_case_and_separator_variants() {
        assert!(matches!(
            parse_task_status(" In_Progress "),
            Ok(TaskStatus::InProgress)
        ));
        assert!(matches!(
            parse_task_status("ON-HOLD"),
            Ok(TaskStatus::OnHold)
        ));
        assert!(matches!(
            parse_task_status(" Todo "),
            Ok(TaskStatus::Backlog)
        ));
    }

    #[test]
    fn parse_task_type_priority_and_risk_are_trimmed_and_case_insensitive() {
        assert!(matches!(
            parse_task_type_opt(Some(" Feature ")),
            Ok(Some(TaskType::Feature))
        ));
        assert!(matches!(
            parse_task_type_opt(Some(" story ")),
            Ok(Some(TaskType::Feature))
        ));
        assert!(matches!(
            parse_task_type_opt(Some(" SPIKE ")),
            Ok(Some(TaskType::Spike))
        ));
        assert!(matches!(
            parse_priority_opt(Some(" HIGH ")),
            Ok(Some(Priority::High))
        ));
        assert!(matches!(
            parse_risk_opt(Some(" low ")),
            Ok(Some(RiskLevel::Low))
        ));
    }

    #[test]
    fn parse_dependency_type_accepts_mixed_case_aliases() {
        assert!(matches!(
            parse_dependency_type(" Blocks_By "),
            Ok(DependencyType::BlocksBy)
        ));
        assert!(matches!(
            parse_dependency_type("BLOCKEDBY"),
            Ok(DependencyType::BlockedBy)
        ));
        assert!(matches!(
            parse_dependency_type("related-to"),
            Ok(DependencyType::RelatedTo)
        ));
        assert!(matches!(
            parse_dependency_type("parent-child"),
            Ok(DependencyType::ParentChild)
        ));
        assert!(matches!(
            parse_dependency_type("split_from"),
            Ok(DependencyType::SplitFrom)
        ));
    }

    #[test]
    fn build_task_filter_normalizes_string_fields_and_tags() {
        let filter = build_task_filter(
            Some(" Feature ".to_string()),
            Some(" IN_PROGRESS ".to_string()),
            Some(" HIGH ".to_string()),
            Some(" low ".to_string()),
            Some(" Human ".to_string()),
            vec![
                "backend".to_string(),
                " backend ".to_string(),
                " api ".to_string(),
                "".to_string(),
            ],
            vec![],
            vec![" REQ-1 ".to_string(), " REQ-1 ".to_string()],
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            Some(" entity-1 ".to_string()),
            Some(" shipping blocker ".to_string()),
        )
        .expect("task filter should parse");

        assert!(matches!(filter.task_type, Some(TaskType::Feature)));
        assert!(matches!(filter.status, Some(TaskStatus::InProgress)));
        assert!(matches!(filter.priority, Some(Priority::High)));
        assert!(matches!(filter.risk, Some(RiskLevel::Low)));
        assert_eq!(filter.assignee_type.as_deref(), Some("human"));
        assert_eq!(
            filter.tags,
            Some(vec!["backend".to_string(), "api".to_string()])
        );
        assert_eq!(filter.linked_requirements, vec!["REQ-1".to_string()]);
        assert_eq!(
            filter.linked_architecture_entity.as_deref(),
            Some("entity-1")
        );
        assert_eq!(filter.search_text.as_deref(), Some("shipping blocker"));
    }

    #[test]
    fn build_task_filter_treats_whitespace_only_values_as_empty() {
        let filter = build_task_filter(
            None,
            None,
            None,
            None,
            Some("   ".to_string()),
            vec![" ".to_string()],
            vec![],
            vec![" ".to_string()],
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            Some("\t".to_string()),
            Some("\n".to_string()),
        )
        .expect("task filter should parse");

        assert!(is_empty_task_filter(&filter));
    }

    #[test]
    fn build_task_filter_rejects_unknown_assignee_type() {
        let error = build_task_filter(
            None,
            None,
            None,
            None,
            Some("robot".to_string()),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .expect_err("unknown assignee type should fail");

        assert_eq!(error.code, "invalid_input");
        assert!(error.message.contains("invalid assignee_type"));
    }

    #[test]
    fn parse_requirement_status_accepts_case_and_underscore_variants() {
        assert!(matches!(
            parse_requirement_status(" In_Progress "),
            Ok(RequirementStatus::InProgress)
        ));
        assert!(matches!(
            parse_requirement_status("PO_REVIEW"),
            Ok(RequirementStatus::PoReview)
        ));
    }

    #[test]
    fn parse_project_type_defaults_and_aliases_are_stable() {
        assert!(matches!(
            parse_project_type_opt(None),
            Ok(Some(ProjectType::Other))
        ));
        assert!(matches!(
            parse_project_type_opt(Some("full_stack")),
            Ok(Some(ProjectType::FullStackPlatform))
        ));
        assert!(matches!(
            parse_project_type_opt(Some("desktop_app")),
            Ok(Some(ProjectType::DesktopApp))
        ));
    }
}
