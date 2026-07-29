use super::{SubjectBatchCreateItem, SubjectBatchUpdateItem, MAX_BATCH_SIZE};

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
