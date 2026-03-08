use std::sync::Arc;

use anyhow::{anyhow, Result};
use orchestrator_core::services::ServiceHub;
use orchestrator_daemon_runtime::{
    enqueue_subject_dispatch, hold_subject, pipeline_for_task, queue_snapshot, queue_stats,
    release_subject, reorder_subjects,
};
use protocol::SubjectDispatch;

use crate::{print_ok, print_value, QueueCommand};

pub(crate) async fn handle_queue(
    command: QueueCommand,
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
    json: bool,
) -> Result<()> {
    match command {
        QueueCommand::List => print_value(queue_snapshot(project_root)?, json),
        QueueCommand::Stats => print_value(queue_stats(project_root)?, json),
        QueueCommand::Enqueue(args) => {
            let task = hub.tasks().get(&args.task_id).await?;
            let pipeline_id = args
                .pipeline_id
                .unwrap_or_else(|| pipeline_for_task(&task));
            let input = args
                .input_json
                .map(|value| serde_json::from_str(&value))
                .transpose()?;
            let dispatch = SubjectDispatch::for_task_with_metadata(
                task.id.clone(),
                pipeline_id,
                "manual-queue-enqueue",
                chrono::Utc::now(),
            )
            .with_input(input);
            let result = enqueue_subject_dispatch(project_root, dispatch)?;
            if !json {
                if result.enqueued {
                    print_ok("subject dispatch enqueued", false);
                    return Ok(());
                }
                print_ok("subject dispatch already queued", false);
                return Ok(());
            }
            print_value(result, true)
        }
        QueueCommand::Hold(args) => {
            let held = hold_subject(project_root, &args.subject_id)?;
            if !json {
                if held {
                    print_ok("queue subject held", false);
                    return Ok(());
                }
                return Err(anyhow!("queue subject not found or not pending"));
            }
            print_value(
                serde_json::json!({ "held": held, "subject_id": args.subject_id }),
                true,
            )
        }
        QueueCommand::Release(args) => {
            let released = release_subject(project_root, &args.subject_id)?;
            if !json {
                if released {
                    print_ok("queue subject released", false);
                    return Ok(());
                }
                return Err(anyhow!("queue subject not found or not held"));
            }
            print_value(
                serde_json::json!({ "released": released, "subject_id": args.subject_id }),
                true,
            )
        }
        QueueCommand::Reorder(args) => {
            let reordered = reorder_subjects(project_root, args.subject_ids.clone())?;
            if !json {
                if reordered {
                    print_ok("queue reordered", false);
                    return Ok(());
                }
                print_ok("queue order unchanged", false);
                return Ok(());
            }
            print_value(
                serde_json::json!({ "reordered": reordered, "subject_ids": args.subject_ids }),
                true,
            )
        }
    }
}
