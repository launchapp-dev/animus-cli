use super::{push_opt, BulkWorkflowRunItem, MAX_BATCH_SIZE};

pub(super) fn build_bulk_workflow_run_item_args(item: &BulkWorkflowRunItem) -> Vec<String> {
    let mut args = vec![
        "workflow".to_string(),
        "run".to_string(),
        "--task-id".to_string(),
        item.task_id.clone(),
    ];
    push_opt(&mut args, "--workflow-ref", item.workflow_ref.clone());
    push_opt(&mut args, "--input-json", item.input_json.clone());
    args
}

pub(super) fn validate_workflow_run_multiple_input(
    tool_name: &str,
    runs: &[BulkWorkflowRunItem],
) -> Result<(), String> {
    if runs.is_empty() {
        return Err(format!("{tool_name}: runs must not be empty"));
    }
    if runs.len() > MAX_BATCH_SIZE {
        return Err(format!(
            "{tool_name}: runs count {} exceeds maximum {MAX_BATCH_SIZE}",
            runs.len()
        ));
    }
    for (i, item) in runs.iter().enumerate() {
        if item.task_id.trim().is_empty() {
            return Err(format!("{tool_name}: item[{i}].task_id must not be empty"));
        }
    }
    Ok(())
}
