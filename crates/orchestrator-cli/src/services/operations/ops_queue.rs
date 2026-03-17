use std::sync::Arc;

use anyhow::{anyhow, Result};
use orchestrator_core::{load_workflow_config_or_default, services::ServiceHub, workflow_ref_for_task};
use orchestrator_daemon_runtime::{
    drop_subject, enqueue_subject_dispatch, hold_subject, queue_snapshot, queue_stats, release_subject,
    reorder_subjects,
};
use protocol::SubjectDispatch;

use super::ops_workflow::resolve_requirement_workflow_ref;
use crate::{print_ok, print_value, QueueCommand};

#[allow(clippy::too_many_arguments)]
async fn resolve_enqueue_dispatch(
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
    task_id: Option<String>,
    requirement_id: Option<String>,
    title: Option<String>,
    description: Option<String>,
    workflow_ref: Option<String>,
    input: Option<serde_json::Value>,
) -> Result<SubjectDispatch> {
    match (task_id, requirement_id, title) {
        (Some(task_id), None, None) => {
            let task = hub.tasks().get(&task_id).await?;
            let workflow_ref = workflow_ref.unwrap_or_else(|| workflow_ref_for_task(&task));
            Ok(SubjectDispatch::for_task_with_metadata(
                task.id.clone(),
                workflow_ref,
                "manual-queue-enqueue",
                chrono::Utc::now(),
            )
            .with_input(input))
        }
        (None, Some(requirement_id), None) => {
            hub.planning().get_requirement(&requirement_id).await?;
            Ok(SubjectDispatch::for_requirement(
                requirement_id,
                workflow_ref.unwrap_or(resolve_requirement_workflow_ref(project_root)?),
                "manual-queue-enqueue",
            )
            .with_input(input))
        }
        (None, None, Some(title)) => Ok(SubjectDispatch::for_custom(
            title,
            description.unwrap_or_default(),
            workflow_ref.unwrap_or_else(|| {
                load_workflow_config_or_default(std::path::Path::new(project_root)).config.default_workflow_ref
            }),
            input,
            "manual-queue-enqueue",
        )),
        (None, None, None) => Err(anyhow!("one of --task-id, --requirement-id, or --title must be provided")),
        _ => Err(anyhow!("--task-id, --requirement-id, and --title are mutually exclusive")),
    }
}

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
            let input = args.input_json.map(|value| serde_json::from_str(&value)).transpose()?;
            let dispatch = resolve_enqueue_dispatch(
                hub.clone(),
                project_root,
                args.task_id,
                args.requirement_id,
                args.title,
                args.description,
                args.workflow_ref,
                input,
            )
            .await?;
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
            print_value(serde_json::json!({ "held": held, "subject_id": args.subject_id }), true)
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
            print_value(serde_json::json!({ "released": released, "subject_id": args.subject_id }), true)
        }
        QueueCommand::Drop(args) => {
            let removed = drop_subject(project_root, &args.subject_id)?;
            if !json {
                if removed > 0 {
                    print_ok(&format!("dropped {removed} queue entry/entries for {}", args.subject_id), false);
                    return Ok(());
                }
                return Err(anyhow!("queue subject not found"));
            }
            print_value(serde_json::json!({ "dropped": removed, "subject_id": args.subject_id }), true)
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
            print_value(serde_json::json!({ "reordered": reordered, "subject_ids": args.subject_ids }), true)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use orchestrator_core::{
        builtin_agent_runtime_config, builtin_workflow_config, write_agent_runtime_config, write_workflow_config,
        InMemoryServiceHub, RequirementItem, RequirementLinks, RequirementPriority, RequirementStatus,
        WorkflowDefinition, REQUIREMENT_TASK_GENERATION_WORKFLOW_REF,
    };
    use serde_json::json;

    use super::*;

    #[tokio::test]
    async fn resolve_enqueue_dispatch_uses_requirement_workflow_default() {
        let temp = tempfile::tempdir().expect("tempdir");
        let mut workflow_config = builtin_workflow_config();
        workflow_config.workflows.push(WorkflowDefinition {
            id: REQUIREMENT_TASK_GENERATION_WORKFLOW_REF.to_string(),
            name: "Requirement Task Generation".to_string(),
            description: "test workflow".to_string(),
            phases: vec!["requirements".to_string().into()],
            post_success: None,
            variables: Vec::new(),
        });
        write_workflow_config(temp.path(), &workflow_config).expect("write config");
        write_agent_runtime_config(temp.path(), &builtin_agent_runtime_config()).expect("write runtime config");

        let hub = Arc::new(InMemoryServiceHub::new());
        let now = chrono::Utc::now();
        hub.planning()
            .upsert_requirement(RequirementItem {
                id: "REQ-39".to_string(),
                title: "Dispatch requirement".to_string(),
                description: "queue dispatch builder test".to_string(),
                body: None,
                legacy_id: None,
                category: None,
                requirement_type: None,
                acceptance_criteria: vec!["queues workflow".to_string()],
                priority: RequirementPriority::Must,
                status: RequirementStatus::Refined,
                source: "test".to_string(),
                tags: Vec::new(),
                links: RequirementLinks::default(),
                comments: Vec::new(),
                relative_path: None,
                linked_task_ids: Vec::new(),
                created_at: now,
                updated_at: now,
            })
            .await
            .expect("requirement should be created");

        let dispatch = resolve_enqueue_dispatch(
            hub,
            temp.path().to_string_lossy().as_ref(),
            None,
            Some("REQ-39".to_string()),
            None,
            None,
            None,
            Some(json!({"scope":"shared-ingress"})),
        )
        .await
        .expect("dispatch should resolve");

        assert_eq!(dispatch.workflow_ref, REQUIREMENT_TASK_GENERATION_WORKFLOW_REF);
        assert_eq!(dispatch.input, Some(json!({"scope":"shared-ingress"})));
    }
}
