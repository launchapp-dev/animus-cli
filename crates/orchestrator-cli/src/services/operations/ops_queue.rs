mod control_routing;

pub(crate) use control_routing::build_queue_routing;

use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use orchestrator_core::{load_workflow_config_or_default, services::ServiceHub, workflow_ref_for_task};
use protocol::{SubjectDispatch, SubjectDispatchExt};

use super::ops_workflow::resolve_requirement_workflow_ref;
use crate::{invalid_input_error, print_ok, print_value, CliError, CliErrorKind, QueueCommand, QueueSubjectArgs};

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
    // Same human-readable text as the previous bare `anyhow!` constructor
    // (which classified as Internal via the message fallback) — this only
    // adds the structured remediation payload for machine callers.
    crate::error_with_remediation(
        CliErrorKind::Internal,
        format!(
            "no queue plugin installed - `animus queue {operation}` requires the `queue` plugin role. \
             Run `animus plugin install-defaults` (or install `launchapp-dev/animus-queue-default`) and retry."
        ),
        crate::missing_plugin_remediation(
            "animus plugin install-defaults",
            "Install the queue plugin (launchapp-dev/animus-queue-default), then retry.",
        ),
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
            // Wake a running daemon so the freshly enqueued entry drains
            // now instead of on the next heartbeat. Fire-and-forget.
            if plugin_response.enqueued {
                orchestrator_daemon_runtime::control::nudge_daemon_scheduler_best_effort(project_root_path).await;
            }
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
        QueueCommand::Hold(args) => handle_queue_bulk(BulkVerb::Hold, args, project_root, json).await,
        QueueCommand::Release(args) => handle_queue_bulk(BulkVerb::Release, args, project_root, json).await,
        QueueCommand::Drop(args) => handle_queue_bulk(BulkVerb::Drop, args, project_root, json).await,
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

#[derive(Debug, Clone, Copy)]
enum BulkVerb {
    Hold,
    Release,
    Drop,
}

impl BulkVerb {
    const fn name(self) -> &'static str {
        match self {
            Self::Hold => "hold",
            Self::Release => "release",
            Self::Drop => "drop",
        }
    }

    const fn past_tense(self) -> &'static str {
        match self {
            Self::Hold => "held",
            Self::Release => "released",
            Self::Drop => "dropped",
        }
    }

    /// Queue entry statuses this verb can act on (mirrors the single-item
    /// `try_queue_*_via_plugin` status sets).
    const fn statuses(self) -> &'static [&'static str] {
        match self {
            Self::Hold => &[animus_queue_protocol::status::PENDING],
            Self::Release => &[animus_queue_protocol::status::HELD],
            Self::Drop => &[
                animus_queue_protocol::status::PENDING,
                animus_queue_protocol::status::HELD,
                animus_queue_protocol::status::ASSIGNED,
            ],
        }
    }

    const fn state_error(self) -> &'static str {
        match self {
            Self::Hold => "queue subject not found or not pending",
            Self::Release => "queue subject not found or not held",
            Self::Drop => "queue subject not found",
        }
    }
}

#[derive(Debug, serde::Serialize)]
struct BulkItemResult {
    subject_id: String,
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    dropped_entries: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

fn merge_subject_ids(flag: Option<String>, positional: Vec<String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    flag.into_iter().chain(positional).filter(|id| seen.insert(id.clone())).collect()
}

fn distinct_in_order(ids: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    ids.into_iter().filter(|id| seen.insert(id.clone())).collect()
}

fn bulk_failure_kind(items: &[BulkItemResult]) -> CliErrorKind {
    let all_not_found =
        items.iter().filter(|item| !item.ok).all(|item| item.error.as_deref().is_some_and(|e| e.contains("not found")));
    if all_not_found {
        CliErrorKind::NotFound
    } else {
        CliErrorKind::Internal
    }
}

fn bulk_payload(verb: BulkVerb, all: bool, items: &[BulkItemResult]) -> Result<serde_json::Value> {
    let succeeded = items.iter().filter(|item| item.ok).count();
    Ok(serde_json::json!({
        "op": verb.name(),
        "all": all,
        "requested": items.len(),
        "succeeded": succeeded,
        "failed": items.len() - succeeded,
        "items": serde_json::to_value(items)?,
        "via": "plugin_host",
    }))
}

fn confirm_bulk_all(verb: BulkVerb, count: usize) -> Result<bool> {
    use std::io::{BufRead, IsTerminal, Write};
    if !std::io::stdin().is_terminal() {
        return Err(invalid_input_error(format!(
            "--all requires --yes in non-interactive mode (would {} {count} queue subject(s))",
            verb.name()
        )));
    }
    eprint!("{} {count} queue subject(s)? [y/N] ", verb.name());
    std::io::stderr().flush().ok();
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line).context("failed to read confirmation")?;
    let answer = line.trim().to_ascii_lowercase();
    Ok(answer == "y" || answer == "yes")
}

async fn resolve_all_subject_ids(verb: BulkVerb, project_root: &str) -> Result<Vec<String>> {
    let req = animus_queue_protocol::QueueListRequest {
        status: verb.statuses().iter().map(|s| (*s).to_string()).collect(),
        limit: None,
        offset: None,
    };
    let response = crate::services::plugin_clients::call_queue_list(std::path::Path::new(project_root), &req)
        .await?
        .ok_or_else(|| queue_plugin_required(verb.name()))?;
    Ok(distinct_in_order(response.entries.into_iter().map(|entry| entry.subject_id)))
}

async fn apply_bulk_verb(verb: BulkVerb, project_root: &str, subject_id: &str) -> Result<BulkItemResult> {
    let mut item = BulkItemResult { subject_id: subject_id.to_string(), ok: false, dropped_entries: None, error: None };
    match verb {
        BulkVerb::Hold | BulkVerb::Release => {
            let result = match verb {
                BulkVerb::Hold => try_queue_hold_via_plugin(project_root, subject_id).await,
                _ => try_queue_release_via_plugin(project_root, subject_id).await,
            };
            match result {
                Ok(Some(resp)) if resp.changed => item.ok = true,
                Ok(Some(_)) => item.error = Some(verb.state_error().to_string()),
                Ok(None) => return Err(queue_plugin_required(verb.name())),
                Err(err) => item.error = Some(format!("{err:#}")),
            }
        }
        BulkVerb::Drop => match try_queue_drop_via_plugin(project_root, subject_id).await {
            Ok(Some(removed)) if removed > 0 => {
                item.ok = true;
                item.dropped_entries = Some(removed);
            }
            Ok(Some(_)) => item.error = Some(verb.state_error().to_string()),
            Ok(None) => return Err(queue_plugin_required(verb.name())),
            Err(err) => item.error = Some(format!("{err:#}")),
        },
    }
    Ok(item)
}

async fn handle_queue_bulk(verb: BulkVerb, args: QueueSubjectArgs, project_root: &str, json: bool) -> Result<()> {
    if args.yes && !args.all {
        return Err(invalid_input_error("--yes is only valid together with --all"));
    }
    let subject_ids = if args.all {
        let ids = resolve_all_subject_ids(verb, project_root).await?;
        if ids.is_empty() {
            if json {
                return print_value(bulk_payload(verb, true, &[])?, true);
            }
            print_ok(&format!("no queue subjects eligible for {} (via queue plugin)", verb.name()), false);
            return Ok(());
        }
        if !args.yes && !confirm_bulk_all(verb, ids.len())? {
            print_ok(&format!("aborted: no queue subjects {}", verb.past_tense()), json);
            return Ok(());
        }
        ids
    } else {
        merge_subject_ids(args.subject_id, args.subject_ids)
    };

    let mut items = Vec::with_capacity(subject_ids.len());
    for subject_id in &subject_ids {
        items.push(apply_bulk_verb(verb, project_root, subject_id).await?);
    }

    // Released entries are immediately dispatchable — wake a running
    // daemon so they drain now instead of on the next heartbeat. One
    // nudge per bulk command, only when something actually changed.
    if matches!(verb, BulkVerb::Release) && items.iter().any(|item| item.ok) {
        orchestrator_daemon_runtime::control::nudge_daemon_scheduler_best_effort(std::path::Path::new(project_root))
            .await;
    }

    let failed: Vec<&BulkItemResult> = items.iter().filter(|item| !item.ok).collect();
    if !json {
        for item in &items {
            match &item.error {
                None => match item.dropped_entries {
                    Some(removed) => {
                        println!("dropped {removed} queue entry/entries for {} (via queue plugin)", item.subject_id)
                    }
                    None => println!("{} {} (via queue plugin)", verb.past_tense(), item.subject_id),
                },
                Some(error) => println!("failed {}: {error}", item.subject_id),
            }
        }
    }
    if failed.is_empty() {
        if json {
            return print_value(bulk_payload(verb, args.all, &items)?, true);
        }
        if items.len() > 1 {
            print_ok(&format!("{} {} queue subject(s) (via queue plugin)", verb.past_tense(), items.len()), false);
        }
        return Ok(());
    }
    let kind = bulk_failure_kind(&items);
    let message = format!("queue {}: {} of {} subject(s) failed", verb.name(), failed.len(), items.len());
    Err(CliError::new(kind, message).with_details(bulk_payload(verb, args.all, &items)?).into())
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

    #[test]
    fn queue_plugin_required_keeps_message_and_kind_but_adds_structured_remediation() {
        let err = queue_plugin_required("list");
        let message = err.to_string();
        assert!(message.contains("no queue plugin installed"), "human text preserved: {message}");
        assert!(message.contains("animus plugin install-defaults"), "human text preserved: {message}");
        assert_eq!(crate::classify_cli_error_kind(&err), CliErrorKind::Internal, "kind unchanged");
        let details = crate::extract_cli_error_details(&err).expect("structured remediation details");
        assert_eq!(details.pointer("/remediation/kind").and_then(serde_json::Value::as_str), Some("missing_plugin"));
        assert_eq!(
            details.pointer("/remediation/install_command").and_then(serde_json::Value::as_str),
            Some("animus plugin install-defaults")
        );
    }

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

    #[tokio::test]
    async fn handle_queue_bulk_rejects_yes_without_all() {
        let args =
            QueueSubjectArgs { subject_ids: vec!["TASK-1".to_string()], subject_id: None, all: false, yes: true };
        let err = handle_queue_bulk(BulkVerb::Hold, args, "/nonexistent", false)
            .await
            .expect_err("--yes without --all should be rejected");
        assert!(err.to_string().contains("--all"), "error should mention --all: {err}");
        assert_eq!(crate::classify_cli_error_kind(&err), CliErrorKind::InvalidInput);
    }

    #[test]
    fn merge_subject_ids_combines_flag_and_positional_and_dedups() {
        let merged = merge_subject_ids(
            Some("TASK-1".to_string()),
            vec!["TASK-2".to_string(), "TASK-1".to_string(), "TASK-3".to_string(), "TASK-2".to_string()],
        );
        assert_eq!(merged, vec!["TASK-1", "TASK-2", "TASK-3"]);
    }

    #[test]
    fn merge_subject_ids_works_without_flag() {
        let merged = merge_subject_ids(None, vec!["TASK-9".to_string()]);
        assert_eq!(merged, vec!["TASK-9"]);
    }

    #[test]
    fn distinct_in_order_preserves_first_occurrence_order() {
        let ids = distinct_in_order(vec![
            "b".to_string(),
            "a".to_string(),
            "b".to_string(),
            "c".to_string(),
            "a".to_string(),
        ]);
        assert_eq!(ids, vec!["b", "a", "c"]);
    }

    #[test]
    fn bulk_failure_kind_is_not_found_when_all_failures_are_not_found() {
        let items = vec![
            BulkItemResult { subject_id: "a".to_string(), ok: true, dropped_entries: None, error: None },
            BulkItemResult {
                subject_id: "b".to_string(),
                ok: false,
                dropped_entries: None,
                error: Some("queue subject not found or not pending".to_string()),
            },
        ];
        assert_eq!(bulk_failure_kind(&items), CliErrorKind::NotFound);
    }

    #[test]
    fn bulk_failure_kind_is_internal_for_mixed_failures() {
        let items = vec![
            BulkItemResult {
                subject_id: "a".to_string(),
                ok: false,
                dropped_entries: None,
                error: Some("queue subject not found".to_string()),
            },
            BulkItemResult {
                subject_id: "b".to_string(),
                ok: false,
                dropped_entries: None,
                error: Some("plugin rpc timed out".to_string()),
            },
        ];
        assert_eq!(bulk_failure_kind(&items), CliErrorKind::Internal);
    }

    #[test]
    fn bulk_payload_carries_per_item_results_and_counts() {
        let items = vec![
            BulkItemResult { subject_id: "a".to_string(), ok: true, dropped_entries: Some(2), error: None },
            BulkItemResult {
                subject_id: "b".to_string(),
                ok: false,
                dropped_entries: None,
                error: Some("queue subject not found".to_string()),
            },
        ];
        let payload = bulk_payload(BulkVerb::Drop, true, &items).expect("payload should serialize");
        assert_eq!(payload["op"], "drop");
        assert_eq!(payload["all"], true);
        assert_eq!(payload["requested"], 2);
        assert_eq!(payload["succeeded"], 1);
        assert_eq!(payload["failed"], 1);
        let items_value = payload["items"].as_array().expect("items array");
        assert_eq!(items_value.len(), 2);
        assert_eq!(items_value[0]["subject_id"], "a");
        assert_eq!(items_value[0]["ok"], true);
        assert_eq!(items_value[0]["dropped_entries"], 2);
        assert_eq!(items_value[1]["ok"], false);
        assert_eq!(items_value[1]["error"], "queue subject not found");
        assert!(items_value[0].get("error").is_none(), "successful items should omit error");
    }

    #[test]
    fn bulk_verb_status_sets_mirror_single_item_paths() {
        assert_eq!(BulkVerb::Hold.statuses(), &[animus_queue_protocol::status::PENDING]);
        assert_eq!(BulkVerb::Release.statuses(), &[animus_queue_protocol::status::HELD]);
        assert_eq!(
            BulkVerb::Drop.statuses(),
            &[
                animus_queue_protocol::status::PENDING,
                animus_queue_protocol::status::HELD,
                animus_queue_protocol::status::ASSIGNED
            ]
        );
    }
}
