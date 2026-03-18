use super::{
    push_opt, BulkTaskStatusItem, BulkTaskUpdateItem, TaskCreateInput, TaskListInput, TaskPrioritizedInput,
    MAX_BATCH_SIZE,
};

pub(super) fn build_task_list_args(input: &TaskListInput) -> Vec<String> {
    let mut args = vec!["task".to_string(), "list".to_string()];
    push_opt(&mut args, "--task-type", input.task_type.clone());
    push_opt(&mut args, "--status", input.status.clone());
    push_opt(&mut args, "--priority", input.priority.clone());
    push_opt(&mut args, "--risk", input.risk.clone());
    push_opt(&mut args, "--assignee-type", input.assignee_type.clone());
    for tag in &input.tag {
        args.push("--tag".to_string());
        args.push(tag.clone());
    }
    push_opt(&mut args, "--linked-requirement", input.linked_requirement.clone());
    push_opt(&mut args, "--linked-architecture-entity", input.linked_architecture_entity.clone());
    push_opt(&mut args, "--search", input.search.clone());
    push_opt(&mut args, "--sort", input.sort.clone());
    args
}

pub(super) fn build_task_prioritized_args(input: &TaskPrioritizedInput) -> Vec<String> {
    let mut args = vec!["task".to_string(), "prioritized".to_string()];
    push_opt(&mut args, "--status", input.status.clone());
    push_opt(&mut args, "--priority", input.priority.clone());
    push_opt(&mut args, "--assignee-type", input.assignee_type.clone());
    push_opt(&mut args, "--search", input.search.clone());
    args
}

pub(super) fn build_task_create_args(input: &TaskCreateInput) -> Vec<String> {
    let mut args = vec![
        "task".to_string(),
        "create".to_string(),
        "--title".to_string(),
        input.title.clone(),
        "--description".to_string(),
        input.description.clone().unwrap_or_default(),
    ];
    push_opt(&mut args, "--task-type", input.task_type.clone());
    push_opt(&mut args, "--priority", input.priority.clone());
    for requirement_id in &input.linked_requirement {
        args.push("--linked-requirement".to_string());
        args.push(requirement_id.clone());
    }
    for entity_id in &input.linked_architecture_entity {
        args.push("--linked-architecture-entity".to_string());
        args.push(entity_id.clone());
    }
    args
}

pub(super) fn build_task_get_args(id: String) -> Vec<String> {
    vec!["task".to_string(), "get".to_string(), "--id".to_string(), id]
}

pub(super) fn build_task_delete_args(id: String, confirm: Option<String>, dry_run: bool) -> Vec<String> {
    let mut args = vec!["task".to_string(), "delete".to_string(), "--id".to_string(), id];
    if let Some(confirm) = confirm {
        args.push("--confirm".to_string());
        args.push(confirm);
    }
    if dry_run {
        args.push("--dry-run".to_string());
    }
    args
}

pub(super) fn build_task_control_args(action: &str, id: String) -> Vec<String> {
    vec!["task".to_string(), action.to_string(), "--id".to_string(), id]
}

pub(super) fn build_bulk_status_item_args(item: &BulkTaskStatusItem) -> Vec<String> {
    vec![
        "task".to_string(),
        "status".to_string(),
        "--id".to_string(),
        item.id.clone(),
        "--status".to_string(),
        item.status.clone(),
    ]
}

pub(super) fn build_bulk_update_item_args(item: &BulkTaskUpdateItem) -> Vec<String> {
    let mut args = vec!["task".to_string(), "update".to_string(), "--id".to_string(), item.id.clone()];
    push_opt(&mut args, "--title", item.title.clone());
    push_opt(&mut args, "--description", item.description.clone());
    push_opt(&mut args, "--priority", item.priority.clone());
    push_opt(&mut args, "--status", item.status.clone());
    push_opt(&mut args, "--assignee", item.assignee.clone());
    push_opt(&mut args, "--input-json", item.input_json.clone());
    args
}

pub(super) fn validate_bulk_status_input(tool_name: &str, updates: &[BulkTaskStatusItem]) -> Result<(), String> {
    if updates.is_empty() {
        return Err(format!("{tool_name}: updates must not be empty"));
    }
    if updates.len() > MAX_BATCH_SIZE {
        return Err(format!("{tool_name}: updates count {} exceeds maximum {MAX_BATCH_SIZE}", updates.len()));
    }
    let mut seen_ids = std::collections::HashSet::new();
    for (i, item) in updates.iter().enumerate() {
        if item.id.trim().is_empty() {
            return Err(format!("{tool_name}: item[{i}].id must not be empty"));
        }
        if item.status.trim().is_empty() {
            return Err(format!("{tool_name}: item[{i}].status must not be empty"));
        }
        if !seen_ids.insert(item.id.as_str()) {
            return Err(format!("{tool_name}: duplicate id '{}' at index {i}", item.id));
        }
    }
    Ok(())
}

pub(super) fn validate_bulk_update_input(tool_name: &str, updates: &[BulkTaskUpdateItem]) -> Result<(), String> {
    if updates.is_empty() {
        return Err(format!("{tool_name}: updates must not be empty"));
    }
    if updates.len() > MAX_BATCH_SIZE {
        return Err(format!("{tool_name}: updates count {} exceeds maximum {MAX_BATCH_SIZE}", updates.len()));
    }
    let mut seen_ids = std::collections::HashSet::new();
    for (i, item) in updates.iter().enumerate() {
        if item.id.trim().is_empty() {
            return Err(format!("{tool_name}: item[{i}].id must not be empty"));
        }
        let has_update = item.title.is_some()
            || item.description.is_some()
            || item.priority.is_some()
            || item.status.is_some()
            || item.assignee.is_some()
            || item.input_json.is_some();
        if !has_update {
            return Err(format!("{tool_name}: item[{i}] (id='{}') must include at least one update field", item.id));
        }
        if !seen_ids.insert(item.id.as_str()) {
            return Err(format!("{tool_name}: duplicate id '{}' at index {i}", item.id));
        }
    }
    Ok(())
}
