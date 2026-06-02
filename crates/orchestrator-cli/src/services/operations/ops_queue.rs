mod control_routing;

pub(crate) use control_routing::build_queue_routing;

use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use orchestrator_core::{load_workflow_config_or_default, services::ServiceHub, workflow_ref_for_task};
use protocol::{SubjectDispatch, SubjectDispatchExt};

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
        (None, None, None) => Err(anyhow!(
            "no subject specified. Use --task-id TASK_ID for existing tasks, --requirement-id REQ_ID for requirements, or --title \"name\" for custom dispatches."
        )),
        _ => Err(anyhow!(
            "--task-id, --requirement-id, and --title are mutually exclusive - provide only one subject selector."
        )),
    }
}

fn queue_plugin_required(operation: &str) -> anyhow::Error {
    anyhow!(
        "no queue plugin installed - `animus queue {operation}` requires the `queue` plugin role. \
         Run `animus plugin install-defaults` (or install `launchapp-dev/animus-queue-default`) and retry."
    )
}

pub(crate) async fn handle_queue(
    command: QueueCommand,
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
    json: bool,
) -> Result<()> {
    let project_root_path = std::path::Path::new(project_root);
    match command {
        QueueCommand::List => {
            let plugin_list_req = animus_queue_protocol::QueueListRequest::default();
            let response = crate::services::plugin_clients::call_queue_list(project_root_path, &plugin_list_req)
                .await?
                .ok_or_else(|| queue_plugin_required("list"))?;
            print_value(response, json)
        }
        QueueCommand::Stats => {
            let stats = crate::services::plugin_clients::call_queue_stats(project_root_path)
                .await?
                .ok_or_else(|| queue_plugin_required("stats"))?;
            print_value(stats, json)
        }
        QueueCommand::Enqueue(args) => {
            let input = args.input_json.clone().map(|value| serde_json::from_str(&value)).transpose()?;
            let dispatch = resolve_enqueue_dispatch(
                hub.clone(),
                project_root,
                args.task_id.clone(),
                args.requirement_id.clone(),
                args.title.clone(),
                args.description.clone(),
                args.workflow_ref.clone(),
                input,
            )
            .await?;

            let dispatch_value =
                serde_json::to_value(&dispatch).context("encoding subject_dispatch for queue plugin")?;
            let plugin_dispatch = serde_json::from_value(dispatch_value)
                .context("subject_dispatch shape drift vs animus_subject_protocol v0.5")?;
            let plugin_request = animus_queue_protocol::QueueEnqueueRequest { subject_dispatch: plugin_dispatch };
            let plugin_response =
                crate::services::plugin_clients::call_queue_enqueue(project_root_path, &plugin_request)
                    .await?
                    .ok_or_else(|| queue_plugin_required("enqueue"))?;
            let translated = serde_json::json!({
                "enqueued": plugin_response.enqueued,
                "entry_id": plugin_response.entry_id,
                "subject_id": plugin_response.subject_id,
                "via": "plugin_host",
            });
            if !json {
                if plugin_response.enqueued {
                    print_ok("subject dispatch enqueued (via queue plugin)", false);
                    return Ok(());
                }
                print_ok("subject dispatch already queued (via queue plugin)", false);
                return Ok(());
            }
            print_value(translated, true)
        }
        QueueCommand::Hold(args) => {
            let result = try_queue_hold_via_plugin(project_root, &args.subject_id)
                .await?
                .ok_or_else(|| queue_plugin_required("hold"))?;
            let held = result.changed;
            if !json {
                if held {
                    print_ok("queue subject held (via queue plugin)", false);
                    return Ok(());
                }
                return Err(anyhow!("queue subject not found or not pending"));
            }
            print_value(serde_json::json!({ "held": held, "subject_id": args.subject_id, "via": "plugin_host" }), true)
        }
        QueueCommand::Release(args) => {
            let result = try_queue_release_via_plugin(project_root, &args.subject_id)
                .await?
                .ok_or_else(|| queue_plugin_required("release"))?;
            let released = result.changed;
            if !json {
                if released {
                    print_ok("queue subject released (via queue plugin)", false);
                    return Ok(());
                }
                return Err(anyhow!("queue subject not found or not held"));
            }
            print_value(
                serde_json::json!({ "released": released, "subject_id": args.subject_id, "via": "plugin_host" }),
                true,
            )
        }
        QueueCommand::Drop(args) => {
            let removed = try_queue_drop_via_plugin(project_root, &args.subject_id)
                .await?
                .ok_or_else(|| queue_plugin_required("drop"))?;
            if !json {
                if removed > 0 {
                    print_ok(
                        &format!("dropped {removed} queue entry/entries for {} (via queue plugin)", args.subject_id),
                        false,
                    );
                    return Ok(());
                }
                return Err(anyhow!("queue subject not found"));
            }
            print_value(
                serde_json::json!({ "dropped": removed, "subject_id": args.subject_id, "via": "plugin_host" }),
                true,
            )
        }
        QueueCommand::Reorder(args) => {
            let reordered_count = try_queue_reorder_via_plugin(project_root, &args.subject_ids)
                .await?
                .ok_or_else(|| queue_plugin_required("reorder"))?;
            let reordered = reordered_count > 0;
            if !json {
                if reordered {
                    print_ok("queue reordered (via queue plugin)", false);
                    return Ok(());
                }
                print_ok("queue order unchanged", false);
                return Ok(());
            }
            print_value(
                serde_json::json!({
                    "reordered": reordered,
                    "reordered_count": reordered_count,
                    "subject_ids": args.subject_ids,
                    "via": "plugin_host",
                }),
                true,
            )
        }
    }
}

async fn lookup_plugin_entries_by_subject(
    project_root: &str,
    subject_id: &str,
    statuses: &[&'static str],
) -> Result<Option<Vec<animus_queue_protocol::QueueEntry>>> {
    let req = animus_queue_protocol::QueueListRequest {
        status: statuses.iter().map(|s| (*s).to_string()).collect(),
        limit: None,
        offset: None,
    };
    let Some(response) =
        crate::services::plugin_clients::call_queue_list(std::path::Path::new(project_root), &req).await?
    else {
        return Ok(None);
    };
    let matched = response.entries.into_iter().filter(|entry| entry.subject_id == subject_id).collect();
    Ok(Some(matched))
}

async fn try_queue_hold_via_plugin(
    project_root: &str,
    subject_id: &str,
) -> Result<Option<animus_queue_protocol::QueueMutationResponse>> {
    let Some(entries) =
        lookup_plugin_entries_by_subject(project_root, subject_id, &[animus_queue_protocol::status::PENDING]).await?
    else {
        return Ok(None);
    };
    if entries.is_empty() {
        return Ok(Some(animus_queue_protocol::QueueMutationResponse { changed: false, not_found: true }));
    }
    let mut changed = false;
    let mut not_found = true;
    for entry in entries {
        let req = animus_queue_protocol::QueueHoldRequest { entry_id: entry.entry_id, reason: None };
        if let Some(resp) =
            crate::services::plugin_clients::call_queue_hold(std::path::Path::new(project_root), &req).await?
        {
            changed |= resp.changed;
            not_found &= resp.not_found;
        }
    }
    Ok(Some(animus_queue_protocol::QueueMutationResponse { changed, not_found }))
}

async fn try_queue_release_via_plugin(
    project_root: &str,
    subject_id: &str,
) -> Result<Option<animus_queue_protocol::QueueMutationResponse>> {
    let Some(entries) =
        lookup_plugin_entries_by_subject(project_root, subject_id, &[animus_queue_protocol::status::HELD]).await?
    else {
        return Ok(None);
    };
    if entries.is_empty() {
        return Ok(Some(animus_queue_protocol::QueueMutationResponse { changed: false, not_found: true }));
    }
    let mut changed = false;
    let mut not_found = true;
    for entry in entries {
        let req = animus_queue_protocol::QueueReleaseRequest { entry_id: entry.entry_id };
        if let Some(resp) =
            crate::services::plugin_clients::call_queue_release(std::path::Path::new(project_root), &req).await?
        {
            changed |= resp.changed;
            not_found &= resp.not_found;
        }
    }
    Ok(Some(animus_queue_protocol::QueueMutationResponse { changed, not_found }))
}

async fn try_queue_drop_via_plugin(project_root: &str, subject_id: &str) -> Result<Option<usize>> {
    let Some(entries) = lookup_plugin_entries_by_subject(
        project_root,
        subject_id,
        &[
            animus_queue_protocol::status::PENDING,
            animus_queue_protocol::status::HELD,
            animus_queue_protocol::status::ASSIGNED,
        ],
    )
    .await?
    else {
        return Ok(None);
    };
    let mut dropped = 0usize;
    for entry in entries {
        let req = animus_queue_protocol::QueueDropRequest { entry_id: entry.entry_id };
        if let Some(resp) =
            crate::services::plugin_clients::call_queue_drop(std::path::Path::new(project_root), &req).await?
        {
            if resp.changed {
                dropped += 1;
            }
        }
    }
    Ok(Some(dropped))
}

async fn try_queue_reorder_via_plugin(project_root: &str, subject_ids: &[String]) -> Result<Option<usize>> {
    let Some(entries) = lookup_plugin_entries_by_subject_set(
        project_root,
        subject_ids,
        &[animus_queue_protocol::status::PENDING, animus_queue_protocol::status::HELD],
    )
    .await?
    else {
        return Ok(None);
    };
    if entries.is_empty() {
        return Ok(Some(0));
    }
    let req = animus_queue_protocol::QueueReorderRequest { entry_ids: entries };
    let Some(resp) =
        crate::services::plugin_clients::call_queue_reorder(std::path::Path::new(project_root), &req).await?
    else {
        return Ok(None);
    };
    Ok(Some(resp.reordered_count))
}

async fn lookup_plugin_entries_by_subject_set(
    project_root: &str,
    subject_ids: &[String],
    statuses: &[&'static str],
) -> Result<Option<Vec<String>>> {
    let req = animus_queue_protocol::QueueListRequest {
        status: statuses.iter().map(|s| (*s).to_string()).collect(),
        limit: None,
        offset: None,
    };
    let Some(response) =
        crate::services::plugin_clients::call_queue_list(std::path::Path::new(project_root), &req).await?
    else {
        return Ok(None);
    };
    let mut by_subject: std::collections::HashMap<&str, Vec<String>> = std::collections::HashMap::new();
    for entry in &response.entries {
        by_subject.entry(entry.subject_id.as_str()).or_default().push(entry.entry_id.clone());
    }
    let mut entry_ids: Vec<String> = Vec::new();
    for subject_id in subject_ids {
        if let Some(ids) = by_subject.remove(subject_id.as_str()) {
            entry_ids.extend(ids);
        }
    }
    Ok(Some(entry_ids))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use orchestrator_core::{
        builtin_agent_runtime_config, builtin_workflow_config, write_agent_runtime_config, write_workflow_config,
        InMemoryServiceHub,
    };

    use super::*;

    #[tokio::test]
    async fn resolve_enqueue_dispatch_missing_subject_shows_actionable_error() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workflow_config = builtin_workflow_config();
        write_workflow_config(temp.path(), &workflow_config).expect("write config");
        write_agent_runtime_config(temp.path(), &builtin_agent_runtime_config()).expect("write runtime config");

        let hub = Arc::new(InMemoryServiceHub::new());
        let err =
            resolve_enqueue_dispatch(hub, temp.path().to_string_lossy().as_ref(), None, None, None, None, None, None)
                .await
                .expect_err("missing subject should fail");

        let msg = err.to_string();
        assert!(msg.contains("--task-id"), "error should mention --task-id");
        assert!(msg.contains("--requirement-id"), "error should mention --requirement-id");
        assert!(msg.contains("--title"), "error should mention --title");
        assert!(msg.contains("custom dispatches"), "error should suggest custom dispatches");
    }

    #[tokio::test]
    async fn resolve_enqueue_dispatch_multiple_subjects_shows_mutual_exclusivity_error() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workflow_config = builtin_workflow_config();
        write_workflow_config(temp.path(), &workflow_config).expect("write config");
        write_agent_runtime_config(temp.path(), &builtin_agent_runtime_config()).expect("write runtime config");

        let hub = Arc::new(InMemoryServiceHub::new());
        let err = resolve_enqueue_dispatch(
            hub,
            temp.path().to_string_lossy().as_ref(),
            Some("TASK-1".to_string()),
            Some("REQ-1".to_string()),
            None,
            None,
            None,
            None,
        )
        .await
        .expect_err("multiple subjects should fail");

        let msg = err.to_string();
        assert!(msg.contains("mutually exclusive"), "error should mention mutual exclusivity");
    }
}
