use std::sync::Arc;

use anyhow::Result;
use orchestrator_core::{
    services::ServiceHub, Complexity, ImpactArea, OrchestratorTask, RiskLevel, Scope,
    TaskCreateInput, TaskFilter, TaskUpdateInput,
};

use crate::{
    ensure_destructive_confirmation, invalid_input_error, parse_complexity_opt,
    parse_dependency_type, parse_impact_areas, parse_input_json_or, parse_priority_opt,
    parse_risk_opt, parse_scope_opt, parse_task_status, parse_task_type_opt, print_value,
    TaskCommand, TaskCreateArgs, TaskUpdateArgs,
};

#[derive(Debug, Default)]
struct TaskFieldPatch {
    risk: Option<RiskLevel>,
    scope: Option<Scope>,
    complexity: Option<Complexity>,
    impact_area: Option<Vec<ImpactArea>>,
    estimated_effort: Option<String>,
    max_cpu_percent: Option<f32>,
    max_memory_mb: Option<u64>,
    requires_network: Option<bool>,
    clear_max_cpu_percent: bool,
    clear_max_memory_mb: bool,
}

impl TaskFieldPatch {
    fn has_updates(&self) -> bool {
        self.risk.is_some()
            || self.scope.is_some()
            || self.complexity.is_some()
            || self.impact_area.is_some()
            || self.estimated_effort.is_some()
            || self.max_cpu_percent.is_some()
            || self.max_memory_mb.is_some()
            || self.requires_network.is_some()
            || self.clear_max_cpu_percent
            || self.clear_max_memory_mb
    }

    fn apply(self, task: &mut OrchestratorTask) {
        if let Some(risk) = self.risk {
            task.risk = risk;
        }
        if let Some(scope) = self.scope {
            task.scope = scope;
        }
        if let Some(complexity) = self.complexity {
            task.complexity = complexity;
        }
        if let Some(impact_area) = self.impact_area {
            task.impact_area = impact_area;
        }
        if let Some(estimated_effort) = self.estimated_effort {
            task.estimated_effort = normalize_estimated_effort(Some(estimated_effort));
        }
        if self.clear_max_cpu_percent {
            task.resource_requirements.max_cpu_percent = None;
        }
        if self.clear_max_memory_mb {
            task.resource_requirements.max_memory_mb = None;
        }
        if let Some(max_cpu_percent) = self.max_cpu_percent {
            task.resource_requirements.max_cpu_percent = Some(max_cpu_percent);
        }
        if let Some(max_memory_mb) = self.max_memory_mb {
            task.resource_requirements.max_memory_mb = Some(max_memory_mb);
        }
        if let Some(requires_network) = self.requires_network {
            task.resource_requirements.requires_network = requires_network;
        }
    }
}

fn normalize_estimated_effort(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let trimmed = value.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_string())
    })
}

fn validate_max_cpu_percent(max_cpu_percent: Option<f32>) -> Result<Option<f32>> {
    let Some(value) = max_cpu_percent else {
        return Ok(None);
    };

    if !value.is_finite() || value <= 0.0 || value > 100.0 {
        return Err(invalid_input_error(format!(
            "invalid --max-cpu-percent '{value}'; expected a number greater than 0 and less than or equal to 100; run the same command with --help"
        )));
    }

    Ok(Some(value))
}

fn validate_max_memory_mb(max_memory_mb: Option<u64>) -> Result<Option<u64>> {
    let Some(value) = max_memory_mb else {
        return Ok(None);
    };

    if value == 0 {
        return Err(invalid_input_error(
            "invalid --max-memory-mb '0'; expected a whole number greater than 0; run the same command with --help",
        ));
    }

    Ok(Some(value))
}

fn build_create_field_patch(args: &TaskCreateArgs) -> Result<TaskFieldPatch> {
    let impact_area = if args.impact_area.is_empty() {
        None
    } else {
        Some(parse_impact_areas(&args.impact_area)?)
    };

    Ok(TaskFieldPatch {
        risk: parse_risk_opt(args.risk.as_deref())?,
        scope: parse_scope_opt(args.scope.as_deref())?,
        complexity: parse_complexity_opt(args.complexity.as_deref())?,
        impact_area,
        estimated_effort: normalize_estimated_effort(args.estimated_effort.clone()),
        max_cpu_percent: validate_max_cpu_percent(args.max_cpu_percent)?,
        max_memory_mb: validate_max_memory_mb(args.max_memory_mb)?,
        requires_network: args.requires_network,
        clear_max_cpu_percent: false,
        clear_max_memory_mb: false,
    })
}

fn build_update_field_patch(args: &TaskUpdateArgs) -> Result<TaskFieldPatch> {
    if args.clear_max_cpu_percent && args.max_cpu_percent.is_some() {
        return Err(invalid_input_error(
            "cannot combine --max-cpu-percent with --clear-max-cpu-percent",
        ));
    }
    if args.clear_max_memory_mb && args.max_memory_mb.is_some() {
        return Err(invalid_input_error(
            "cannot combine --max-memory-mb with --clear-max-memory-mb",
        ));
    }

    let impact_area = if args.replace_impact_area || !args.impact_area.is_empty() {
        Some(parse_impact_areas(&args.impact_area)?)
    } else {
        None
    };

    Ok(TaskFieldPatch {
        risk: parse_risk_opt(args.risk.as_deref())?,
        scope: parse_scope_opt(args.scope.as_deref())?,
        complexity: parse_complexity_opt(args.complexity.as_deref())?,
        impact_area,
        estimated_effort: normalize_estimated_effort(args.estimated_effort.clone()),
        max_cpu_percent: validate_max_cpu_percent(args.max_cpu_percent)?,
        max_memory_mb: validate_max_memory_mb(args.max_memory_mb)?,
        requires_network: args.requires_network,
        clear_max_cpu_percent: args.clear_max_cpu_percent,
        clear_max_memory_mb: args.clear_max_memory_mb,
    })
}

fn create_field_patch_from_args(args: &TaskCreateArgs) -> Result<TaskFieldPatch> {
    if args.input_json.is_some() {
        return Ok(TaskFieldPatch::default());
    }
    build_create_field_patch(args)
}

fn update_field_patch_from_args(args: &TaskUpdateArgs) -> Result<TaskFieldPatch> {
    if args.input_json.is_some() {
        return Ok(TaskFieldPatch::default());
    }
    build_update_field_patch(args)
}

fn align_version_for_follow_up_replace(task: &mut OrchestratorTask) {
    // create/update already bump metadata.version; compensate before replace so
    // an advanced-field patch behaves as a single logical mutation.
    task.metadata.version = task.metadata.version.saturating_sub(1);
}

pub(crate) async fn handle_task(
    command: TaskCommand,
    hub: Arc<dyn ServiceHub>,
    json: bool,
) -> Result<()> {
    let tasks = hub.tasks();

    match command {
        TaskCommand::List(args) => {
            let filter = TaskFilter {
                task_type: parse_task_type_opt(args.task_type.as_deref())?,
                status: match args.status {
                    Some(status) => Some(parse_task_status(&status)?),
                    None => None,
                },
                priority: parse_priority_opt(args.priority.as_deref())?,
                risk: parse_risk_opt(args.risk.as_deref())?,
                assignee_type: args.assignee_type,
                tags: if args.tag.is_empty() {
                    None
                } else {
                    Some(args.tag)
                },
                linked_requirement: args.linked_requirement,
                linked_architecture_entity: args.linked_architecture_entity,
                search_text: args.search,
            };

            if filter.task_type.is_none()
                && filter.status.is_none()
                && filter.priority.is_none()
                && filter.risk.is_none()
                && filter.assignee_type.is_none()
                && filter.tags.is_none()
                && filter.linked_requirement.is_none()
                && filter.linked_architecture_entity.is_none()
                && filter.search_text.is_none()
            {
                print_value(tasks.list().await?, json)
            } else {
                print_value(tasks.list_filtered(filter).await?, json)
            }
        }
        TaskCommand::Prioritized => print_value(tasks.list_prioritized().await?, json),
        TaskCommand::Next => print_value(tasks.next_task().await?, json),
        TaskCommand::Stats => print_value(tasks.statistics().await?, json),
        TaskCommand::Get(args) => print_value(tasks.get(&args.id).await?, json),
        TaskCommand::Create(args) => {
            let field_patch = create_field_patch_from_args(&args)?;
            let input = parse_input_json_or(args.input_json, || {
                Ok(TaskCreateInput {
                    title: args.title,
                    description: args.description,
                    task_type: parse_task_type_opt(args.task_type.as_deref())?,
                    priority: parse_priority_opt(args.priority.as_deref())?,
                    created_by: Some("ao-cli".to_string()),
                    tags: Vec::new(),
                    linked_requirements: Vec::new(),
                    linked_architecture_entities: args.linked_architecture_entity,
                })
            })?;
            let mut task = tasks.create(input).await?;
            if field_patch.has_updates() {
                field_patch.apply(&mut task);
                align_version_for_follow_up_replace(&mut task);
                task = tasks.replace(task).await?;
            }
            print_value(task, json)
        }
        TaskCommand::Update(args) => {
            let field_patch = update_field_patch_from_args(&args)?;
            let input = parse_input_json_or(args.input_json, || {
                Ok(TaskUpdateInput {
                    title: args.title,
                    description: args.description,
                    priority: parse_priority_opt(args.priority.as_deref())?,
                    status: match args.status {
                        Some(status) => Some(parse_task_status(&status)?),
                        None => None,
                    },
                    assignee: args.assignee,
                    tags: None,
                    updated_by: Some("ao-cli".to_string()),
                    deadline: None,
                    linked_architecture_entities: if args.replace_linked_architecture_entities
                        || !args.linked_architecture_entity.is_empty()
                    {
                        Some(args.linked_architecture_entity)
                    } else {
                        None
                    },
                })
            })?;
            let mut task = tasks.update(&args.id, input).await?;
            if field_patch.has_updates() {
                field_patch.apply(&mut task);
                align_version_for_follow_up_replace(&mut task);
                task = tasks.replace(task).await?;
            }
            print_value(task, json)
        }
        TaskCommand::Delete(args) => {
            let task = tasks.get(&args.id).await?;
            if args.dry_run {
                let task_id = task.id.clone();
                let task_title = task.title.clone();
                let task_status = task.status.clone();
                let task_paused = task.paused;
                let task_cancelled = task.cancelled;
                return print_value(
                    serde_json::json!({
                        "operation": "task.delete",
                        "target": {
                            "task_id": task_id.clone(),
                        },
                        "action": "task.delete",
                        "dry_run": true,
                        "destructive": true,
                        "requires_confirmation": true,
                        "planned_effects": [
                            "delete task from project state",
                        ],
                        "next_step": format!(
                            "rerun 'ao task delete --id {} --confirm {}' to apply",
                            task_id,
                            task_id
                        ),
                        "task": {
                            "id": task_id.clone(),
                            "title": task_title,
                            "status": task_status,
                            "paused": task_paused,
                            "cancelled": task_cancelled,
                        },
                    }),
                    json,
                );
            }

            ensure_destructive_confirmation(
                args.confirm.as_deref(),
                &args.id,
                "task delete",
                "--id",
            )?;
            tasks.delete(&args.id).await?;
            print_value(
                serde_json::json!({
                    "success": true,
                    "message": "task deleted",
                    "task_id": args.id,
                }),
                json,
            )
        }
        TaskCommand::Assign(args) => {
            print_value(tasks.assign(&args.id, args.assignee).await?, json)
        }
        TaskCommand::AssignAgent(args) => print_value(
            tasks
                .assign_agent(&args.id, args.role, args.model, args.updated_by)
                .await?,
            json,
        ),
        TaskCommand::AssignHuman(args) => print_value(
            tasks
                .assign_human(&args.id, args.user_id, args.updated_by)
                .await?,
            json,
        ),
        TaskCommand::ChecklistAdd(args) => print_value(
            tasks
                .add_checklist_item(&args.id, args.description, args.updated_by)
                .await?,
            json,
        ),
        TaskCommand::ChecklistUpdate(args) => print_value(
            tasks
                .update_checklist_item(&args.id, &args.item_id, args.completed, args.updated_by)
                .await?,
            json,
        ),
        TaskCommand::DependencyAdd(args) => {
            let dependency_type = parse_dependency_type(&args.dependency_type)?;
            print_value(
                tasks
                    .add_dependency(
                        &args.id,
                        &args.dependency_id,
                        dependency_type,
                        args.updated_by,
                    )
                    .await?,
                json,
            )
        }
        TaskCommand::DependencyRemove(args) => print_value(
            tasks
                .remove_dependency(&args.id, &args.dependency_id, args.updated_by)
                .await?,
            json,
        ),
        TaskCommand::Status(args) => {
            let status = parse_task_status(&args.status)?;
            print_value(tasks.set_status(&args.id, status).await?, json)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_create_args() -> TaskCreateArgs {
        TaskCreateArgs {
            title: "Task".to_string(),
            description: String::new(),
            task_type: None,
            priority: None,
            risk: None,
            scope: None,
            complexity: None,
            impact_area: Vec::new(),
            estimated_effort: None,
            max_cpu_percent: None,
            max_memory_mb: None,
            requires_network: None,
            linked_architecture_entity: Vec::new(),
            input_json: None,
        }
    }

    fn sample_update_args() -> TaskUpdateArgs {
        TaskUpdateArgs {
            id: "TASK-001".to_string(),
            title: None,
            description: None,
            priority: None,
            status: None,
            risk: None,
            scope: None,
            complexity: None,
            impact_area: Vec::new(),
            replace_impact_area: false,
            estimated_effort: None,
            max_cpu_percent: None,
            max_memory_mb: None,
            clear_max_cpu_percent: false,
            clear_max_memory_mb: false,
            requires_network: None,
            assignee: None,
            linked_architecture_entity: Vec::new(),
            replace_linked_architecture_entities: false,
            input_json: None,
        }
    }

    #[test]
    fn validate_max_cpu_percent_rejects_zero_and_accepts_upper_bound() {
        assert!(validate_max_cpu_percent(Some(0.0)).is_err());
        assert_eq!(
            validate_max_cpu_percent(Some(100.0)).expect("100 should be valid"),
            Some(100.0)
        );
    }

    #[test]
    fn validate_max_memory_mb_rejects_zero() {
        assert!(validate_max_memory_mb(Some(0)).is_err());
        assert_eq!(
            validate_max_memory_mb(Some(256)).expect("positive memory must be valid"),
            Some(256)
        );
    }

    #[test]
    fn normalize_estimated_effort_trims_and_drops_empty_values() {
        assert_eq!(
            normalize_estimated_effort(Some(" 2d ".to_string())),
            Some("2d".to_string())
        );
        assert_eq!(normalize_estimated_effort(Some(" \n\t ".to_string())), None);
    }

    #[test]
    fn update_patch_rejects_conflicting_cpu_flags() {
        let mut args = sample_update_args();
        args.max_cpu_percent = Some(40.0);
        args.clear_max_cpu_percent = true;

        let err = build_update_field_patch(&args)
            .expect_err("conflicting cpu flags should be rejected");
        assert!(err
            .to_string()
            .contains("cannot combine --max-cpu-percent with --clear-max-cpu-percent"));
    }

    #[test]
    fn create_patch_ignores_advanced_flags_when_input_json_is_present() {
        let mut args = sample_create_args();
        args.input_json = Some("{\"title\":\"json task\"}".to_string());
        args.risk = Some("invalid-risk".to_string());
        args.max_cpu_percent = Some(0.0);

        let patch =
            create_field_patch_from_args(&args).expect("input-json mode should skip flag parsing");
        assert!(!patch.has_updates());
    }

    #[test]
    fn update_patch_ignores_advanced_flags_when_input_json_is_present() {
        let mut args = sample_update_args();
        args.input_json = Some("{\"title\":\"json update\"}".to_string());
        args.scope = Some("not-a-scope".to_string());
        args.clear_max_cpu_percent = true;
        args.max_cpu_percent = Some(90.0);

        let patch =
            update_field_patch_from_args(&args).expect("input-json mode should skip flag parsing");
        assert!(!patch.has_updates());
    }

    #[tokio::test]
    async fn create_with_advanced_fields_keeps_version_at_initial_value() {
        let hub = Arc::new(orchestrator_core::InMemoryServiceHub::new());
        let mut args = sample_create_args();
        args.title = "Advanced Task".to_string();
        args.risk = Some("high".to_string());

        handle_task(TaskCommand::Create(args), hub.clone(), true)
            .await
            .expect("create should succeed");

        let task = hub
            .tasks()
            .get("TASK-001")
            .await
            .expect("created task should be readable");
        assert_eq!(task.metadata.version, 1);
        assert_eq!(task.risk, RiskLevel::High);
    }

    #[tokio::test]
    async fn create_with_all_advanced_fields_applies_expected_values() {
        let hub = Arc::new(orchestrator_core::InMemoryServiceHub::new());
        let mut args = sample_create_args();
        args.title = "Advanced Task".to_string();
        args.risk = Some("high".to_string());
        args.scope = Some("large".to_string());
        args.complexity = Some("high".to_string());
        args.impact_area = vec![
            "frontend".to_string(),
            "backend".to_string(),
            "frontend".to_string(),
        ];
        args.estimated_effort = Some(" 2d ".to_string());
        args.max_cpu_percent = Some(75.0);
        args.max_memory_mb = Some(2048);
        args.requires_network = Some(false);

        handle_task(TaskCommand::Create(args), hub.clone(), true)
            .await
            .expect("create should succeed");

        let task = hub
            .tasks()
            .get("TASK-001")
            .await
            .expect("created task should be readable");
        assert_eq!(task.metadata.version, 1);
        assert_eq!(task.risk, RiskLevel::High);
        assert_eq!(task.scope, Scope::Large);
        assert_eq!(task.complexity, Complexity::High);
        assert_eq!(
            task.impact_area,
            vec![ImpactArea::Frontend, ImpactArea::Backend]
        );
        assert_eq!(task.estimated_effort.as_deref(), Some("2d"));
        assert_eq!(task.resource_requirements.max_cpu_percent, Some(75.0));
        assert_eq!(task.resource_requirements.max_memory_mb, Some(2048));
        assert!(!task.resource_requirements.requires_network);
    }

    #[tokio::test]
    async fn update_with_advanced_fields_increments_version_once() {
        let hub = Arc::new(orchestrator_core::InMemoryServiceHub::new());
        hub.tasks()
            .create(TaskCreateInput {
                title: "Task".to_string(),
                description: String::new(),
                task_type: None,
                priority: None,
                created_by: Some("test".to_string()),
                tags: Vec::new(),
                linked_requirements: Vec::new(),
                linked_architecture_entities: Vec::new(),
            })
            .await
            .expect("seed task should be created");

        let mut args = sample_update_args();
        args.risk = Some("low".to_string());

        handle_task(TaskCommand::Update(args), hub.clone(), true)
            .await
            .expect("update should succeed");

        let task = hub
            .tasks()
            .get("TASK-001")
            .await
            .expect("updated task should be readable");
        assert_eq!(task.metadata.version, 2);
        assert_eq!(task.risk, RiskLevel::Low);
    }

    #[tokio::test]
    async fn update_with_clear_resource_flags_unsets_existing_limits() {
        let hub = Arc::new(orchestrator_core::InMemoryServiceHub::new());
        let mut create_args = sample_create_args();
        create_args.max_cpu_percent = Some(60.0);
        create_args.max_memory_mb = Some(1024);

        handle_task(TaskCommand::Create(create_args), hub.clone(), true)
            .await
            .expect("seed create should succeed");

        let mut update_args = sample_update_args();
        update_args.clear_max_cpu_percent = true;
        update_args.clear_max_memory_mb = true;

        handle_task(TaskCommand::Update(update_args), hub.clone(), true)
            .await
            .expect("update should succeed");

        let task = hub
            .tasks()
            .get("TASK-001")
            .await
            .expect("updated task should be readable");
        assert_eq!(task.metadata.version, 2);
        assert_eq!(task.resource_requirements.max_cpu_percent, None);
        assert_eq!(task.resource_requirements.max_memory_mb, None);
    }
}
