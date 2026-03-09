use super::{push_opt, QueueEnqueueInput, QueueReorderInput};

pub(super) fn build_queue_enqueue_args(input: &QueueEnqueueInput) -> Vec<String> {
    let mut args = vec!["queue".to_string(), "enqueue".to_string()];
    push_opt(&mut args, "--task-id", input.task_id.clone());
    push_opt(&mut args, "--requirement-id", input.requirement_id.clone());
    push_opt(&mut args, "--title", input.title.clone());
    push_opt(&mut args, "--description", input.description.clone());
    push_opt(&mut args, "--workflow-ref", input.workflow_ref.clone());
    push_opt(&mut args, "--input-json", input.input_json.clone());
    args
}

pub(super) fn build_queue_reorder_args(input: &QueueReorderInput) -> Vec<String> {
    let mut args = vec!["queue".to_string(), "reorder".to_string()];
    for subject_id in &input.subject_ids {
        args.push("--subject-id".to_string());
        args.push(subject_id.clone());
    }
    args
}
