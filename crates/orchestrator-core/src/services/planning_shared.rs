use super::*;
use crate::state_machines::CompiledStateMachines;
use crate::{STANDARD_WORKFLOW_REF, UI_UX_WORKFLOW_REF};

mod requirement_lifecycle;
use requirement_lifecycle::run_requirement_lifecycle_state_machine;

pub(super) fn draft_vision_and_record(
    lock: &mut CoreState,
    project_root: String,
    project_name: String,
    input: VisionDraftInput,
    now: chrono::DateTime<Utc>,
) -> VisionDocument {
    let complexity_assessment = input.complexity_assessment.clone().unwrap_or_else(|| {
        infer_complexity_assessment(
            &input.problem_statement,
            &input.target_users,
            &input.goals,
            &input.constraints,
        )
    });
    let vision = VisionDocument {
        id: Uuid::new_v4().to_string(),
        project_root,
        markdown: build_vision_markdown(
            &project_name,
            &input.problem_statement,
            &input.target_users,
            &input.goals,
            &input.constraints,
            input.value_proposition.as_deref(),
            Some(&complexity_assessment),
        ),
        problem_statement: input.problem_statement,
        target_users: input.target_users,
        goals: input.goals,
        constraints: input.constraints,
        value_proposition: input.value_proposition,
        complexity_assessment: Some(complexity_assessment),
        created_at: now,
        updated_at: now,
    };

    lock.vision = Some(vision.clone());
    lock.logs.push(LogEntry {
        timestamp: now,
        level: LogLevel::Info,
        message: "vision drafted".to_string(),
    });
    vision
}

pub(super) fn draft_requirements_and_record(
    lock: &mut CoreState,
    input: RequirementsDraftInput,
    codebase_insight: Option<&CodebaseInsight>,
) -> Result<(Vec<String>, usize)> {
    let Some(vision) = lock.vision.clone() else {
        return Err(anyhow!("vision not found; run `ao vision draft` first"));
    };

    if !input.append_only {
        lock.requirements.clear();
    }

    // `0` means uncapped; callers can still pass an explicit limit.
    let max_requirements = input.max_requirements;
    let mut candidates = build_requirement_candidates(&vision, codebase_insight, max_requirements);
    let existing_titles: std::collections::HashSet<String> = lock
        .requirements
        .values()
        .map(|requirement| requirement.title.trim().to_ascii_lowercase())
        .collect();

    let mut appended_count = 0usize;
    let mut appended_ids = Vec::new();
    for candidate in &mut candidates {
        let title_key = candidate.title.trim().to_ascii_lowercase();
        if input.append_only && existing_titles.contains(&title_key) {
            continue;
        }

        if requirement_needs_research(candidate) {
            if !candidate
                .tags
                .iter()
                .any(|tag| tag.eq_ignore_ascii_case("needs-research"))
            {
                candidate.tags.push("needs-research".to_string());
            }
            if !candidate.acceptance_criteria.iter().any(|criterion| {
                criterion
                    .to_ascii_lowercase()
                    .contains("research findings documented")
            }) {
                candidate.acceptance_criteria.push(
                    "Research findings documented with sources and decision rationale.".to_string(),
                );
            }
        }

        let requirement_id = next_requirement_id(&lock.requirements);
        candidate.id = requirement_id.clone();
        if candidate
            .relative_path
            .as_ref()
            .map(|value| value.trim().is_empty())
            .unwrap_or(true)
        {
            candidate.relative_path = Some(format!("generated/{}.json", requirement_id));
        }
        candidate.created_at = Utc::now();
        candidate.updated_at = candidate.created_at;
        lock.requirements
            .insert(requirement_id.clone(), candidate.clone());
        appended_count = appended_count.saturating_add(1);
        appended_ids.push(requirement_id);
    }

    lock.logs.push(LogEntry {
        timestamp: Utc::now(),
        level: LogLevel::Info,
        message: format!("requirements drafted (added {appended_count})"),
    });

    Ok((appended_ids, appended_count))
}

pub(super) fn list_requirements_sorted(lock: &CoreState) -> Vec<RequirementItem> {
    let mut requirements: Vec<_> = lock.requirements.values().cloned().collect();
    requirements.sort_by(|a, b| a.id.cmp(&b.id));
    requirements
}

fn normalized_search_haystack(chunks: impl IntoIterator<Item = String>) -> String {
    chunks
        .into_iter()
        .collect::<Vec<_>>()
        .join(" ")
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

fn requirement_matches_filter(requirement: &RequirementItem, filter: &RequirementFilter) -> bool {
    if let Some(status) = filter.status {
        if requirement.status != status {
            return false;
        }
    }
    if let Some(priority) = filter.priority {
        if requirement.priority != priority {
            return false;
        }
    }
    if let Some(requirement_type) = filter.requirement_type {
        if requirement.requirement_type != Some(requirement_type) {
            return false;
        }
    }
    if let Some(category) = &filter.category {
        if requirement.category.as_deref() != Some(category.as_str()) {
            return false;
        }
    }
    if let Some(tags) = &filter.tags {
        if !tags.iter().all(|tag| requirement.tags.contains(tag)) {
            return false;
        }
    }
    if let Some(labels) = &filter.labels {
        if !labels
            .iter()
            .all(|label| requirement.labels.contains(label))
        {
            return false;
        }
    }
    if let Some(area) = &filter.area {
        if requirement.area.as_deref() != Some(area.as_str()) {
            return false;
        }
    }
    if let Some(source) = &filter.source {
        if !requirement.source.eq_ignore_ascii_case(source) {
            return false;
        }
    }
    if let Some(linked_epic_id) = &filter.linked_epic_id {
        if !requirement.linked_epic_ids.contains(linked_epic_id) {
            return false;
        }
    }
    if let Some(linked_task_id) = &filter.linked_task_id {
        if !requirement.linked_task_ids.contains(linked_task_id)
            && !requirement.links.tasks.contains(linked_task_id)
        {
            return false;
        }
    }
    if let Some(search_text) = &filter.search_text {
        let haystack = normalized_search_haystack(vec![
            requirement.id.clone(),
            requirement.title.clone(),
            requirement.description.clone(),
            requirement.body.clone().unwrap_or_default(),
            requirement.category.clone().unwrap_or_default(),
            requirement.source.clone(),
            requirement.area.clone().unwrap_or_default(),
            requirement.external_ref.clone().unwrap_or_default(),
            requirement.acceptance_criteria.join(" "),
            requirement.tags.join(" "),
            requirement.labels.join(" "),
        ]);
        let needle = normalized_search_haystack(vec![search_text.clone()]);
        if !haystack.contains(&needle) {
            return false;
        }
    }

    true
}

pub(super) fn list_requirements_filtered(
    lock: &CoreState,
    filter: &RequirementFilter,
) -> Vec<RequirementItem> {
    let mut requirements: Vec<_> = lock
        .requirements
        .values()
        .filter(|requirement| requirement_matches_filter(requirement, filter))
        .cloned()
        .collect();
    requirements.sort_by(|a, b| a.id.cmp(&b.id));
    requirements
}

pub(super) fn get_requirement(lock: &CoreState, id: &str) -> Result<RequirementItem> {
    lock.requirements
        .get(id)
        .cloned()
        .ok_or_else(|| anyhow!("requirement not found: {id}"))
}

pub(super) fn requirements_by_ids_sorted(
    lock: &CoreState,
    requirement_ids: &[String],
) -> Vec<RequirementItem> {
    let mut requirements: Vec<_> = requirement_ids
        .iter()
        .filter_map(|id| lock.requirements.get(id).cloned())
        .collect();
    requirements.sort_by(|a, b| a.id.cmp(&b.id));
    requirements
}

pub(super) fn list_epics_sorted(lock: &CoreState) -> Vec<EpicItem> {
    let mut epics: Vec<_> = lock.epics.values().cloned().collect();
    epics.sort_by(|a, b| a.id.cmp(&b.id));
    epics
}

pub(super) fn get_epic(lock: &CoreState, id: &str) -> Result<EpicItem> {
    lock.epics
        .get(id)
        .cloned()
        .ok_or_else(|| anyhow!("epic not found: {id}"))
}

pub(super) fn refine_requirements_and_record(
    lock: &mut CoreState,
    input: RequirementsRefineInput,
) -> Vec<RequirementItem> {
    let focus_hint = input
        .focus
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    let requirement_ids = input.requirement_ids;

    let mut refined = Vec::new();
    for requirement in lock.requirements.values_mut() {
        if !requirement_matches_id_filter(requirement, &requirement_ids) {
            continue;
        }
        let needs_research = requirement_needs_research(requirement);

        if requirement.acceptance_criteria.is_empty() {
            requirement
                .acceptance_criteria
                .push("Validation criteria must be measurable and testable.".to_string());
        }

        if !requirement
            .acceptance_criteria
            .iter()
            .any(|criterion| criterion.to_ascii_lowercase().contains("automated test"))
        {
            requirement
                .acceptance_criteria
                .push("Includes automated test coverage for core acceptance criteria.".to_string());
        }

        if let Some(focus_hint) = &focus_hint {
            let criteria = format!("Refinement focus addressed: {focus_hint}");
            if !requirement
                .acceptance_criteria
                .iter()
                .any(|value| value == &criteria)
            {
                requirement.acceptance_criteria.push(criteria);
            }
        }

        if needs_research {
            if !requirement
                .tags
                .iter()
                .any(|tag| tag.eq_ignore_ascii_case("needs-research"))
            {
                requirement.tags.push("needs-research".to_string());
            }

            let research_criteria = [
                "Research findings documented with sources and confidence notes.",
                "Requirement validation confirms research outputs are reflected in acceptance criteria.",
            ];
            for criterion in research_criteria {
                if !requirement
                    .acceptance_criteria
                    .iter()
                    .any(|value| value == criterion)
                {
                    requirement.acceptance_criteria.push(criterion.to_string());
                }
            }
        }

        requirement.status = RequirementStatus::Refined;
        requirement.updated_at = Utc::now();
        refined.push(requirement.clone());
    }

    refined.sort_by(|a, b| a.id.cmp(&b.id));

    lock.logs.push(LogEntry {
        timestamp: Utc::now(),
        level: LogLevel::Info,
        message: format!("requirements refined ({})", refined.len()),
    });

    refined
}

fn requirement_is_frontend_related(requirement: &RequirementItem) -> bool {
    let text = format!(
        "{} {} {}",
        requirement.title,
        requirement.description,
        requirement.acceptance_criteria.join(" ")
    );
    crate::types::is_frontend_related_content(&requirement.tags, &text)
}

fn requirement_needs_research(requirement: &RequirementItem) -> bool {
    if requirement.tags.iter().any(|tag| {
        matches!(
            tag.trim().to_ascii_lowercase().as_str(),
            "needs-research" | "research" | "discovery" | "investigation" | "spike"
        )
    }) {
        return true;
    }

    let haystack = format!(
        "{} {} {}",
        requirement.title,
        requirement.description,
        requirement.acceptance_criteria.join(" ")
    )
    .to_ascii_lowercase();
    [
        "research",
        "investigate",
        "evaluate",
        "compare",
        "benchmark",
        "unknown",
        "spike",
        "feasibility",
        "tradeoff",
        "decision",
        "validate assumptions",
    ]
    .iter()
    .any(|needle| haystack.contains(needle))
}

fn criterion_title_fragment(criterion: &str, max_chars: usize) -> String {
    let cleaned = criterion
        .trim()
        .trim_start_matches('-')
        .trim()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let mut out = String::new();
    for ch in cleaned.chars() {
        if out.chars().count() >= max_chars {
            break;
        }
        out.push(ch);
    }
    if cleaned.chars().count() > max_chars {
        out.push_str("...");
    }
    if out.is_empty() {
        "acceptance criterion".to_string()
    } else {
        out
    }
}

fn is_generic_acceptance_criterion(criterion: &str) -> bool {
    let normalized = criterion.trim().to_ascii_lowercase();
    [
        "repository contains runnable product code that implements the goal end-to-end.",
        "apis and data contracts needed by the goal are implemented with persisted run history.",
        "automated tests cover the critical user journey for this goal.",
        "includes automated test coverage for core acceptance criteria.",
        "refinement focus addressed:",
        "implementation demonstrates explicit compliance with vision constraint:",
        "verification artifacts (tests/checklists/review notes) confirm the constraint is enforced.",
    ]
    .iter()
    .any(|prefix| normalized.starts_with(prefix))
}

fn build_requirement_task_specs(
    requirement: &RequirementItem,
    assessment: &ComplexityAssessment,
) -> Vec<(String, String)> {
    let build_detailed_description = |scope_slice: &str, scoped_criteria: &[String]| -> String {
        let acceptance_criteria = if scoped_criteria.is_empty() {
            if requirement.acceptance_criteria.is_empty() {
                vec!["Deliverable is implemented and validated.".to_string()]
            } else {
                requirement.acceptance_criteria.clone()
            }
        } else {
            scoped_criteria.to_vec()
        };

        format!(
                "{}\n\n## Scope Slice\n{}\n\n## Implementation Notes\n- Build this slice as an independently reviewable change.\n- Keep compatibility with linked requirement constraints and existing architecture.\n\n## Acceptance Criteria\n{}\n\n## Validation Checklist\n{}\n",
                requirement.description,
                scope_slice,
                acceptance_criteria
                    .iter()
                    .map(|criterion| format!("- {}", criterion))
                    .collect::<Vec<_>>()
                    .join("\n"),
                requirement
                    .acceptance_criteria
                    .iter()
                    .map(|criterion| format!("- [ ] {}", criterion))
                    .collect::<Vec<_>>()
                    .join("\n")
            )
    };

    // Constraint requirements should stay focused and usually map to one
    // enforceable task.
    if requirement.source.eq_ignore_ascii_case("vision-constraint") {
        let description = build_detailed_description(&requirement.title, &[]);
        return vec![(requirement.title.clone(), description)];
    }

    let actionable_criteria = requirement
        .acceptance_criteria
        .iter()
        .filter(|criterion| !is_generic_acceptance_criterion(criterion))
        .map(|criterion| criterion.trim().to_string())
        .filter(|criterion| !criterion.is_empty())
        .collect::<Vec<_>>();

    let (min_tasks, max_tasks) = assessment.tier.task_count_range();
    if actionable_criteria.len() <= 1 && min_tasks <= 1 {
        let description = build_detailed_description(&requirement.title, &[]);
        return vec![(requirement.title.clone(), description)];
    }

    let preferred_count = actionable_criteria.len().clamp(min_tasks, max_tasks);

    let mut fragments = actionable_criteria
        .iter()
        .map(|criterion| criterion_title_fragment(criterion, 56))
        .collect::<Vec<_>>();
    let fallback_fragments = [
        "Implementation",
        "Integration",
        "Testing",
        "Docs",
        "Hardening",
        "Release Readiness",
    ];
    for fallback in fallback_fragments {
        if fragments.len() >= preferred_count {
            break;
        }
        fragments.push(fallback.to_string());
    }

    let mut specs = Vec::new();
    for (index, fragment) in fragments.into_iter().take(preferred_count).enumerate() {
        let title = if preferred_count == 1 {
            requirement.title.clone()
        } else {
            format!("{} - {}", requirement.title, fragment)
        };
        let scoped_criterion = actionable_criteria.get(index).cloned();
        let acceptance_block = scoped_criterion
            .or_else(|| requirement.acceptance_criteria.get(index).cloned())
            .unwrap_or_else(|| "Deliverable is implemented and validated.".to_string());
        let description = build_detailed_description(&fragment, &[acceptance_block]);
        specs.push((title, description));
    }

    specs
}

fn sync_requirement_task_links(requirement: &mut RequirementItem, task_ids: &[String]) {
    for task_id in task_ids {
        if !requirement
            .linked_task_ids
            .iter()
            .any(|existing| existing == task_id)
        {
            requirement.linked_task_ids.push(task_id.clone());
        }
        if !requirement
            .links
            .tasks
            .iter()
            .any(|existing| existing == task_id)
        {
            requirement.links.tasks.push(task_id.clone());
        }
    }

    requirement.linked_task_ids.sort();
    requirement.linked_task_ids.dedup();
    requirement.links.tasks.sort();
    requirement.links.tasks.dedup();
}

fn default_task_checklist(needs_research: bool, now: chrono::DateTime<Utc>) -> Vec<ChecklistItem> {
    let mut checklist_items = vec![
        "Requirement scope refined and approved by PO/EM gates.".to_string(),
        "Code review gate passes with all blocking feedback resolved.".to_string(),
        "QA/testing gate passes with automated validation evidence.".to_string(),
    ];
    if needs_research {
        checklist_items.insert(
            1,
            "Research findings are captured and linked before implementation proceeds.".to_string(),
        );
    }

    checklist_items
        .into_iter()
        .enumerate()
        .map(|(index, description)| ChecklistItem {
            id: format!("CHK-{:03}", index + 1),
            description,
            completed: false,
            created_at: now,
            completed_at: None,
        })
        .collect()
}

fn unsatisfied_blocked_dependencies(lock: &CoreState, task: &OrchestratorTask) -> Vec<String> {
    let mut issues = Vec::new();
    for dependency in &task.dependencies {
        if dependency.dependency_type != DependencyType::BlockedBy {
            continue;
        }

        match lock.tasks.get(&dependency.task_id) {
            Some(dep_task) if dep_task.status == TaskStatus::Done => {}
            Some(dep_task) => issues.push(format!("{} ({})", dependency.task_id, dep_task.status)),
            None => issues.push(format!("{} (missing)", dependency.task_id)),
        }
    }
    issues
}

pub(super) fn execute_requirements_and_record(
    lock: &mut CoreState,
    input: RequirementsExecutionInput,
    project_root: Option<&std::path::Path>,
    workflow_manager: Option<&WorkflowStateManager>,
    state_machines: Option<&CompiledStateMachines>,
) -> Result<RequirementsExecutionResult> {
    let mut selected_requirements: Vec<String> = lock
        .requirements
        .values()
        .filter(|requirement| {
            requirement_matches_id_filter(requirement, &input.requirement_ids)
                && (input.include_wont || requirement.priority != crate::RequirementPriority::Wont)
        })
        .map(|requirement| requirement.id.clone())
        .collect();
    selected_requirements.sort();

    if selected_requirements.is_empty() {
        let guidance = if input.include_wont {
            "run `ao requirements draft` first"
        } else {
            "run `ao requirements draft` first or pass `--include-wont true` to include out-of-scope requirements"
        };
        return Err(anyhow!("no requirements matched; {guidance}"));
    }

    let requirements_snapshot = lock
        .requirements
        .values()
        .cloned()
        .collect::<Vec<RequirementItem>>();
    let missing_constraints =
        missing_vision_constraint_coverage(lock.vision.as_ref(), &requirements_snapshot);
    if !missing_constraints.is_empty() {
        return Err(anyhow!(
            "vision constraints missing from requirements: {}. \
Run `ao requirements draft`/`ao requirements refine` (or upsert explicit constraint requirements) before execution",
            missing_constraints.join(" | ")
        ));
    }

    let vision = lock.vision.clone();
    let mut lifecycle_failures = Vec::new();
    for requirement_id in &selected_requirements {
        let Some(requirement) = lock.requirements.get_mut(requirement_id) else {
            continue;
        };
        if let Err(error) =
            run_requirement_lifecycle_state_machine(requirement, vision.as_ref(), state_machines)
        {
            lifecycle_failures.push(format!("{requirement_id}: {error}"));
        }
    }
    if !lifecycle_failures.is_empty() {
        return Err(anyhow!(
            "requirement lifecycle gating failed: {}",
            lifecycle_failures.join(" | ")
        ));
    }

    let mut task_ids_created = Vec::new();
    let mut task_ids_reused = Vec::new();
    let mut requirement_to_tasks: HashMap<String, Vec<String>> = HashMap::new();
    let complexity_assessment = lock
        .vision
        .as_ref()
        .map(effective_complexity_assessment)
        .unwrap_or_default();

    for requirement_id in &selected_requirements {
        let mut existing_task_ids = lock
            .requirements
            .get(requirement_id)
            .map(|requirement| {
                let mut merged = requirement.linked_task_ids.clone();
                merged.extend(requirement.links.tasks.clone());
                merged
            })
            .unwrap_or_default();

        if existing_task_ids.is_empty() {
            existing_task_ids = lock
                .tasks
                .values()
                .filter(|task| {
                    task.linked_requirements
                        .iter()
                        .any(|id| id == requirement_id)
                })
                .map(|task| task.id.clone())
                .collect();
            existing_task_ids.sort();
            existing_task_ids.dedup();
        }

        let task_ids_for_requirement = if !existing_task_ids.is_empty() {
            task_ids_reused.extend(existing_task_ids.clone());
            existing_task_ids
        } else {
            let Some(requirement) = lock.requirements.get(requirement_id).cloned() else {
                continue;
            };
            let frontend_related = requirement_is_frontend_related(&requirement);
            let needs_research = requirement_needs_research(&requirement);
            let task_specs = build_requirement_task_specs(&requirement, &complexity_assessment);
            let mut created_task_ids = Vec::new();

            for (title, description) in task_specs {
                let task_id = next_task_id(&lock.tasks);
                let now = Utc::now();
                let task = OrchestratorTask {
                    id: task_id.clone(),
                    title,
                    description,
                    task_type: TaskType::Feature,
                    status: TaskStatus::Backlog,
                    blocked_reason: None,
                    blocked_at: None,
                    blocked_phase: None,
                    blocked_by: None,
                    priority: requirement.priority.to_task_priority(),
                    risk: RiskLevel::Medium,
                    scope: Scope::Medium,
                    complexity: Complexity::Medium,
                    impact_area: if frontend_related {
                        vec![crate::types::ImpactArea::Frontend]
                    } else {
                        Vec::new()
                    },
                    assignee: Assignee::Agent {
                        role: "implementation".to_string(),
                        model: None,
                    },
                    estimated_effort: None,
                    epic_id: if requirement.linked_epic_ids.len() == 1 {
                        requirement.linked_epic_ids.first().cloned()
                    } else {
                        None
                    },
                    parent_task_id: None,
                    linked_requirements: vec![requirement.id.clone()],
                    linked_architecture_entities: Vec::new(),
                    dependencies: Vec::new(),
                    checklist: default_task_checklist(needs_research, now),
                    tags: {
                        let mut tags = vec!["from-requirement".to_string()];
                        tags.push("requirement-derived".to_string());
                        if frontend_related {
                            tags.push("frontend".to_string());
                            tags.push("ui-ux".to_string());
                        }
                        if needs_research {
                            tags.push("needs-research".to_string());
                        }
                        tags
                    },
                    labels: requirement.labels.clone(),
                    area: requirement.area.clone(),
                    external_ref: requirement.external_ref.clone(),
                    workflow_metadata: WorkflowMetadata {
                        requires_design: frontend_related,
                        requires_architecture: needs_research,
                        ..WorkflowMetadata::default()
                    },
                    worktree_path: None,
                    branch_name: None,
                    metadata: TaskMetadata {
                        created_at: now,
                        updated_at: now,
                        created_by: "requirement-review-loop-ai".to_string(),
                        updated_by: "requirement-review-loop-ai".to_string(),
                        started_at: None,
                        completed_at: None,
                        version: 1,
                    },
                    deadline: None,
                    paused: false,
                    cancelled: false,
                    resolution: None,
                    resource_requirements: Default::default(),
                    consecutive_dispatch_failures: None,
                    last_dispatch_failure_at: None,
                    dispatch_history: Vec::new(),
                };
                lock.tasks.insert(task_id.clone(), task);
                task_ids_created.push(task_id.clone());
                created_task_ids.push(task_id);
            }

            created_task_ids
        };

        if let Some(requirement) = lock.requirements.get_mut(requirement_id) {
            sync_requirement_task_links(requirement, &task_ids_for_requirement);
            requirement.status = if input.start_workflows {
                RequirementStatus::InProgress
            } else {
                RequirementStatus::Planned
            };
            requirement.updated_at = Utc::now();
        }
        requirement_to_tasks.insert(requirement_id.clone(), task_ids_for_requirement);
    }

    let mut workflow_ids_started = Vec::new();
    if input.start_workflows {
        for task_id in requirement_to_tasks.values().flatten() {
            let dependency_issues = {
                let Some(task) = lock.tasks.get(task_id) else {
                    continue;
                };
                unsatisfied_blocked_dependencies(lock, task)
            };
            if !dependency_issues.is_empty() {
                if let Some(task) = lock.tasks.get_mut(task_id) {
                    task.status = TaskStatus::Blocked;
                    task.paused = true;
                    task.blocked_reason = Some(format!(
                        "dependency gate: waiting on {}",
                        dependency_issues.join(", ")
                    ));
                    task.blocked_at = Some(Utc::now());
                    task.metadata.updated_at = Utc::now();
                    task.metadata.updated_by = protocol::ACTOR_CLI.to_string();
                    task.metadata.version = task.metadata.version.saturating_add(1);
                }
                continue;
            }

            let has_active_workflow = lock.workflows.values().any(|workflow| {
                workflow.task_id == *task_id
                    && matches!(
                        workflow.status,
                        crate::WorkflowStatus::Running
                            | crate::WorkflowStatus::Pending
                            | crate::WorkflowStatus::Paused
                    )
                    && workflow.machine_state != crate::types::WorkflowMachineState::MergeConflict
            });
            if has_active_workflow {
                continue;
            }

            let task = lock.tasks.get(task_id).cloned();
            let workflow_ref = input.workflow_ref.clone().unwrap_or_else(|| {
                if task
                    .as_ref()
                    .map(|task| task.is_frontend_related())
                    .unwrap_or(false)
                {
                    UI_UX_WORKFLOW_REF.to_string()
                } else {
                    STANDARD_WORKFLOW_REF.to_string()
                }
            });
            let phase_plan = crate::resolve_phase_plan_for_workflow_ref(
                project_root,
                Some(workflow_ref.as_str()),
            )?;
            let executor = if let Some(machine_catalog) = state_machines {
                WorkflowLifecycleExecutor::with_state_machines(phase_plan, machine_catalog.clone())
            } else {
                WorkflowLifecycleExecutor::new(phase_plan)
            };
            let workflow_id = Uuid::new_v4().to_string();
            let workflow = executor.bootstrap(
                workflow_id.clone(),
                WorkflowRunInput::for_task(task_id.clone(), Some(workflow_ref)),
            );

            if let Some(manager) = workflow_manager {
                manager.save(&workflow)?;
                let workflow = manager.save_checkpoint(&workflow, CheckpointReason::Start)?;
                lock.workflows.insert(workflow_id.clone(), workflow);
            } else {
                lock.workflows.insert(workflow_id.clone(), workflow);
            }
            workflow_ids_started.push(workflow_id);
        }
    }

    lock.logs.push(LogEntry {
        timestamp: Utc::now(),
        level: LogLevel::Info,
        message: format!(
            "requirements executed (considered: {}, tasks created: {}, workflows: {})",
            selected_requirements.len(),
            task_ids_created.len(),
            workflow_ids_started.len()
        ),
    });

    Ok(RequirementsExecutionResult {
        requirements_considered: selected_requirements.len(),
        task_ids_created,
        task_ids_reused,
        workflow_ids_started,
    })
}
