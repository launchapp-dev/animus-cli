mod control_routing;

pub(crate) use control_routing::build_queue_routing;

use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use orchestrator_core::{load_workflow_config_or_default, services::ServiceHub};
use protocol::{SubjectDispatch, SubjectDispatchExt};

use super::ops_workflow::resolve_requirement_workflow_ref;
use super::subject_id_dispatch::{
    deprecated_subject_flag_warning, resolve_subject_id_ref, subject_ref_for_kind, RouterSubjectProbe, SubjectKindProbe,
};
use crate::{invalid_input_error, print_ok, print_value, CliError, CliErrorKind, QueueCommand, QueueSubjectArgs};

/// Build a genuinely subjectless (ad-hoc) dispatch with NO bound subject.
///
/// The wire subject is now optional, so "no subject" is a true absence
/// (`SubjectDispatch::subject == None`) rather than a synthetic `custom`-kind
/// sentinel. The run-loop binds a None subject context: subject template vars
/// are simply absent and no subject adapter is resolved. Subject-bound command
/// phases are out of place in such a workflow and should be command-guarded /
/// skipped.
///
/// Because the dispatch carries no subject there is no `subject_key`, so the
/// queue does not dedup subjectless runs — a burst of `relate` firings each
/// enqueue as their own entry.
fn subjectless_dispatch(workflow_ref: String, input: Option<serde_json::Value>) -> SubjectDispatch {
    SubjectDispatch::subjectless(workflow_ref, "manual-queue-enqueue", chrono::Utc::now()).with_input(input)
}

#[allow(clippy::too_many_arguments)]
async fn resolve_enqueue_dispatch(
    project_root: &str,
    task_id: Option<String>,
    requirement_id: Option<String>,
    title: Option<String>,
    description: Option<String>,
    workflow_ref: Option<String>,
    input: Option<serde_json::Value>,
    adhoc: bool,
) -> Result<SubjectDispatch> {
    if adhoc {
        let workflow_ref = workflow_ref.ok_or_else(|| {
            invalid_input_error(
                "--adhoc requires --workflow-ref: a subjectless run must name the workflow to dispatch.",
            )
        })?;
        return Ok(subjectless_dispatch(workflow_ref, input));
    }
    match (task_id, requirement_id, title) {
        (Some(task_id), None, None) => {
            // Resolve the task's existence through the installed subject_backend
            // plugin(s) — the SAME store `subject get/list/status` read — instead
            // of the in-tree file-backed task store. On a deployment whose
            // subjects live in a plugin backend (e.g. animus-postgres) the
            // in-tree store is empty, so the legacy `hub.tasks().get()` path
            // returned not_found for subjects that demonstrably exist. This keeps
            // enqueue-by-id consistent with the generic `--subject-id` path.
            let workflow_ref = workflow_ref.unwrap_or_else(|| {
                load_workflow_config_or_default(std::path::Path::new(project_root)).config.default_workflow_ref
            });
            let probe = RouterSubjectProbe::discover(std::path::Path::new(project_root)).await?;
            resolve_builtin_kind_dispatch("task", &task_id, workflow_ref, input, &probe).await
        }
        (None, Some(requirement_id), None) => {
            let workflow_ref = match workflow_ref {
                Some(workflow_ref) => workflow_ref,
                None => resolve_requirement_workflow_ref(project_root)?,
            };
            let probe = RouterSubjectProbe::discover(std::path::Path::new(project_root)).await?;
            resolve_builtin_kind_dispatch("requirement", &requirement_id, workflow_ref, input, &probe).await
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
            "no subject specified. Use --task-id TASK_ID for existing tasks, --requirement-id REQ_ID for requirements, --title \"name\" for custom dispatches, or --adhoc --workflow-ref REF for a subjectless run."
        )),
        _ => Err(anyhow!(
            "--task-id, --requirement-id, and --title are mutually exclusive - provide only one subject selector."
        )),
    }
}

/// Resolve a built-in `--task-id` / `--requirement-id` enqueue by confirming the
/// subject's existence through the subject router (the installed
/// `subject_backend` plugin), then building a kind-correct dispatch carrying the
/// backend-qualified id.
///
/// The existence probe routes `<kind>/get` to the same plugin store that
/// `subject get/list/status` and the generic `--subject-id` enqueue path use,
/// so a subject created through a plugin backend resolves here too. A missing
/// subject surfaces a clean `not_found` (exit class 3); a genuinely unhealthy
/// backend propagates as an error rather than masquerading as not-found.
async fn resolve_builtin_kind_dispatch(
    kind: &str,
    native_id: &str,
    workflow_ref: String,
    input: Option<serde_json::Value>,
    probe: &dyn SubjectKindProbe,
) -> Result<SubjectDispatch> {
    let qualified_id = crate::qualify_subject_id(native_id, kind);
    if !probe.subject_exists(kind, &qualified_id).await? {
        return Err(crate::not_found_error(format!("{kind} not found: {native_id}")));
    }
    Ok(SubjectDispatch::for_subject_with_metadata(
        subject_ref_for_kind(kind, qualified_id),
        workflow_ref,
        "manual-queue-enqueue",
        chrono::Utc::now(),
    )
    .with_input(input))
}

/// Resolve a generic `--subject-id` enqueue into a kind-correct
/// [`SubjectDispatch`]. The subject's real kind is taken from the qualified
/// prefix or discovered by probing the installed subject backends, so a
/// `kind=blog` subject is stored as a `blog` dispatch (and leased/resolved via
/// `blog/get`) rather than coerced to `task`.
async fn resolve_enqueue_dispatch_for_subject_id(
    project_root: &str,
    subject_id: &str,
    workflow_ref: Option<String>,
    input: Option<serde_json::Value>,
) -> Result<SubjectDispatch> {
    let probe = RouterSubjectProbe::discover(std::path::Path::new(project_root)).await?;
    let subject_ref = resolve_subject_id_ref(subject_id, &probe).await?;
    let workflow_ref = workflow_ref.unwrap_or_else(|| {
        load_workflow_config_or_default(std::path::Path::new(project_root)).config.default_workflow_ref
    });
    Ok(SubjectDispatch::for_subject_with_metadata(
        subject_ref,
        workflow_ref,
        "manual-queue-enqueue",
        chrono::Utc::now(),
    )
    .with_input(input))
}

/// Reserved key under which a spawning chat conversation's id is stamped into a
/// dispatch's `input` payload. The workflow runner reads it back out to emit
/// `ANIMUS_CONVERSATION_ID` per phase so provider spend attributes to the
/// conversation that enqueued the workflow.
pub(crate) const CONVERSATION_INPUT_KEY: &str = "__animus_conversation_id";

/// Stamp `ANIMUS_CONVERSATION_ID` (when present in the process env — e.g. the
/// chat conductor's bridged `animus mcp serve`) into the dispatch input so it
/// persists onto the queued run and reaches the runner. Leaves a non-object
/// input untouched to avoid corrupting an explicit payload.
fn inject_conversation_id_from_env(input: &mut Option<serde_json::Value>) {
    let conversation_id = match std::env::var("ANIMUS_CONVERSATION_ID") {
        Ok(value) if !value.trim().is_empty() => value.trim().to_string(),
        _ => return,
    };
    match input {
        Some(serde_json::Value::Object(map)) => {
            // The env value is the conversation that actually enqueued this run;
            // it MUST win over any caller-supplied (stale/forged) reserved key so
            // spend never attributes to the wrong conversation.
            map.insert(CONVERSATION_INPUT_KEY.to_string(), serde_json::Value::String(conversation_id));
        }
        None => {
            let mut map = serde_json::Map::new();
            map.insert(CONVERSATION_INPUT_KEY.to_string(), serde_json::Value::String(conversation_id));
            *input = Some(serde_json::Value::Object(map));
        }
        Some(_) => {}
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
    _hub: Arc<dyn ServiceHub>,
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
            if json {
                return print_value(response, true);
            }
            render_queue_list_human(&response);
            Ok(())
        }
        QueueCommand::Stats => {
            let stats = crate::services::plugin_clients::call_queue_stats(project_root_path)
                .await?
                .ok_or_else(|| queue_plugin_required("stats"))?;
            if json {
                return print_value(stats, true);
            }
            println!("{}", queue_stats_summary(&stats));
            Ok(())
        }
        QueueCommand::Enqueue(args) => {
            // Deprecation notice (behavior unchanged): the legacy `--task-id` /
            // `--requirement-id` selectors still resolve exactly as before, but
            // `--subject-id <kind>:<id>` is now the canonical single dispatch
            // selector. Only warn when the deprecated flag is the actual
            // selector (i.e. `--subject-id` was not also passed). Suppressed in
            // `--json` mode: the error/ok envelope is emitted to stderr too, so
            // a bare warning line would corrupt the `animus.cli.v1` stream that
            // scripted/MCP callers parse (the MCP tool schema already documents
            // the deprecation).
            if !json && args.subject_id.is_none() {
                // Pass the raw selector value: the helper qualifies it with the
                // same rule the dispatch uses, so an already-qualified id is
                // preserved and the hint never names a different subject.
                if let Some(id) = args.task_id.as_deref() {
                    eprintln!("{}", deprecated_subject_flag_warning("--task-id", "task", id));
                }
                if let Some(id) = args.requirement_id.as_deref() {
                    eprintln!("{}", deprecated_subject_flag_warning("--requirement-id", "requirement", id));
                }
            }
            let mut input = args.input_json.clone().map(|value| serde_json::from_str(&value)).transpose()?;
            inject_conversation_id_from_env(&mut input);
            let dispatch = if let Some(subject_id) = args.subject_id.clone() {
                // Generic BaaS path: resolve the subject's real kind (qualified
                // prefix or router probe) so a non-task/requirement subject
                // dispatches under its own kind instead of being coerced to task.
                resolve_enqueue_dispatch_for_subject_id(project_root, &subject_id, args.workflow_ref.clone(), input)
                    .await?
            } else {
                resolve_enqueue_dispatch(
                    project_root,
                    args.task_id.clone(),
                    args.requirement_id.clone(),
                    args.title.clone(),
                    args.description.clone(),
                    args.workflow_ref.clone(),
                    input,
                    args.adhoc,
                )
                .await?
            };

            let run_at = args.run_at.as_deref().map(resolve_run_at).transpose()?;

            // The kernel dispatch type (rc protocol) can now be subjectless, but
            // the queue plugin RPC is still pinned to the v0.5 line for deployed
            // queue-plugin wire compatibility, and that SubjectDispatch requires
            // a subject. A subjectless run therefore cannot travel through the
            // installed queue plugin yet: dispatch it directly (workflow run)
            // until the queue protocol takes up the optional-subject 0.7 line.
            if dispatch.subject().is_none() {
                return Err(invalid_input_error(
                    "subjectless (--adhoc) enqueue is not yet supported through the installed queue plugin: its queue RPC protocol still requires a subject. Dispatch the workflow directly instead.",
                ));
            }
            let dispatch_value =
                serde_json::to_value(&dispatch).context("encoding subject_dispatch for queue plugin")?;
            let plugin_dispatch = serde_json::from_value(dispatch_value)
                .context("subject_dispatch shape drift vs animus_subject_protocol v0.5")?;
            let plugin_request = animus_queue_protocol::QueueEnqueueRequest {
                subject_dispatch: plugin_dispatch,
                run_at: run_at.clone(),
                expire_after_secs: args.expire_after_secs,
            };
            let plugin_response =
                crate::services::plugin_clients::call_queue_enqueue(project_root_path, &plugin_request)
                    .await?
                    .ok_or_else(|| queue_plugin_required("enqueue"))?;
            // Wake a running daemon so the freshly enqueued entry drains
            // now instead of on the next heartbeat. Fire-and-forget. Deferred
            // entries still nudge — the scheduler re-evaluates and simply
            // leaves a not-yet-due entry queued.
            if plugin_response.enqueued {
                orchestrator_daemon_runtime::control::nudge_daemon_scheduler_best_effort(project_root_path).await;
            }
            let translated = serde_json::json!({
                "enqueued": plugin_response.enqueued,
                "entry_id": plugin_response.entry_id,
                "subject_id": plugin_response.subject_id,
                "run_at": run_at,
                "expire_after_secs": args.expire_after_secs,
                "warning": plugin_response.warning,
                "via": "plugin_host",
            });
            if !json {
                let base = if plugin_response.enqueued {
                    match run_at.as_deref() {
                        Some(when) => format!("subject dispatch scheduled for {when} (via queue plugin)"),
                        None => "subject dispatch enqueued (via queue plugin)".to_string(),
                    }
                } else {
                    "subject dispatch already queued (via queue plugin)".to_string()
                };
                print_ok(&base, false);
                if let Some(warning) = plugin_response.warning.as_deref() {
                    println!("warning: {warning}");
                }
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

/// Resolve a `--at` value into an RFC 3339 timestamp for the queue plugin.
/// Accepts either an absolute RFC 3339 instant (normalized to UTC) or a
/// relative offset like `90s` / `30m` / `2h` / `3d` added to now (a bare
/// number is treated as seconds).
fn resolve_run_at(raw: &str) -> Result<String> {
    let trimmed = raw.trim();
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(trimmed) {
        return Ok(dt.with_timezone(&chrono::Utc).to_rfc3339());
    }
    let secs = crate::cli_types::parse_duration_secs_default_seconds(trimmed).map_err(|err| {
        invalid_input_error(format!(
            "--at '{trimmed}' is not an RFC 3339 timestamp or a duration (e.g. 30m, 2h): {err}"
        ))
    })?;
    Ok((chrono::Utc::now() + chrono::Duration::seconds(secs as i64)).to_rfc3339())
}

/// Render a `queue list` response for human (non-`--json`) output: a table of
/// queued entries plus a one-line stats summary. An empty queue prints a hint
/// instead of a bare table.
fn render_queue_list_human(response: &animus_queue_protocol::QueueListResponse) {
    if response.entries.is_empty() {
        println!("queue is empty");
        println!("enqueue work with `animus queue enqueue --task-id <id>` (or --requirement-id / --title)");
        return;
    }
    let rows: Vec<Vec<String>> = response
        .entries
        .iter()
        .enumerate()
        .map(|(pos, entry)| {
            vec![
                (pos + 1).to_string(),
                entry.subject_id.clone(),
                entry.status.clone(),
                entry.subject_dispatch.workflow_ref.clone(),
                queue_short_timestamp(&entry.enqueued_at),
            ]
        })
        .collect();
    crate::render_table(&["POS", "SUBJECT", "STATUS", "WORKFLOW", "ENQUEUED"], &rows);
    println!("\n{}", queue_stats_summary(&response.stats));
}

/// One-line aggregate summary of queue counts. `deferred` (a subset of
/// `pending` not yet leasable) is shown only when non-zero to keep the
/// common line uncluttered.
fn queue_stats_summary(stats: &animus_queue_protocol::QueueStats) -> String {
    let mut summary = format!(
        "{} total | {} pending | {} assigned | {} held",
        stats.total, stats.pending, stats.assigned, stats.held
    );
    if stats.deferred > 0 {
        summary.push_str(&format!(" ({} deferred)", stats.deferred));
    }
    summary
}

/// Trim an RFC 3339 timestamp to `YYYY-MM-DD HH:MM` for table density.
fn queue_short_timestamp(ts: &str) -> String {
    if ts.is_empty() {
        return "--".to_string();
    }
    match ts.split_once('T') {
        Some((date, time)) => {
            let hm = time.get(..5).unwrap_or(time);
            format!("{date} {hm}")
        }
        None => ts.to_string(),
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
    let matched =
        response.entries.into_iter().filter(|entry| subject_id_matches(&entry.subject_id, subject_id)).collect();
    Ok(Some(matched))
}

/// Compare a stored queue `subject_id` (kind-qualified, e.g. `task:TASK-001`)
/// against a user-supplied id, accepting either the qualified form or the
/// bare native id (e.g. `TASK-001`). A qualified query matches only an
/// identical stored id, so two kinds sharing a native id (`task:TASK-001` vs
/// `linear:TASK-001`) never alias. A bare query matches any stored entry whose
/// native id equals it, after the stored `<kind>:` qualifier is stripped.
fn subject_id_matches(stored: &str, query: &str) -> bool {
    if stored == query {
        return true;
    }
    if query.contains(':') {
        return false;
    }
    crate::bare_subject_id(stored) == query
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
    use async_trait::async_trait;
    use orchestrator_core::{builtin_workflow_config, write_agent_runtime_config, write_workflow_config};

    use super::*;

    /// Stub subject probe: reports whether a given `(kind, qualified_id)` exists,
    /// so the built-in `--task-id` / `--requirement-id` resolution can be tested
    /// without spawning a real subject_backend plugin.
    struct StubProbe {
        exists: bool,
    }

    #[async_trait]
    impl SubjectKindProbe for StubProbe {
        fn candidate_kinds(&self) -> Vec<String> {
            vec!["task".to_string(), "requirement".to_string()]
        }

        async fn subject_exists(&self, _kind: &str, _qualified_id: &str) -> Result<bool> {
            Ok(self.exists)
        }
    }

    #[tokio::test]
    async fn resolve_builtin_kind_dispatch_resolves_existing_task_via_router() {
        // The router (subject_backend plugin) owns the id — enqueue must resolve
        // it and build a kind=task dispatch carrying the backend-qualified id.
        let probe = StubProbe { exists: true };
        let dispatch = resolve_builtin_kind_dispatch("task", "TASK-216", "standard-workflow".to_string(), None, &probe)
            .await
            .expect("existing task resolves");
        // `task` canonicalizes to the namespaced built-in kind, and the id is
        // carried in the backend-qualified `task:<native>` form the subject
        // backend resolves (matching the generic `--subject-id` path).
        assert_eq!(dispatch.subject_kind(), Some("animus.task"));
        assert_eq!(dispatch.subject_id(), Some("task:TASK-216"));
        assert_eq!(dispatch.workflow_ref, "standard-workflow");
    }

    #[tokio::test]
    async fn resolve_builtin_kind_dispatch_missing_subject_is_not_found() {
        // The router does not own the id — enqueue must surface a clean not_found
        // (exit class 3) rather than a generic internal error.
        let probe = StubProbe { exists: false };
        let err = resolve_builtin_kind_dispatch("task", "TASK-999", "standard-workflow".to_string(), None, &probe)
            .await
            .expect_err("missing subject should fail");
        assert!(err.to_string().contains("task not found: TASK-999"), "got: {err}");
        assert_eq!(crate::classify_cli_error_kind(&err), CliErrorKind::NotFound);
    }

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
        write_agent_runtime_config(temp.path(), &crate::shared::seeded_agent_runtime_config())
            .expect("write runtime config");

        let err =
            resolve_enqueue_dispatch(temp.path().to_string_lossy().as_ref(), None, None, None, None, None, None, false)
                .await
                .expect_err("missing subject should fail");

        let msg = err.to_string();
        assert!(msg.contains("--task-id"), "error should mention --task-id");
        assert!(msg.contains("--requirement-id"), "error should mention --requirement-id");
        assert!(msg.contains("--title"), "error should mention --title");
        assert!(msg.contains("custom dispatches"), "error should suggest custom dispatches");
        assert!(msg.contains("--adhoc"), "error should mention the subjectless --adhoc path");
    }

    #[tokio::test]
    async fn resolve_enqueue_dispatch_adhoc_builds_subjectless_dispatch() {
        let temp = tempfile::tempdir().expect("tempdir");
        // A subjectless run: no task/requirement/title selector, --adhoc set with
        // an explicit workflow ref. It must build a valid dispatch (not error).
        let dispatch = resolve_enqueue_dispatch(
            temp.path().to_string_lossy().as_ref(),
            None,
            None,
            None,
            None,
            Some("relate".to_string()),
            None,
            true,
        )
        .await
        .expect("adhoc dispatch builds");
        assert_eq!(dispatch.workflow_ref, "relate");
        // Genuinely subjectless: the dispatch carries NO subject at all (not a
        // synthetic custom sentinel). The run-loop binds a None subject context.
        assert!(dispatch.subject().is_none(), "subjectless dispatch must have no subject");
        assert_eq!(dispatch.subject_id(), None);
        assert_eq!(dispatch.subject_kind(), None);
    }

    #[tokio::test]
    async fn resolve_enqueue_dispatch_adhoc_requires_workflow_ref() {
        let temp = tempfile::tempdir().expect("tempdir");
        let err =
            resolve_enqueue_dispatch(temp.path().to_string_lossy().as_ref(), None, None, None, None, None, None, true)
                .await
                .expect_err("adhoc without workflow ref should fail");
        assert!(err.to_string().contains("--workflow-ref"), "error names the missing workflow ref: {err}");
    }

    #[test]
    fn subjectless_dispatches_have_no_subject_key() {
        // A subjectless dispatch carries no subject, so it has no queue
        // subject_key. With nothing to key on, the queue does not dedup a burst
        // of subjectless runs (e.g. a `relate` storm) into one entry.
        let a = subjectless_dispatch("relate".to_string(), None);
        let b = subjectless_dispatch("relate".to_string(), None);
        assert_eq!(a.subject_key(), None);
        assert_eq!(b.subject_key(), None);
        assert!(a.subject().is_none());
        assert!(b.subject().is_none());
    }

    #[tokio::test]
    async fn resolve_enqueue_dispatch_multiple_subjects_shows_mutual_exclusivity_error() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workflow_config = builtin_workflow_config();
        write_workflow_config(temp.path(), &workflow_config).expect("write config");
        write_agent_runtime_config(temp.path(), &crate::shared::seeded_agent_runtime_config())
            .expect("write runtime config");

        let err = resolve_enqueue_dispatch(
            temp.path().to_string_lossy().as_ref(),
            Some("TASK-1".to_string()),
            Some("REQ-1".to_string()),
            None,
            None,
            None,
            None,
            false,
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
    fn subject_id_matches_accepts_bare_and_qualified() {
        assert!(subject_id_matches("task:TASK-001", "task:TASK-001"));
        assert!(subject_id_matches("task:TASK-001", "TASK-001"));
        assert!(subject_id_matches("TASK-001", "TASK-001"));
        assert!(!subject_id_matches("task:TASK-001", "TASK-002"));
        assert!(subject_id_matches("linear:ENG-9", "ENG-9"));
    }

    #[test]
    fn subject_id_matches_keeps_qualified_queries_exact() {
        // A qualified query must not alias a same-native-id entry of another kind.
        assert!(!subject_id_matches("linear:TASK-001", "task:TASK-001"));
        assert!(subject_id_matches("task:TASK-001", "task:TASK-001"));
        // A bare query still matches across the qualifier boundary, but only
        // against the stored native id.
        assert!(subject_id_matches("linear:TASK-001", "TASK-001"));
        assert!(subject_id_matches("task:TASK-001", "TASK-001"));
    }

    #[test]
    fn queue_stats_summary_renders_all_buckets() {
        let stats = animus_queue_protocol::QueueStats { total: 5, pending: 3, assigned: 1, held: 1, deferred: 0 };
        assert_eq!(queue_stats_summary(&stats), "5 total | 3 pending | 1 assigned | 1 held");
    }

    #[test]
    fn queue_stats_summary_appends_deferred_when_present() {
        let stats = animus_queue_protocol::QueueStats { total: 5, pending: 3, assigned: 1, held: 1, deferred: 2 };
        assert_eq!(queue_stats_summary(&stats), "5 total | 3 pending | 1 assigned | 1 held (2 deferred)");
    }

    #[test]
    fn queue_short_timestamp_trims_to_minute() {
        assert_eq!(queue_short_timestamp("2026-06-10T12:34:56Z"), "2026-06-10 12:34");
        assert_eq!(queue_short_timestamp(""), "--");
        assert_eq!(queue_short_timestamp("nodate"), "nodate");
    }

    #[test]
    fn resolve_run_at_passes_through_rfc3339() {
        let out = resolve_run_at("2030-01-01T15:00:00Z").expect("rfc3339");
        let parsed = chrono::DateTime::parse_from_rfc3339(&out).expect("re-parse");
        assert_eq!(parsed.with_timezone(&chrono::Utc).to_rfc3339(), out);
        assert!(out.starts_with("2030-01-01T15:00:00"));
    }

    #[test]
    fn resolve_run_at_accepts_relative_offset() {
        let before = chrono::Utc::now();
        let out = resolve_run_at("2h").expect("relative");
        let when = chrono::DateTime::parse_from_rfc3339(&out).expect("parse").with_timezone(&chrono::Utc);
        let delta = (when - before).num_seconds();
        // ~2h ahead, allowing a wide band for test scheduling slack.
        assert!((7_100..=7_300).contains(&delta), "delta was {delta}s");
    }

    #[test]
    fn resolve_run_at_rejects_garbage() {
        let err = resolve_run_at("whenever").expect_err("garbage must error");
        assert!(err.to_string().contains("--at 'whenever'"));
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
