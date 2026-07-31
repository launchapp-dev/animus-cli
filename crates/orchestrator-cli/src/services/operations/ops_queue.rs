mod control_routing;

pub(crate) use control_routing::build_queue_routing;

use std::sync::Arc;

use animus_execution_protocol::RepositoryReservation;
use anyhow::{Context, Result};
use orchestrator_core::{load_workflow_config_or_default, services::ServiceHub};
use protocol::{SubjectDispatch, SubjectDispatchExt};

use super::subject_id_dispatch::{resolve_subject_id_ref, RouterSubjectProbe};
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

async fn resolve_enqueue_dispatch(
    project_root: &str,
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
    match title {
        Some(title) => Ok(SubjectDispatch::for_custom(
            title,
            description.unwrap_or_default(),
            workflow_ref.unwrap_or_else(|| {
                load_workflow_config_or_default(std::path::Path::new(project_root)).config.default_workflow_ref
            }),
            input,
            "manual-queue-enqueue",
        )),
        None => Err(invalid_input_error(
            "no subject specified. Use --subject-id SUBJECT_ID for existing subjects (any kind; qualified task:TASK-001 or bare TASK-001), --title \"name\" for custom dispatches, or --adhoc --workflow-ref REF for a subjectless run."
        )),
    }
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
) -> Result<(SubjectDispatch, serde_json::Value)> {
    let probe = RouterSubjectProbe::discover(std::path::Path::new(project_root)).await?;
    let subject_ref = resolve_subject_id_ref(subject_id, &probe).await?;
    let subject_record = probe.subject_record(&subject_ref).await?;
    let workflow_ref = workflow_ref.unwrap_or_else(|| {
        load_workflow_config_or_default(std::path::Path::new(project_root)).config.default_workflow_ref
    });
    Ok((
        SubjectDispatch::for_subject_with_metadata(
            subject_ref,
            workflow_ref,
            "manual-queue-enqueue",
            chrono::Utc::now(),
        )
        .with_input(input),
        subject_record,
    ))
}

fn record_string_field(record: &serde_json::Value, names: &[&str]) -> Option<String> {
    let record = record.get("subject").unwrap_or(record);
    [
        record.get("data").and_then(serde_json::Value::as_object),
        record.get("attributes").and_then(serde_json::Value::as_object),
        record.get("custom").and_then(serde_json::Value::as_object),
        record.as_object(),
    ]
    .into_iter()
    .flatten()
    .find_map(|bag| {
        bag.iter().find_map(|(key, value)| {
            names
                .iter()
                .any(|name| key.eq_ignore_ascii_case(name))
                .then(|| value.as_str().map(str::trim).filter(|value| !value.is_empty()).map(str::to_string))
                .flatten()
        })
    })
}

fn dispatch_string_field(dispatch: &SubjectDispatch, names: &[&str]) -> Option<String> {
    dispatch.vars.iter().find_map(|(key, value)| {
        (names.iter().any(|name| key.eq_ignore_ascii_case(name)) && !value.trim().is_empty())
            .then(|| value.trim().to_string())
    })
}

fn canonical_head_ref(value: &str, field: &str) -> Result<String> {
    let value = value.trim();
    let value = value.strip_prefix("origin/").unwrap_or(value);
    let qualified = if value.starts_with("refs/heads/") {
        value.to_string()
    } else if value.starts_with("refs/") {
        return Err(invalid_input_error(format!("{field} must name a mutable branch (refs/heads/*), got '{value}'")));
    } else {
        format!("refs/heads/{value}")
    };
    if qualified.contains("..")
        || qualified.ends_with('/')
        || qualified.chars().any(|character| character.is_whitespace() || "~^:?*[\\".contains(character))
    {
        return Err(invalid_input_error(format!("{field} is not a valid git branch ref: '{value}'")));
    }
    Ok(qualified)
}

fn default_subject_head_ref(dispatch: &SubjectDispatch) -> Result<String> {
    let subject = dispatch.subject().context("repository reservation requires a subject")?;
    let native_id = subject.id().split_once(':').map_or(subject.id(), |(_, native)| native);
    let component: String =
        native_id
            .trim()
            .chars()
            .map(|character| {
                if character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.') {
                    character
                } else {
                    '-'
                }
            })
            .collect();
    anyhow::ensure!(!component.is_empty(), "subject id cannot produce an owned git branch");
    Ok(format!("refs/heads/animus/{component}"))
}

fn repository_reservation_from_dispatch(
    dispatch: &SubjectDispatch,
    subject_record: Option<&serde_json::Value>,
) -> Result<Option<RepositoryReservation>> {
    let field = |names: &[&str]| {
        dispatch_string_field(dispatch, names)
            .or_else(|| subject_record.and_then(|record| record_string_field(record, names)))
    };
    // `git_repo` is the explicit coding marker used by Portal task subjects.
    // Do not infer coding ownership from a generic `repository` field: other
    // subject kinds may use that word as descriptive metadata.
    let repository = field(&["git_repo"]);
    let base_ref = field(&["base_ref", "git_ref"]);
    let head_ref = field(&["head_ref", "branch", "branch_name"]);

    let Some(repository) = repository else {
        anyhow::ensure!(
            base_ref.is_none() && head_ref.is_none(),
            "subject declares git_ref/head_ref without git_repo; set git_repo or remove the git fields"
        );
        return Ok(None);
    };
    let base_ref = base_ref.ok_or_else(|| {
        invalid_input_error(
            "coding subject declares git_repo but no git_ref/base_ref; set the immutable base branch before enqueue",
        )
    })?;
    let reservation = RepositoryReservation {
        repository,
        base_ref: canonical_head_ref(&base_ref, "git_ref/base_ref")?,
        head_ref: match head_ref {
            Some(head_ref) => canonical_head_ref(&head_ref, "head_ref/branch")?,
            None => default_subject_head_ref(dispatch)?,
        },
    };
    reservation.validate().map_err(invalid_input_error)?;
    Ok(Some(reservation))
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

#[derive(Debug, Clone)]
pub(crate) struct QueueEnqueueRequest {
    pub(crate) title: Option<String>,
    pub(crate) subject_id: Option<String>,
    pub(crate) description: Option<String>,
    pub(crate) workflow_ref: Option<String>,
    pub(crate) input: Option<serde_json::Value>,
    pub(crate) idempotency_key: Option<String>,
    pub(crate) run_at: Option<String>,
    pub(crate) expire_after_secs: Option<u64>,
    pub(crate) adhoc: bool,
}

pub(crate) async fn queue_list_application(project_root: &str) -> Result<animus_queue_protocol::QueueListResponse> {
    crate::services::plugin_clients::call_queue_list(
        std::path::Path::new(project_root),
        &animus_queue_protocol::QueueListRequest::default(),
    )
    .await?
    .ok_or_else(|| queue_plugin_required("list"))
}

pub(crate) async fn queue_stats_application(project_root: &str) -> Result<animus_queue_protocol::QueueStats> {
    crate::services::plugin_clients::call_queue_stats(std::path::Path::new(project_root))
        .await?
        .ok_or_else(|| queue_plugin_required("stats"))
}

pub(crate) async fn queue_enqueue_application(
    mut request: QueueEnqueueRequest,
    project_root: &str,
) -> Result<serde_json::Value> {
    inject_conversation_id_from_env(&mut request.input);
    let (dispatch, subject_record) = if let Some(subject_id) = request.subject_id.as_deref() {
        resolve_enqueue_dispatch_for_subject_id(project_root, subject_id, request.workflow_ref.clone(), request.input)
            .await?
    } else {
        (
            resolve_enqueue_dispatch(
                project_root,
                request.title,
                request.description,
                request.workflow_ref,
                request.input,
                request.adhoc,
            )
            .await?,
            serde_json::Value::Null,
        )
    };
    let run_at = request.run_at.as_deref().map(resolve_run_at).transpose()?;
    if request.expire_after_secs.is_some() && run_at.is_none() {
        return Err(invalid_input_error("--expire-after requires --at"));
    }
    if dispatch.subject().is_none() {
        return Err(invalid_input_error(
            "subjectless (--adhoc) enqueue is not yet supported through the installed queue plugin: its queue RPC protocol still requires a subject. Dispatch the workflow directly instead.",
        ));
    }
    let dispatch_value = serde_json::to_value(&dispatch).context("encoding subject_dispatch for queue plugin")?;
    let plugin_dispatch = serde_json::from_value(dispatch_value)
        .context("subject_dispatch shape drift vs animus_subject_protocol v0.5")?;
    let repository =
        repository_reservation_from_dispatch(&dispatch, (!subject_record.is_null()).then_some(&subject_record))?;
    let plugin_request = animus_queue_protocol::QueueEnqueueV2Request {
        subject_dispatch: plugin_dispatch,
        idempotency_key: request.idempotency_key,
        repository,
        run_at: run_at.clone(),
        expire_after_secs: request.expire_after_secs,
    };
    let plugin_response =
        crate::services::plugin_clients::call_queue_enqueue_v2(std::path::Path::new(project_root), &plugin_request)
            .await?
            .ok_or_else(|| queue_plugin_required("enqueue"))?;
    if plugin_response.enqueued {
        orchestrator_daemon_runtime::control::nudge_daemon_scheduler_best_effort(std::path::Path::new(project_root))
            .await;
    }
    Ok(serde_json::json!({
        "enqueued": plugin_response.enqueued,
        "entry_id": plugin_response.entry_id,
        "subject_id": plugin_response.subject.qualified_id,
        "subject_generation": plugin_response.subject.generation,
        "run_at": run_at,
        "expire_after_secs": request.expire_after_secs,
        "warning": plugin_response.warning,
        "via": "plugin_host",
    }))
}

pub(crate) async fn queue_reorder_application(
    subject_ids: Vec<String>,
    project_root: &str,
) -> Result<serde_json::Value> {
    let reordered_count = try_queue_reorder_via_plugin(project_root, &subject_ids)
        .await?
        .ok_or_else(|| queue_plugin_required("reorder"))?;
    Ok(serde_json::json!({
        "reordered": reordered_count > 0,
        "reordered_count": reordered_count,
        "subject_ids": subject_ids,
        "via": "plugin_host",
    }))
}

pub(crate) async fn handle_queue(
    command: QueueCommand,
    _hub: Arc<dyn ServiceHub>,
    project_root: &str,
    json: bool,
) -> Result<()> {
    match command {
        QueueCommand::List => {
            let response = queue_list_application(project_root).await?;
            if json {
                return print_value(response, true);
            }
            render_queue_list_human(&response);
            Ok(())
        }
        QueueCommand::Stats => {
            let stats = queue_stats_application(project_root).await?;
            if json {
                return print_value(stats, true);
            }
            println!("{}", queue_stats_summary(&stats));
            Ok(())
        }
        QueueCommand::Enqueue(args) => {
            let translated = queue_enqueue_application(
                QueueEnqueueRequest {
                    title: args.title,
                    subject_id: args.subject_id,
                    description: args.description,
                    workflow_ref: args.workflow_ref,
                    input: args.input_json.map(|value| serde_json::from_str(&value)).transpose()?,
                    idempotency_key: args.idempotency_key,
                    run_at: args.run_at,
                    expire_after_secs: args.expire_after_secs,
                    adhoc: args.adhoc,
                },
                project_root,
            )
            .await?;
            if !json {
                let base = if translated["enqueued"].as_bool() == Some(true) {
                    match translated["run_at"].as_str() {
                        Some(when) => format!("subject dispatch scheduled for {when} (via queue plugin)"),
                        None => "subject dispatch enqueued (via queue plugin)".to_string(),
                    }
                } else {
                    "subject dispatch already queued (via queue plugin)".to_string()
                };
                print_ok(&base, false);
                if let Some(warning) = translated["warning"].as_str() {
                    println!("warning: {warning}");
                }
                return Ok(());
            }
            print_value(translated, true)
        }
        QueueCommand::Hold(args) => handle_queue_bulk(QueueBulkVerb::Hold, args, project_root, json).await,
        QueueCommand::Release(args) => handle_queue_bulk(QueueBulkVerb::Release, args, project_root, json).await,
        QueueCommand::Drop(args) => handle_queue_bulk(QueueBulkVerb::Drop, args, project_root, json).await,
        QueueCommand::Reorder(args) => {
            let response = queue_reorder_application(args.subject_ids, project_root).await?;
            if !json {
                if response["reordered"].as_bool() == Some(true) {
                    print_ok("queue reordered (via queue plugin)", false);
                    return Ok(());
                }
                print_ok("queue order unchanged", false);
                return Ok(());
            }
            print_value(response, true)
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
pub(crate) enum QueueBulkVerb {
    Hold,
    Release,
    Drop,
}

impl QueueBulkVerb {
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
pub(crate) struct BulkItemResult {
    pub(crate) subject_id: String,
    pub(crate) ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) dropped_entries: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) error: Option<String>,
}

#[derive(Debug)]
pub(crate) struct QueueBulkResponse {
    pub(crate) verb: QueueBulkVerb,
    pub(crate) all: bool,
    pub(crate) items: Vec<BulkItemResult>,
}

impl QueueBulkResponse {
    pub(crate) fn payload(&self) -> Result<serde_json::Value> {
        bulk_payload(self.verb, self.all, &self.items)
    }

    pub(crate) fn failure_error(&self) -> Result<Option<anyhow::Error>> {
        let failed = self.items.iter().filter(|item| !item.ok).count();
        if failed == 0 {
            return Ok(None);
        }
        let kind = bulk_failure_kind(&self.items);
        let message = format!("queue {}: {} of {} subject(s) failed", self.verb.name(), failed, self.items.len());
        Ok(Some(CliError::new(kind, message).with_details(self.payload()?).into()))
    }
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

fn bulk_payload(verb: QueueBulkVerb, all: bool, items: &[BulkItemResult]) -> Result<serde_json::Value> {
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

fn confirm_bulk_all(verb: QueueBulkVerb, count: usize) -> Result<bool> {
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

async fn resolve_all_subject_ids(verb: QueueBulkVerb, project_root: &str) -> Result<Vec<String>> {
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

async fn apply_bulk_verb(verb: QueueBulkVerb, project_root: &str, subject_id: &str) -> Result<BulkItemResult> {
    let mut item = BulkItemResult { subject_id: subject_id.to_string(), ok: false, dropped_entries: None, error: None };
    match verb {
        QueueBulkVerb::Hold | QueueBulkVerb::Release => {
            let result = match verb {
                QueueBulkVerb::Hold => try_queue_hold_via_plugin(project_root, subject_id).await,
                _ => try_queue_release_via_plugin(project_root, subject_id).await,
            };
            match result {
                Ok(Some(resp)) if resp.changed => item.ok = true,
                Ok(Some(_)) => item.error = Some(verb.state_error().to_string()),
                Ok(None) => return Err(queue_plugin_required(verb.name())),
                Err(err) => item.error = Some(format!("{err:#}")),
            }
        }
        QueueBulkVerb::Drop => match try_queue_drop_via_plugin(project_root, subject_id).await {
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

pub(crate) async fn queue_bulk_application(
    verb: QueueBulkVerb,
    subject_ids: Vec<String>,
    all: bool,
    project_root: &str,
) -> Result<QueueBulkResponse> {
    let subject_ids = distinct_in_order(subject_ids);
    let mut items = Vec::with_capacity(subject_ids.len());
    for subject_id in &subject_ids {
        items.push(apply_bulk_verb(verb, project_root, subject_id).await?);
    }
    if matches!(verb, QueueBulkVerb::Release) && items.iter().any(|item| item.ok) {
        orchestrator_daemon_runtime::control::nudge_daemon_scheduler_best_effort(std::path::Path::new(project_root))
            .await;
    }
    Ok(QueueBulkResponse { verb, all, items })
}

async fn handle_queue_bulk(verb: QueueBulkVerb, args: QueueSubjectArgs, project_root: &str, json: bool) -> Result<()> {
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

    let response = queue_bulk_application(verb, subject_ids, args.all, project_root).await?;
    if !json {
        for item in &response.items {
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
    if let Some(error) = response.failure_error()? {
        return Err(error);
    }
    if json {
        return print_value(response.payload()?, true);
    }
    if response.items.len() > 1 {
        print_ok(&format!("{} {} queue subject(s) (via queue plugin)", verb.past_tense(), response.items.len()), false);
    }
    Ok(())
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
    use orchestrator_core::{builtin_workflow_config, write_agent_runtime_config, write_workflow_config};

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

    #[test]
    fn repository_reservation_is_derived_from_subject_custom_fields() {
        let dispatch = SubjectDispatch::for_task("task:TASK-1175", "standard");
        let record = serde_json::json!({
            "subject": {
                "custom": {
                    "git_repo": "https://github.com/launchapp-dev/animus-cli.git",
                    "git_ref": "main"
                }
            }
        });
        let reservation = repository_reservation_from_dispatch(&dispatch, Some(&record))
            .expect("valid reservation")
            .expect("coding subject");
        assert_eq!(reservation.repository, "https://github.com/launchapp-dev/animus-cli.git");
        assert_eq!(reservation.base_ref, "refs/heads/main");
        assert_eq!(reservation.head_ref, "refs/heads/animus/TASK-1175");
    }

    #[test]
    fn repository_reservation_requires_explicit_base_ref() {
        let dispatch = SubjectDispatch::for_task("task:TASK-1175", "standard");
        let record = serde_json::json!({
            "custom": { "git_repo": "https://github.com/launchapp-dev/animus-cli.git" }
        });
        let error = repository_reservation_from_dispatch(&dispatch, Some(&record))
            .expect_err("coding enqueue without base must fail closed");
        assert!(error.to_string().contains("git_ref/base_ref"));
    }

    #[test]
    fn non_code_subject_has_no_repository_reservation() {
        let dispatch = SubjectDispatch::for_requirement("requirement:REQUIREMENT-076", "audit", "manual-queue-enqueue");
        let record = serde_json::json!({ "custom": { "channel": "stability" } });
        assert!(repository_reservation_from_dispatch(&dispatch, Some(&record)).unwrap().is_none());
    }

    #[tokio::test]
    async fn resolve_enqueue_dispatch_missing_subject_shows_actionable_error() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workflow_config = builtin_workflow_config();
        write_workflow_config(temp.path(), &workflow_config).expect("write config");
        write_agent_runtime_config(temp.path(), &crate::shared::seeded_agent_runtime_config())
            .expect("write runtime config");

        let err = resolve_enqueue_dispatch(temp.path().to_string_lossy().as_ref(), None, None, None, None, false)
            .await
            .expect_err("missing subject should fail");

        let msg = err.to_string();
        assert!(msg.contains("--subject-id"), "error should mention --subject-id");
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
        let err = resolve_enqueue_dispatch(temp.path().to_string_lossy().as_ref(), None, None, None, None, true)
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
    async fn handle_queue_bulk_rejects_yes_without_all() {
        let args =
            QueueSubjectArgs { subject_ids: vec!["TASK-1".to_string()], subject_id: None, all: false, yes: true };
        let err = handle_queue_bulk(QueueBulkVerb::Hold, args, "/nonexistent", false)
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
        let payload = bulk_payload(QueueBulkVerb::Drop, true, &items).expect("payload should serialize");
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
        assert_eq!(QueueBulkVerb::Hold.statuses(), &[animus_queue_protocol::status::PENDING]);
        assert_eq!(QueueBulkVerb::Release.statuses(), &[animus_queue_protocol::status::HELD]);
        assert_eq!(
            QueueBulkVerb::Drop.statuses(),
            &[
                animus_queue_protocol::status::PENDING,
                animus_queue_protocol::status::HELD,
                animus_queue_protocol::status::ASSIGNED
            ]
        );
    }
}
