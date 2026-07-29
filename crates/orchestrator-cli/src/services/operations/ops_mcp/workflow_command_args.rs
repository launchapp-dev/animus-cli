use super::{BulkWorkflowRunItem, MAX_BATCH_SIZE};

pub(super) fn validate_workflow_run_multiple_input(
    tool_name: &str,
    runs: &[BulkWorkflowRunItem],
) -> Result<(), String> {
    if runs.is_empty() {
        return Err(format!("{tool_name}: runs must not be empty"));
    }
    if runs.len() > MAX_BATCH_SIZE {
        return Err(format!("{tool_name}: runs count {} exceeds maximum {MAX_BATCH_SIZE}", runs.len()));
    }
    for (i, item) in runs.iter().enumerate() {
        if item.subject_id.trim().is_empty() {
            return Err(format!("{tool_name}: item[{i}].subject_id must not be empty"));
        }
    }
    Ok(())
}
