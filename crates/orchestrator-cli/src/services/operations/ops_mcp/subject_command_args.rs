use super::{
    push_opt, SubjectBatchCreateItem, SubjectBatchUpdateItem, SubjectCreateInput, SubjectGetInput, SubjectListInput,
    SubjectNextInput, SubjectStatusInput, SubjectUpdateInput, MAX_BATCH_SIZE,
};

pub(super) fn build_subject_list_args(input: &SubjectListInput) -> Vec<String> {
    let mut args = vec!["subject".to_string(), "list".to_string(), "--kind".to_string(), input.kind.clone()];
    push_opt(&mut args, "--status", input.status.clone());
    if let Some(limit) = input.limit {
        args.push("--limit".to_string());
        args.push(limit.to_string());
    }
    push_opt(&mut args, "--cursor", input.cursor.clone());
    push_opt(&mut args, "--query", input.query.clone());
    args
}

pub(super) fn build_subject_get_args(input: &SubjectGetInput) -> Vec<String> {
    vec![
        "subject".to_string(),
        "get".to_string(),
        "--kind".to_string(),
        input.kind.clone(),
        "--id".to_string(),
        input.id.clone(),
    ]
}

pub(super) fn build_subject_create_args(input: &SubjectCreateInput) -> Vec<String> {
    let mut args = vec![
        "subject".to_string(),
        "create".to_string(),
        "--kind".to_string(),
        input.kind.clone(),
        "--title".to_string(),
        input.title.clone(),
    ];
    push_opt(&mut args, "--status", input.status.clone());
    push_opt(&mut args, "--priority", input.priority.clone());
    if !input.labels.is_empty() {
        args.push("--labels".to_string());
        args.push(input.labels.join(","));
    }
    push_opt(&mut args, "--body", input.body.clone());
    push_opt(&mut args, "--data", input.data.as_ref().map(|v| v.to_string()));
    args
}

pub(super) fn build_subject_update_args(input: &SubjectUpdateInput) -> Vec<String> {
    let mut args = vec![
        "subject".to_string(),
        "update".to_string(),
        "--kind".to_string(),
        input.kind.clone(),
        "--id".to_string(),
        input.id.clone(),
    ];
    push_opt(&mut args, "--title", input.title.clone());
    push_opt(&mut args, "--status", input.status.clone());
    push_opt(&mut args, "--priority", input.priority.clone());
    if !input.labels.is_empty() {
        args.push("--labels".to_string());
        args.push(input.labels.join(","));
    }
    push_opt(&mut args, "--body", input.body.clone());
    push_opt(&mut args, "--data", input.data.as_ref().map(|v| v.to_string()));
    args
}

pub(super) fn build_subject_batch_create_item_args(kind: &str, item: &SubjectBatchCreateItem) -> Vec<String> {
    build_subject_create_args(&SubjectCreateInput {
        kind: kind.to_string(),
        title: item.title.clone(),
        status: item.status.clone(),
        priority: item.priority.clone(),
        labels: item.labels.clone(),
        body: item.body.clone(),
        data: item.data.clone(),
        project_root: None,
    })
}

pub(super) fn build_subject_batch_update_item_args(kind: &str, item: &SubjectBatchUpdateItem) -> Vec<String> {
    build_subject_update_args(&SubjectUpdateInput {
        kind: kind.to_string(),
        id: item.id.clone(),
        // batch-update doesn't carry title/body fields; single-update only.
        title: None,
        status: item.status.clone(),
        priority: item.priority.clone(),
        labels: item.labels.clone(),
        body: None,
        data: item.data.clone(),
        project_root: None,
    })
}

fn validate_batch_shape<T>(tool_name: &str, kind: &str, items: &[T]) -> Result<(), String> {
    if kind.trim().is_empty() {
        return Err(format!("{tool_name}: kind must not be empty"));
    }
    if items.is_empty() {
        return Err(format!("{tool_name}: items must not be empty"));
    }
    if items.len() > MAX_BATCH_SIZE {
        return Err(format!("{tool_name}: items count {} exceeds maximum {MAX_BATCH_SIZE}", items.len()));
    }
    Ok(())
}

pub(super) fn validate_subject_batch_create_input(
    tool_name: &str,
    kind: &str,
    items: &[SubjectBatchCreateItem],
) -> Result<(), String> {
    validate_batch_shape(tool_name, kind, items)?;
    for (i, item) in items.iter().enumerate() {
        if item.title.trim().is_empty() {
            return Err(format!("{tool_name}: item[{i}].title must not be empty"));
        }
    }
    Ok(())
}

pub(super) fn validate_subject_batch_update_input(
    tool_name: &str,
    kind: &str,
    items: &[SubjectBatchUpdateItem],
) -> Result<(), String> {
    validate_batch_shape(tool_name, kind, items)?;
    for (i, item) in items.iter().enumerate() {
        if item.id.trim().is_empty() {
            return Err(format!("{tool_name}: item[{i}].id must not be empty"));
        }
        if item.status.is_none() && item.priority.is_none() && item.labels.is_empty() && item.data.is_none() {
            return Err(format!("{tool_name}: item[{i}] requires at least one of status / priority / labels / data"));
        }
    }
    Ok(())
}

pub(super) fn build_subject_next_args(input: &SubjectNextInput) -> Vec<String> {
    vec!["subject".to_string(), "next".to_string(), "--kind".to_string(), input.kind.clone()]
}

pub(super) fn build_subject_status_args(input: &SubjectStatusInput) -> Vec<String> {
    vec![
        "subject".to_string(),
        "status".to_string(),
        "--kind".to_string(),
        input.kind.clone(),
        "--id".to_string(),
        input.id.clone(),
        "--status".to_string(),
        input.status.clone(),
    ]
}
