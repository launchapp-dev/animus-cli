use std::path::Path;

use animus_plugin_protocol::error_codes;
use anyhow::{anyhow, Result};
use orchestrator_daemon_runtime::{resolve_subject_dispatch, SubjectPluginDispatch};
use protocol::Config;
use serde::Serialize;
use serde_json::{json, Value};

use crate::{
    invalid_input_error, not_found_error, print_value, unavailable_error, BatchOnError, SubjectBatchCreateArgs,
    SubjectBatchUpdateArgs, SubjectCommand, SubjectCreateArgs, SubjectDeleteArgs, SubjectGetArgs, SubjectListArgs,
    SubjectNextArgs, SubjectStatusArgs, SubjectUpdateArgs,
};

/// Maximum items per `subject batch-create` / `batch-update` call. Mirrors
/// the `MAX_BATCH_SIZE` cap enforced by the `animus.subject.batch-*` MCP
/// tools so both surfaces reject oversized payloads identically.
const MAX_BATCH_SIZE: usize = 100;

/// Result-envelope schema tag for CLI batch operations. Distinct from the
/// MCP `animus.mcp.batch.result.v1` tag so machine consumers can tell which
/// surface produced the envelope, while the body shape (summary + per-item
/// results) is identical.
const CLI_BATCH_RESULT_SCHEMA: &str = "animus.cli.batch.result.v1";

#[derive(Debug, Serialize)]
struct SubjectCallResponse {
    kind: String,
    verb: &'static str,
    method: String,
    plugin_count: usize,
    result: Value,
}

pub(crate) async fn handle_subject(command: SubjectCommand, project_root: &str, json: bool) -> Result<()> {
    match command {
        SubjectCommand::List(args) => handle_subject_list(args, project_root, json).await,
        SubjectCommand::Get(args) => handle_subject_get(args, project_root, json).await,
        SubjectCommand::Create(args) => handle_subject_create(args, project_root, json).await,
        SubjectCommand::BatchCreate(args) => handle_subject_batch_create(args, project_root, json).await,
        SubjectCommand::Update(args) => handle_subject_update(args, project_root, json).await,
        SubjectCommand::BatchUpdate(args) => handle_subject_batch_update(args, project_root, json).await,
        SubjectCommand::Next(args) => handle_subject_next(args, project_root, json).await,
        SubjectCommand::Status(args) => handle_subject_status(args, project_root, json).await,
        SubjectCommand::Delete(args) => handle_subject_delete(args, project_root, json).await,
    }
}

/// Default page size for `subject list` when no `--limit` is given. Bounds
/// MCP/agent list calls (the common token-bloat source) while `--limit 0`
/// returns everything.
const DEFAULT_SUBJECT_LIST_LIMIT: u32 = 50;

async fn handle_subject_list(args: SubjectListArgs, project_root: &str, json: bool) -> Result<()> {
    let kind = resolve_kind(args.kind.as_deref(), project_root, json)?;
    let query = args.query.as_deref().map(str::trim).filter(|s| !s.is_empty()).map(str::to_string);
    let mut filter = serde_json::Map::new();
    filter.insert("kind".to_string(), json!([kind]));
    if let Some(status) = args.status.as_deref() {
        filter.insert("status".to_string(), json!([status]));
    }
    // Default to a bounded page so MCP/agent callers (and a bare `subject list`)
    // don't pull the entire set; `--limit 0` opts out and returns ALL. Backends
    // that honor `limit` also return `total` + `next_cursor` for paging; backends
    // that ignore it simply return everything (no behavior change for them).
    let limit = args.limit.unwrap_or(DEFAULT_SUBJECT_LIST_LIMIT);
    // A --query lookup must see the WHOLE set (the backend has no title filter,
    // and the daemon drops unknown filter fields), so fetch unbounded and apply
    // both the title filter and the page limit client-side below.
    let fetch_limit = if query.is_some() { 0 } else { limit };
    if fetch_limit > 0 {
        filter.insert("limit".to_string(), json!(fetch_limit));
    }
    if let Some(cursor) = args.cursor.as_deref() {
        filter.insert("cursor".to_string(), json!(cursor));
    }
    let params = Some(Value::Object(filter));
    if let Some(q) = query {
        return dispatch_list_filtered(&kind, params, &q, limit, project_root, json).await;
    }
    dispatch(&kind, "list", params, project_root, json).await
}

/// `subject list --query`: fetch the full set, filter by a case-insensitive
/// substring of the subject TITLE, then apply the page `limit` to the matches.
/// Lets agents/UI resolve a subject by name without paging — and without the
/// agent-facing MCP result truncating a huge unfiltered list. Mirrors
/// `dispatch`'s output (json envelope vs human table); the result is the
/// filtered subset with an exact `total` and a null `next_cursor`.
async fn dispatch_list_filtered(
    kind: &str,
    params: Option<Value>,
    query: &str,
    limit: u32,
    project_root: &str,
    json: bool,
) -> Result<()> {
    let resolution = resolve_subject_dispatch(Path::new(project_root)).await?;
    let method = format!("{kind}/list");
    let raw = route_or_not_found(&resolution.selected, &method, params).await?;
    let needle = query.to_lowercase();
    let mut matches: Vec<Value> = extract_subjects(&raw)
        .into_iter()
        .filter(|s| s.get("title").and_then(Value::as_str).map(|t| t.to_lowercase().contains(&needle)).unwrap_or(false))
        .cloned()
        .collect();
    let total = matches.len() as u64;
    if limit > 0 {
        matches.truncate(limit as usize);
    }
    let result = json!({ "subjects": matches, "next_cursor": Value::Null, "total": total });
    if json {
        return print_value(
            SubjectCallResponse {
                kind: kind.to_string(),
                verb: "list",
                method,
                plugin_count: resolution.selected.plugin_count(),
                result,
            },
            true,
        );
    }
    render_subject_human("list", kind, &result);
    Ok(())
}

async fn handle_subject_get(args: SubjectGetArgs, project_root: &str, json: bool) -> Result<()> {
    let kind = resolve_kind(args.kind.as_deref(), project_root, json)?;
    if args.id.trim().is_empty() {
        return Err(invalid_input_error("--id must not be empty"));
    }
    let id = crate::qualify_subject_id(&args.id, &kind);
    let params = Some(json!({ "id": id }));
    dispatch(&kind, "get", params, project_root, json).await
}

async fn handle_subject_create(args: SubjectCreateArgs, project_root: &str, json: bool) -> Result<()> {
    let kind = resolve_kind(args.kind.as_deref(), project_root, json)?;
    let title = args.title.trim();
    if title.is_empty() {
        return Err(invalid_input_error("--title must not be empty"));
    }
    let mut payload = serde_json::Map::new();
    payload.insert("title".to_string(), json!(title));
    if let Some(status) = args.status.as_deref() {
        payload.insert("status".to_string(), json!(status));
    }
    if let Some(priority) = args.priority.as_deref() {
        payload.insert("priority".to_string(), json!(priority));
    }
    if !args.labels.is_empty() {
        payload.insert("labels".to_string(), json!(args.labels));
    }
    if let Some(body) = args.body.as_deref() {
        payload.insert("body".to_string(), json!(body));
    }
    let params = Some(Value::Object(payload));
    dispatch(&kind, "create", params, project_root, json).await
}

async fn handle_subject_update(args: SubjectUpdateArgs, project_root: &str, json: bool) -> Result<()> {
    let kind = resolve_kind(args.kind.as_deref(), project_root, json)?;
    if args.id.trim().is_empty() {
        return Err(invalid_input_error("--id must not be empty"));
    }
    let id = crate::qualify_subject_id(&args.id, &kind);
    let mut patch = serde_json::Map::new();
    if let Some(status) = args.status.as_deref() {
        patch.insert("status".to_string(), json!(status));
    }
    if let Some(priority) = args.priority.as_deref() {
        patch.insert("priority".to_string(), json!(priority));
    }
    if !args.labels.is_empty() {
        patch.insert("labels".to_string(), json!(args.labels));
    }
    if let Some(body) = args.body.as_deref() {
        patch.insert("body".to_string(), json!(body));
    }
    if patch.is_empty() {
        return Err(invalid_input_error(
            "subject update requires at least one of --status / --priority / --labels / --body",
        ));
    }
    let params = Some(json!({ "id": id, "patch": Value::Object(patch) }));
    dispatch(&kind, "update", params, project_root, json).await
}

/// One item of a `subject batch-create` request. Mirrors the MCP
/// `SubjectBatchCreateItem` shape so the same JSON items array works against
/// either surface.
#[derive(Debug, serde::Deserialize)]
struct BatchCreateItem {
    title: String,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    priority: Option<String>,
    #[serde(default)]
    labels: Vec<String>,
    #[serde(default)]
    body: Option<String>,
}

/// One item of a `subject batch-update` request. Mirrors the MCP
/// `SubjectBatchUpdateItem` shape.
#[derive(Debug, serde::Deserialize)]
struct BatchUpdateItem {
    id: String,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    priority: Option<String>,
    #[serde(default)]
    labels: Vec<String>,
}

/// Read and deserialize a JSON items array from `--file`. Accepts either a
/// bare array (`[ {..}, {..} ]`) or an object wrapper `{ "items": [..] }`
/// so payloads copied from the MCP tool input also work. Enforces the
/// non-empty + 100-item cap shared with the MCP tools.
fn read_batch_items<T: serde::de::DeserializeOwned>(file: &Path, tool_name: &str, kind: &str) -> Result<Vec<T>> {
    if kind.trim().is_empty() {
        return Err(invalid_input_error(format!("{tool_name}: kind must not be empty")));
    }
    let raw = std::fs::read_to_string(file).map_err(|err| {
        invalid_input_error(format!("{tool_name}: failed to read items file {}: {err}", file.display()))
    })?;
    let value: Value = serde_json::from_str(&raw).map_err(|err| {
        invalid_input_error(format!("{tool_name}: items file {} is not valid JSON: {err}", file.display()))
    })?;
    let items_value = match value {
        Value::Array(_) => value,
        Value::Object(ref map) if map.contains_key("items") => map.get("items").cloned().unwrap_or(Value::Null),
        _ => {
            return Err(invalid_input_error(format!(
                "{tool_name}: items file must be a JSON array, or an object with an \"items\" array"
            )))
        }
    };
    let items: Vec<T> = serde_json::from_value(items_value).map_err(|err| {
        invalid_input_error(format!("{tool_name}: items file {} has the wrong shape: {err}", file.display()))
    })?;
    if items.is_empty() {
        return Err(invalid_input_error(format!("{tool_name}: items must not be empty")));
    }
    if items.len() > MAX_BATCH_SIZE {
        return Err(invalid_input_error(format!(
            "{tool_name}: items count {} exceeds maximum {MAX_BATCH_SIZE}",
            items.len()
        )));
    }
    Ok(items)
}

/// Run pre-built per-item subject calls through one shared dispatch
/// resolution, honoring `on_error` (stop = remaining items are marked
/// skipped; continue = every item runs) and assembling an
/// `animus.cli.batch.result.v1` envelope whose per-item result shape matches
/// the MCP `run_batch_items` output. The `items` carry `(target_id, method,
/// params)` triples; routing goes through the same `route_or_not_found` path
/// the single-item verbs use.
async fn run_subject_batch(
    tool_name: &str,
    kind: &str,
    verb: &'static str,
    items: Vec<(String, Option<Value>)>,
    on_error: BatchOnError,
    project_root: &str,
    json: bool,
) -> Result<()> {
    let resolution = resolve_subject_dispatch(Path::new(project_root)).await?;
    let method = format!("{kind}/{verb}");
    let requested = items.len();
    let mut outcomes: Vec<Value> = Vec::with_capacity(requested);
    let mut stopped = false;
    let mut any_change = false;
    // Track the classified kind of every failure so the batch can preserve a
    // single-item verb's typed exit class when every item failed the same way
    // (e.g. no subject backend → all `Unavailable`/exit 5). `None` once two
    // distinct kinds are seen: a mixed-failure batch has no single class.
    let mut failure_kind: Option<crate::CliErrorKind> = None;
    let mut failure_kind_uniform = true;

    for (index, (target_id, params)) in items.into_iter().enumerate() {
        if stopped {
            outcomes.push(json!({
                "index": index,
                "status": "skipped",
                "target_id": target_id,
                "result": null,
                "error": null,
                "reason": "stopped after earlier failure",
            }));
            continue;
        }
        match route_or_not_found(&resolution.selected, &method, params).await {
            Ok(result) => {
                any_change = true;
                outcomes.push(json!({
                    "index": index,
                    "status": "success",
                    "target_id": target_id,
                    "result": result,
                }));
            }
            Err(err) => {
                let kind = crate::classify_cli_error_kind(&err);
                match failure_kind {
                    None => failure_kind = Some(kind),
                    Some(existing) if existing != kind => failure_kind_uniform = false,
                    Some(_) => {}
                }
                outcomes.push(json!({
                    "index": index,
                    "status": "failed",
                    "target_id": target_id,
                    "result": null,
                    "error": { "message": err.to_string() },
                }));
                if on_error.is_stop() {
                    stopped = true;
                }
            }
        }
    }

    // A successful write may have made new work dispatchable — wake a
    // running daemon once for the whole batch (best-effort; no-ops when the
    // daemon is down). Matches the single-item create/update/status nudge.
    if any_change {
        orchestrator_daemon_runtime::control::nudge_daemon_scheduler_best_effort(Path::new(project_root)).await;
    }

    let executed = outcomes.iter().filter(|o| o["status"] != "skipped").count();
    let succeeded = outcomes.iter().filter(|o| o["status"] == "success").count();
    let failed = outcomes.iter().filter(|o| o["status"] == "failed").count();
    let skipped = outcomes.iter().filter(|o| o["status"] == "skipped").count();

    let payload = json!({
        "schema": CLI_BATCH_RESULT_SCHEMA,
        "tool": tool_name,
        "kind": kind,
        "method": method,
        "plugin_count": resolution.selected.plugin_count(),
        "on_error": on_error.as_str(),
        "summary": {
            "requested": requested,
            "executed": executed,
            "succeeded": succeeded,
            "failed": failed,
            "skipped": skipped,
            "completed": failed == 0,
        },
        "results": outcomes,
    });

    if failed == 0 {
        return print_value(payload, json);
    }

    // One or more items failed: the command must exit non-zero so scripts
    // can detect the partial failure (codex review P2; mirrors the
    // `daemon preflight` pattern). Human mode pre-prints the full per-item
    // report to stderr before the error summary line; JSON mode carries the
    // same payload under `/error/details` in the `animus.cli.v1` error
    // envelope.
    if !json {
        if let Ok(rendered) = serde_json::to_string_pretty(&payload) {
            eprintln!("{rendered}");
        }
    }
    // Preserve the single-item verb's typed exit class when every *executed*
    // item failed for one uniform reason — e.g. no subject backend installed
    // maps to `Unavailable`/exit 5 just like `subject create` would, keeping
    // the batch verbs drop-in for scripts (codex review P2). Skipped items
    // (default `--on-error stop` after the first failure) don't dilute the
    // class: the only failures observed share one kind. A partial-success or
    // mixed-failure batch has no single class, so it stays `Internal`.
    let exit_kind = match failure_kind {
        Some(kind) if failure_kind_uniform && succeeded == 0 => kind,
        _ => crate::CliErrorKind::Internal,
    };
    Err(crate::CliError::new(
        exit_kind,
        format!("{tool_name}: {failed} of {requested} batch items failed (see details)"),
    )
    .with_details(payload)
    .into())
}

async fn handle_subject_batch_create(args: SubjectBatchCreateArgs, project_root: &str, json: bool) -> Result<()> {
    let tool_name = "animus.subject.batch-create";
    let kind = resolve_kind(args.kind.as_deref(), project_root, json)?;
    let items: Vec<BatchCreateItem> = read_batch_items(&args.file, tool_name, &kind)?;
    let mut calls: Vec<(String, Option<Value>)> = Vec::with_capacity(items.len());
    for (i, item) in items.iter().enumerate() {
        if item.title.trim().is_empty() {
            return Err(invalid_input_error(format!("{tool_name}: item[{i}].title must not be empty")));
        }
        let mut payload = serde_json::Map::new();
        payload.insert("title".to_string(), json!(item.title.trim()));
        if let Some(status) = item.status.as_deref() {
            payload.insert("status".to_string(), json!(status));
        }
        if let Some(priority) = item.priority.as_deref() {
            payload.insert("priority".to_string(), json!(priority));
        }
        if !item.labels.is_empty() {
            payload.insert("labels".to_string(), json!(item.labels));
        }
        if let Some(body) = item.body.as_deref() {
            payload.insert("body".to_string(), json!(body));
        }
        calls.push((item.title.trim().to_string(), Some(Value::Object(payload))));
    }
    run_subject_batch(tool_name, &kind, "create", calls, args.on_error, project_root, json).await
}

async fn handle_subject_batch_update(args: SubjectBatchUpdateArgs, project_root: &str, json: bool) -> Result<()> {
    let tool_name = "animus.subject.batch-update";
    let kind = resolve_kind(args.kind.as_deref(), project_root, json)?;
    let items: Vec<BatchUpdateItem> = read_batch_items(&args.file, tool_name, &kind)?;
    let mut calls: Vec<(String, Option<Value>)> = Vec::with_capacity(items.len());
    for (i, item) in items.iter().enumerate() {
        let id = item.id.trim();
        if id.is_empty() {
            return Err(invalid_input_error(format!("{tool_name}: item[{i}].id must not be empty")));
        }
        let mut patch = serde_json::Map::new();
        if let Some(status) = item.status.as_deref() {
            patch.insert("status".to_string(), json!(status));
        }
        if let Some(priority) = item.priority.as_deref() {
            patch.insert("priority".to_string(), json!(priority));
        }
        // TODO(codex-p2): an explicit `"labels": []` is currently treated as
        // an absent field (no label-clear forwarded), matching the single-item
        // `subject update --labels` verb and the create path. Forwarding an
        // explicit empty array as a clear would diverge batch-update from
        // those contracts; revisit only if the single-item verb gains the same
        // label-clear semantics so both surfaces stay aligned.
        if !item.labels.is_empty() {
            patch.insert("labels".to_string(), json!(item.labels));
        }
        if patch.is_empty() {
            return Err(invalid_input_error(format!(
                "{tool_name}: item[{i}] requires at least one of status / priority / labels"
            )));
        }
        calls.push((id.to_string(), Some(json!({ "id": id, "patch": Value::Object(patch) }))));
    }
    run_subject_batch(tool_name, &kind, "update", calls, args.on_error, project_root, json).await
}

async fn handle_subject_next(args: SubjectNextArgs, project_root: &str, json: bool) -> Result<()> {
    let kind = resolve_kind(args.kind.as_deref(), project_root, json)?;
    dispatch(&kind, "next", None, project_root, json).await
}

async fn handle_subject_status(args: SubjectStatusArgs, project_root: &str, json: bool) -> Result<()> {
    let kind = resolve_kind(args.kind.as_deref(), project_root, json)?;
    if args.id.trim().is_empty() {
        return Err(invalid_input_error("--id must not be empty"));
    }
    let id = crate::qualify_subject_id(&args.id, &kind);
    let id = id.as_str();
    let status = args.status.trim();
    if status.is_empty() {
        return Err(invalid_input_error("--status must not be empty"));
    }

    let resolution = resolve_subject_dispatch(Path::new(project_root)).await?;
    // Snapshot the subject before the transition so the human output can
    // report which stuck-state flags (`paused`, `blocked_*`) the backend
    // cleared. Best-effort: a failed pre-fetch never blocks the transition.
    let before = if json {
        None
    } else {
        route_or_not_found(&resolution.selected, &format!("{kind}/get"), Some(json!({ "id": id }))).await.ok()
    };
    let method = format!("{kind}/status");
    let result = route_or_not_found(&resolution.selected, &method, Some(json!({ "id": id, "status": status }))).await?;
    orchestrator_daemon_runtime::control::nudge_daemon_scheduler_best_effort(Path::new(project_root)).await;
    if let Some(before) = before.as_ref() {
        if let Some(line) = describe_cleared_block_flags(before, &result) {
            eprintln!("{line}");
        }
    }
    if json {
        return print_value(
            SubjectCallResponse {
                kind: kind.to_string(),
                verb: "status",
                method,
                plugin_count: resolution.selected.plugin_count(),
                result,
            },
            true,
        );
    }
    render_subject_human("status", &kind, &result);
    Ok(())
}

/// Compare a subject's pre/post-transition JSON and describe which
/// stuck-state flags the transition cleared (`paused: true -> false`,
/// `blocked_reason` / `blocked_by` / `blocked_at` / `blocked_phase`
/// set -> null). Returns `None` when nothing was cleared. Pure so it can be
/// unit-tested without spawning subject plugins.
fn describe_cleared_block_flags(before: &Value, after: &Value) -> Option<String> {
    fn subject_object(value: &Value) -> Option<&serde_json::Map<String, Value>> {
        let object = value.as_object()?;
        for key in ["subject", "task", "item"] {
            if let Some(nested) = object.get(key).and_then(Value::as_object) {
                return Some(nested);
            }
        }
        Some(object)
    }

    let before = subject_object(before)?;
    let after = subject_object(after)?;
    let mut cleared = Vec::new();
    if before.get("paused").and_then(Value::as_bool) == Some(true)
        && after.get("paused").and_then(Value::as_bool) == Some(false)
    {
        cleared.push("paused".to_string());
    }
    for key in ["blocked_reason", "blocked_by", "blocked_at", "blocked_phase"] {
        let Some(previous) = before.get(key).filter(|value| !value.is_null()) else {
            continue;
        };
        if after.get(key).map(Value::is_null).unwrap_or(true) {
            match previous.as_str() {
                Some(text) => cleared.push(format!("{key} (\"{text}\")")),
                None => cleared.push(key.to_string()),
            }
        }
    }
    if cleared.is_empty() {
        None
    } else {
        Some(format!("unstuck: cleared {}", cleared.join(", ")))
    }
}

async fn handle_subject_delete(args: SubjectDeleteArgs, project_root: &str, json: bool) -> Result<()> {
    let kind = resolve_kind(args.kind.as_deref(), project_root, json)?;
    if args.id.trim().is_empty() {
        return Err(invalid_input_error("--id must not be empty"));
    }
    let id = crate::qualify_subject_id(&args.id, &kind);
    let id = id.as_str();
    if !args.yes {
        let preview = json!({
            "kind": kind,
            "verb": "delete",
            "id": id,
            "would_delete": true,
            "hint": "re-run with --yes to actually delete",
        });
        print_value(&preview, json)?;
        return Ok(());
    }
    let params = Some(json!({ "id": id }));
    dispatch(&kind, "delete", params, project_root, json).await
}

/// Resolve the `--kind` value used for `animus subject <verb>`.
///
/// Precedence:
///
/// 1. `--kind` on the command line (must be non-empty, no `/`).
/// 2. `default_subject_kind` from `.animus/config.json`.
/// 3. Error: ask the user to pass `--kind` or set `default_subject_kind`.
///
/// When the config default is used and `json` is false, a one-line hint is
/// printed to stderr so the user knows which kind was silently selected.
///
/// The resolved kind is returned as an owned `String` so callers don't
/// have to keep `args` alive across the dispatch await.
fn resolve_kind(raw: Option<&str>, project_root: &str, json: bool) -> Result<String> {
    if let Some(value) = raw {
        return validate_kind(value).map(|s| s.to_string());
    }
    let config = Config::load_or_default(project_root)
        .map_err(|err| anyhow!("failed to load project config from '{project_root}': {err}"))?;
    match config.default_subject_kind.as_deref().and_then(|s| {
        let trimmed = s.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed)
        }
    }) {
        Some(default) => {
            let kind = validate_kind(default).map(|s| s.to_string())?;
            if !json {
                eprintln!(
                    "(using default kind '{kind}'; pass --kind or set default_subject_kind in .animus/config.json)"
                );
            }
            Ok(kind)
        }
        None => Err(invalid_input_error(
            "no subject kind supplied. Pass `--kind <kind>` or set `default_subject_kind` in .animus/config.json. \
             Run `animus plugin list` to see installed subject_backend kinds.",
        )),
    }
}

fn validate_kind(raw: &str) -> Result<&str> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(invalid_input_error("--kind must not be empty"));
    }
    if trimmed.contains('/') {
        return Err(invalid_input_error("--kind must not contain '/'"));
    }
    Ok(trimmed)
}

/// Build the daemon-side subject dispatch (spawns each installed
/// subject_backend plugin in one-shot mode), route `<kind>/<verb>`
/// through it, and render the response under the `animus.cli.v1`
/// envelope.
///
/// When the daemon is already running we will eventually want the CLI
/// to forward via MCP/IPC to reuse the existing plugin processes; for
/// v0.4.0 the CLI always spawns its own short-lived hosts so the
/// command works whether or not the daemon is up. The plugin host
/// shutdown is implicit (handles dropped at function return), matching
/// `animus plugin call`'s pattern.
async fn dispatch(kind: &str, verb: &'static str, params: Option<Value>, project_root: &str, json: bool) -> Result<()> {
    let resolution = resolve_subject_dispatch(Path::new(project_root)).await?;
    let method = format!("{kind}/{verb}");
    let result = route_or_not_found(&resolution.selected, &method, params).await?;
    // Write verbs may have made new work dispatchable (e.g. a task flipped
    // to Ready) — wake a running daemon so it picks the change up now
    // instead of on the next heartbeat. Fire-and-forget: silently no-ops
    // when the daemon is down or predates `daemon/nudge`. MCP subject
    // tools execute these CLI handlers in a subprocess, so this single
    // choke point covers both surfaces.
    if matches!(verb, "create" | "update" | "status") {
        orchestrator_daemon_runtime::control::nudge_daemon_scheduler_best_effort(Path::new(project_root)).await;
    }
    if json {
        return print_value(
            SubjectCallResponse {
                kind: kind.to_string(),
                verb,
                method,
                plugin_count: resolution.selected.plugin_count(),
                result,
            },
            true,
        );
    }
    render_subject_human(verb, kind, &result);
    Ok(())
}

/// Render a subject call result for human (non-`--json`) output, stripping the
/// routing envelope (kind/verb/method/plugin_count) entirely. `list` renders a
/// table; `get`/`next`/`create`/`update`/`status` render a readable
/// key:value block. Falls back to a single line for anything unexpected.
fn render_subject_human(verb: &str, kind: &str, result: &Value) {
    match verb {
        "list" => {
            let subjects = extract_subjects(result);
            let shown = subjects.len();
            render_subject_table(&subjects);
            render_list_pagination_footer(result, shown);
        }
        "next" => match extract_single_subject(result) {
            Some(subject) => render_subject_block(subject),
            None => println!("no ready {kind} subject"),
        },
        "delete" => println!("deleted {kind} subject"),
        _ => match extract_single_subject(result) {
            Some(subject) => render_subject_block(subject),
            None => println!("{result}"),
        },
    }
}

/// Print a one-line pagination footer for `subject list` human output when the
/// backend reports another page, so the bounded default page isn't silently
/// truncated. No-op when there's no non-empty `next_cursor`.
fn render_list_pagination_footer(result: &Value, shown: usize) {
    let Some(cursor) = result.get("next_cursor").and_then(Value::as_str).filter(|c| !c.is_empty()) else {
        return;
    };
    match result.get("total").and_then(Value::as_u64) {
        Some(total) => {
            println!("\nshowing {shown} of {total} — next page: --cursor {cursor}  (all: --limit 0)")
        }
        None => {
            println!("\nshowing {shown} — more available, next page: --cursor {cursor}  (all: --limit 0)")
        }
    }
}

/// Pull the array of subject objects out of a `<kind>/list` result. Accepts a
/// `{ "subjects": [...] }` wrapper (the protocol shape), a bare array, or a
/// single object.
fn extract_subjects(result: &Value) -> Vec<&Value> {
    if let Some(array) = result.get("subjects").and_then(Value::as_array) {
        return array.iter().collect();
    }
    match result {
        Value::Array(items) => items.iter().collect(),
        Value::Object(_) => vec![result],
        _ => Vec::new(),
    }
}

/// Unwrap a single subject object from a `get`/`next`/`create`/`update`/
/// `status` result, tolerating a `{ "subject": {...} }` wrapper, a `null`
/// (no subject), or a bare object. Requires the object to be subject-shaped
/// (carry an `id`) so non-subject responses (e.g. a `{ "ok": true }` ack) fall
/// through to the caller's raw fallback rather than rendering blank fields.
fn extract_single_subject(result: &Value) -> Option<&Value> {
    if result.is_null() {
        return None;
    }
    if let Some(inner) = result.get("subject") {
        return inner.as_object().filter(|_| inner.get("id").is_some()).map(|_| inner);
    }
    result.as_object().filter(|_| result.get("id").is_some()).map(|_| result)
}

/// Render a list of subjects as a fixed-width table:
/// `ID  STATUS  PRI  TITLE  BLOCKED_REASON  UPDATED`.
fn render_subject_table(subjects: &[&Value]) {
    if subjects.is_empty() {
        println!("no subjects found");
        return;
    }
    let rows: Vec<Vec<String>> = subjects
        .iter()
        .map(|subject| {
            vec![
                subject_field_str(subject, "id"),
                subject_field_str(subject, "status"),
                crate::format_priority(subject.get("priority")),
                subject_field_str(subject, "title"),
                subject_blocked_reason(subject),
                subject_updated(subject),
            ]
        })
        .collect();
    crate::render_table(&["ID", "STATUS", "PRI", "TITLE", "BLOCKED_REASON", "UPDATED"], &rows);
}

/// Render a single subject as an aligned `key: value` block.
fn render_subject_block(subject: &Value) {
    let fields: &[(&str, String)] = &[
        ("id", subject_field_str(subject, "id")),
        ("kind", subject_field_str(subject, "kind")),
        ("title", subject_field_str(subject, "title")),
        ("status", subject_field_str(subject, "status")),
        ("priority", crate::format_priority(subject.get("priority"))),
    ];
    let width = fields.iter().map(|(k, _)| k.len()).max().unwrap_or(0);
    for (key, value) in fields {
        println!("{key:<width$}  {value}");
    }
    let blocked = subject_blocked_reason(subject);
    if blocked != "--" {
        println!("{:<width$}  {blocked}", "blocked");
    }
    if let Some(labels) = subject.get("labels").and_then(Value::as_array) {
        if !labels.is_empty() {
            let joined = labels.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(", ");
            println!("{:<width$}  {joined}", "labels");
        }
    }
    let updated = subject_updated(subject);
    if updated != "--" {
        println!("{:<width$}  {updated}", "updated");
    }
    if let Some(description) = subject.get("description").and_then(Value::as_str) {
        if !description.trim().is_empty() {
            println!("\n{description}");
        }
    }
}

fn subject_field_str(subject: &Value, key: &str) -> String {
    match subject.get(key) {
        Some(Value::String(s)) if !s.is_empty() => s.clone(),
        Some(Value::String(_)) | None | Some(Value::Null) => "--".to_string(),
        Some(other) => other.to_string(),
    }
}

/// Resolve a subject's blocked reason from either a top-level field or the
/// backend-specific `custom` map. Returns `--` when not blocked.
fn subject_blocked_reason(subject: &Value) -> String {
    for source in [Some(subject), subject.get("custom")] {
        let Some(source) = source else { continue };
        if let Some(reason) = source.get("blocked_reason").and_then(Value::as_str) {
            if !reason.trim().is_empty() {
                return reason.to_string();
            }
        }
    }
    "--".to_string()
}

/// Format the subject's `updated_at` timestamp as a date (`YYYY-MM-DD`),
/// trimming the time component for table density.
fn subject_updated(subject: &Value) -> String {
    match subject.get("updated_at").and_then(Value::as_str) {
        Some(ts) if !ts.is_empty() => ts.split('T').next().unwrap_or(ts).to_string(),
        _ => "--".to_string(),
    }
}

async fn route_or_not_found(dispatch: &SubjectPluginDispatch, method: &str, params: Option<Value>) -> Result<Value> {
    match dispatch.route_call(method, params).await {
        Ok(value) => Ok(value),
        Err(rpc_error) => Err(classify_subject_rpc_error(method, &rpc_error)),
    }
}

/// Map a subject backend [`animus_plugin_protocol::RpcError`] onto the CLI's
/// typed exit-code families so scripts can distinguish "you typed the wrong
/// id" (2/3) from "no plugin is mounted" (5) from genuine internal faults (1).
fn classify_subject_rpc_error(method: &str, rpc_error: &animus_plugin_protocol::RpcError) -> anyhow::Error {
    let message = format!("subject call '{method}' failed ({}): {}", rpc_error.code, rpc_error.message);
    let lower = rpc_error.message.to_ascii_lowercase();
    // The dispatch/router layers emit deterministic "no subject backend
    // mounted/registered for kind '<kind>'" messages when no plugin can
    // serve the kind — that's a missing/unreachable plugin, not a missing
    // subject.
    if lower.contains("no subject backend") {
        return crate::error_with_remediation(
            crate::CliErrorKind::Unavailable,
            format!("{message}; install one with `animus plugin install-defaults --include-subjects`"),
            crate::missing_plugin_remediation(
                "animus plugin install-defaults --include-subjects",
                "Install a subject_backend plugin that serves this kind, then retry.",
            ),
        );
    }
    if rpc_error.code == error_codes::INVALID_PARAMS {
        return invalid_input_error(message);
    }
    if matches!(
        rpc_error.code,
        error_codes::TIMEOUT | error_codes::REQUEST_CANCELLED | error_codes::PLUGIN_NOT_INITIALIZED
    ) {
        return unavailable_error(message);
    }
    if lower.contains("not found") || lower.contains("does not exist") || lower.contains("no such") {
        return not_found_error(message);
    }
    anyhow!(message)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_items(dir: &std::path::Path, name: &str, contents: &str) -> std::path::PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, contents).expect("write items file");
        path
    }

    #[test]
    fn read_batch_items_accepts_bare_array_and_items_wrapper() {
        let tmp = tempfile::tempdir().expect("tmp");
        let bare = write_items(tmp.path(), "bare.json", r#"[{"title":"a"},{"title":"b"}]"#);
        let items: Vec<BatchCreateItem> =
            read_batch_items(&bare, "animus.subject.batch-create", "task").expect("bare array parses");
        assert_eq!(items.len(), 2);

        let wrapped = write_items(tmp.path(), "wrapped.json", r#"{"items":[{"title":"a"}]}"#);
        let items: Vec<BatchCreateItem> =
            read_batch_items(&wrapped, "animus.subject.batch-create", "task").expect("items wrapper parses");
        assert_eq!(items.len(), 1);
    }

    #[test]
    fn read_batch_items_enforces_empty_cap_and_shape() {
        let tmp = tempfile::tempdir().expect("tmp");

        let empty = write_items(tmp.path(), "empty.json", "[]");
        let err = read_batch_items::<BatchCreateItem>(&empty, "animus.subject.batch-create", "task")
            .expect_err("empty rejected");
        assert!(err.to_string().contains("items must not be empty"), "got: {err}");

        let big_items: Vec<String> = (0..101).map(|i| format!(r#"{{"title":"t{i}"}}"#)).collect();
        let big = write_items(tmp.path(), "big.json", &format!("[{}]", big_items.join(",")));
        let err = read_batch_items::<BatchCreateItem>(&big, "animus.subject.batch-create", "task")
            .expect_err("over cap rejected");
        assert!(err.to_string().contains("exceeds maximum 100"), "got: {err}");

        let not_array = write_items(tmp.path(), "obj.json", r#"{"title":"a"}"#);
        let err = read_batch_items::<BatchCreateItem>(&not_array, "animus.subject.batch-create", "task")
            .expect_err("non-array rejected");
        assert!(err.to_string().contains("must be a JSON array"), "got: {err}");
    }

    #[test]
    fn read_batch_items_rejects_empty_kind() {
        let tmp = tempfile::tempdir().expect("tmp");
        let file = write_items(tmp.path(), "x.json", r#"[{"title":"a"}]"#);
        let err = read_batch_items::<BatchCreateItem>(&file, "animus.subject.batch-create", "  ")
            .expect_err("empty kind rejected");
        assert!(err.to_string().contains("kind must not be empty"), "got: {err}");
    }

    #[tokio::test]
    // The test-env serialization lock is intentionally held across the await to
    // keep HOME / the disable-plugins env stable for the whole async test body.
    #[allow(clippy::await_holding_lock)]
    async fn run_subject_batch_continue_marks_each_item_failed_without_backend() {
        // Isolated from any globally installed subject plugins (codex review
        // P2): pin HOME to a temp dir and force subject plugin discovery off
        // so the dispatch is deterministically empty and no real subjects
        // are ever created by the test.
        let _lock = crate::shared::test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let tmp = tempfile::tempdir().expect("tmp");
        let _home = protocol::test_utils::EnvVarGuard::set("HOME", Some(tmp.path().to_string_lossy().as_ref()));
        let _disable =
            protocol::test_utils::EnvVarGuard::set(orchestrator_daemon_runtime::SUBJECT_PLUGINS_DISABLE_ENV, Some("1"));
        let project_root = tmp.path().to_str().expect("utf-8");
        let calls =
            vec![("a".to_string(), Some(json!({ "title": "a" }))), ("b".to_string(), Some(json!({ "title": "b" })))];
        // Every item fails (empty dispatch). With on_error=continue all items
        // execute; the command exits non-zero with the batch payload attached
        // as structured details so scripts can detect the partial failure.
        let res = run_subject_batch(
            "animus.subject.batch-create",
            "task",
            "create",
            calls,
            BatchOnError::Continue,
            project_root,
            true,
        )
        .await;
        let err = res.expect_err("a batch where items failed must exit non-zero");
        // Every item failed for the same reason (no subject backend mounted),
        // so the batch preserves that single-item typed exit class
        // (`Unavailable`/exit 5) rather than collapsing to `Internal`.
        assert_eq!(crate::classify_cli_error_kind(&err), crate::CliErrorKind::Unavailable);
        assert!(err.to_string().contains("2 of 2 batch items failed"), "got: {err}");
        let details = crate::extract_cli_error_details(&err).expect("error details carry the batch payload");
        assert_eq!(details.pointer("/schema").and_then(serde_json::Value::as_str), Some("animus.cli.batch.result.v1"));
        assert_eq!(details.pointer("/summary/failed").and_then(serde_json::Value::as_u64), Some(2));
        assert_eq!(details.pointer("/summary/executed").and_then(serde_json::Value::as_u64), Some(2));
        assert_eq!(
            details.pointer("/results/0/status").and_then(serde_json::Value::as_str),
            Some("failed"),
            "per-item results must be present in details"
        );
    }

    #[test]
    fn validate_kind_rejects_empty_and_slash() {
        assert!(validate_kind("").is_err(), "empty kind rejected");
        assert!(validate_kind("   ").is_err(), "whitespace kind rejected");
        assert!(validate_kind("task/list").is_err(), "kind containing '/' rejected");
        assert_eq!(validate_kind(" task ").expect("trimmed"), "task");
    }

    #[test]
    fn resolve_kind_prefers_explicit_arg() {
        let tmp = tempfile::tempdir().expect("tmp");
        let project_root = tmp.path().to_str().expect("utf-8");
        let resolved = resolve_kind(Some("issue"), project_root, false).expect("resolves");
        assert_eq!(resolved, "issue");
    }

    #[test]
    fn resolve_kind_falls_back_to_config_default() {
        let tmp = tempfile::tempdir().expect("tmp");
        let project_root = tmp.path().to_str().expect("utf-8");
        // Default `Config::load_or_default` writes `default_subject_kind: "task"`.
        let _ = Config::load_or_default(project_root).expect("seed config");
        let resolved = resolve_kind(None, project_root, true).expect("resolves from default");
        assert_eq!(resolved, "task");
    }

    #[test]
    fn resolve_kind_errors_when_neither_arg_nor_default_present() {
        use std::fs;
        let tmp = tempfile::tempdir().expect("tmp");
        let project_root = tmp.path();
        let animus_dir = project_root.join(".animus");
        fs::create_dir_all(&animus_dir).expect("create .animus");
        fs::write(animus_dir.join("config.json"), serde_json::json!({ "agent_runner_token": null }).to_string())
            .expect("seed config");
        let err = resolve_kind(None, project_root.to_str().expect("utf-8"), false)
            .expect_err("must error when no default and no flag");
        let message = err.to_string();
        assert!(
            message.contains("--kind") || message.contains("default_subject_kind"),
            "error names the missing input: {message}"
        );
    }

    #[test]
    fn resolve_kind_emits_hint_in_human_mode_when_using_config_default() {
        let tmp = tempfile::tempdir().expect("tmp");
        let project_root = tmp.path().to_str().expect("utf-8");
        let _ = Config::load_or_default(project_root).expect("seed config");
        // json=true must not emit hint (no panic, just silently returns the kind)
        let resolved_json = resolve_kind(None, project_root, true).expect("resolves in json mode");
        assert_eq!(resolved_json, "task");
        // json=false resolves the same kind; the eprintln is a side-effect we can't
        // assert on without capturing stderr, but we confirm resolution is correct.
        let resolved_human = resolve_kind(None, project_root, false).expect("resolves in human mode");
        assert_eq!(resolved_human, "task");
    }

    #[tokio::test]
    async fn route_or_not_found_returns_unavailable_for_empty_dispatch() {
        let dispatch = SubjectPluginDispatch::empty();
        let err = route_or_not_found(&dispatch, "task/list", None).await.expect_err("expect Unavailable");
        let message = err.to_string();
        assert!(message.contains("task"), "error message names kind: {message}");
        assert!(
            message.contains("subject call") || message.contains("no subject backend"),
            "error includes routing context: {message}"
        );
        assert!(message.contains("install-defaults --include-subjects"), "error carries install hint: {message}");
        assert_eq!(crate::classify_cli_error_kind(&err), crate::CliErrorKind::Unavailable);
    }

    #[test]
    fn classify_subject_rpc_error_missing_backend_carries_structured_remediation() {
        use animus_plugin_protocol::RpcError;
        let rpc_error = RpcError {
            code: error_codes::INTERNAL_ERROR,
            message: "no subject backend mounted for kind 'task'".into(),
            data: None,
        };
        let err = classify_subject_rpc_error("task/list", &rpc_error);
        assert_eq!(crate::classify_cli_error_kind(&err), crate::CliErrorKind::Unavailable, "kind unchanged");
        assert!(err.to_string().contains("install one with `animus plugin install-defaults --include-subjects`"));
        let details = crate::extract_cli_error_details(&err).expect("structured remediation details");
        assert_eq!(details.pointer("/remediation/kind").and_then(serde_json::Value::as_str), Some("missing_plugin"));
        assert_eq!(
            details.pointer("/remediation/install_command").and_then(serde_json::Value::as_str),
            Some("animus plugin install-defaults --include-subjects")
        );
        assert!(details.pointer("/remediation/next_step").and_then(serde_json::Value::as_str).is_some());
    }

    #[test]
    fn classify_subject_rpc_error_maps_rpc_codes_to_typed_kinds() {
        use animus_plugin_protocol::RpcError;
        let cases = [
            (
                RpcError { code: error_codes::INVALID_PARAMS, message: "bad patch shape".into(), data: None },
                crate::CliErrorKind::InvalidInput,
            ),
            (
                RpcError { code: error_codes::TIMEOUT, message: "request timed out".into(), data: None },
                crate::CliErrorKind::Unavailable,
            ),
            (
                RpcError {
                    code: error_codes::INTERNAL_ERROR,
                    message: "subject 'task:TASK-9' not found".into(),
                    data: None,
                },
                crate::CliErrorKind::NotFound,
            ),
            (
                RpcError { code: error_codes::INTERNAL_ERROR, message: "store corrupted".into(), data: None },
                crate::CliErrorKind::Internal,
            ),
        ];
        for (rpc_error, expected) in cases {
            let err = classify_subject_rpc_error("task/get", &rpc_error);
            assert_eq!(crate::classify_cli_error_kind(&err), expected, "rpc message: {}", rpc_error.message);
        }
    }

    #[test]
    fn describe_cleared_block_flags_reports_paused_and_blocked_fields() {
        let before = json!({
            "id": "task:TASK-9",
            "status": "blocked",
            "paused": true,
            "blocked_reason": "paused by workflow wf-123",
            "blocked_by": "wf-123",
            "blocked_at": "2026-06-10T12:00:00Z",
        });
        let after = json!({
            "id": "task:TASK-9",
            "status": "ready",
            "paused": false,
            "blocked_reason": null,
            "blocked_by": null,
            "blocked_at": null,
        });
        let line = describe_cleared_block_flags(&before, &after).expect("cleared flags reported");
        assert!(line.starts_with("unstuck: cleared "), "got: {line}");
        assert!(line.contains("paused"), "got: {line}");
        assert!(line.contains("blocked_reason (\"paused by workflow wf-123\")"), "got: {line}");
        assert!(line.contains("blocked_by (\"wf-123\")"), "got: {line}");
        assert!(line.contains("blocked_at"), "got: {line}");
    }

    #[test]
    fn describe_cleared_block_flags_handles_nested_subject_payloads() {
        let before = json!({ "subject": { "paused": true, "blocked_reason": "stuck" } });
        let after = json!({ "subject": { "paused": false } });
        let line = describe_cleared_block_flags(&before, &after).expect("nested payload diffed");
        assert!(line.contains("paused"), "got: {line}");
        assert!(line.contains("blocked_reason (\"stuck\")"), "got: {line}");
    }

    #[test]
    fn describe_cleared_block_flags_is_silent_when_nothing_cleared() {
        let clean_before = json!({ "id": "task:TASK-1", "status": "ready", "paused": false });
        let clean_after = json!({ "id": "task:TASK-1", "status": "in-progress", "paused": false });
        assert!(describe_cleared_block_flags(&clean_before, &clean_after).is_none());

        // Still-blocked transitions must not claim a clear.
        let blocked = json!({ "paused": true, "blocked_reason": "dep gate" });
        assert!(describe_cleared_block_flags(&blocked, &blocked).is_none());
        // Non-object payloads are ignored gracefully.
        assert!(describe_cleared_block_flags(&json!("ok"), &json!("ok")).is_none());
    }

    #[test]
    fn extract_subjects_unwraps_protocol_and_bare_shapes() {
        let wrapped = json!({ "subjects": [ { "id": "task:T-1" }, { "id": "task:T-2" } ] });
        assert_eq!(extract_subjects(&wrapped).len(), 2);
        let bare = json!([ { "id": "task:T-1" } ]);
        assert_eq!(extract_subjects(&bare).len(), 1);
        let single = json!({ "id": "task:T-1", "title": "x" });
        assert_eq!(extract_subjects(&single).len(), 1);
        assert!(extract_subjects(&json!(null)).is_empty());
    }

    #[test]
    fn extract_single_subject_handles_wrappers_and_null() {
        let wrapped = json!({ "subject": { "id": "task:T-1" } });
        assert!(extract_single_subject(&wrapped).is_some());
        let bare = json!({ "id": "task:T-1" });
        assert!(extract_single_subject(&bare).is_some());
        assert!(extract_single_subject(&json!(null)).is_none());
        // A non-subject ack (no `id`) is not treated as a subject.
        assert!(extract_single_subject(&json!({ "ok": true })).is_none());
        assert!(extract_single_subject(&json!({ "subject": { "ok": true } })).is_none());
    }

    #[test]
    fn subject_blocked_reason_reads_top_level_and_custom() {
        let top = json!({ "blocked_reason": "dep gate" });
        assert_eq!(subject_blocked_reason(&top), "dep gate");
        let nested = json!({ "custom": { "blocked_reason": "waiting on infra" } });
        assert_eq!(subject_blocked_reason(&nested), "waiting on infra");
        let clean = json!({ "id": "task:T-1" });
        assert_eq!(subject_blocked_reason(&clean), "--");
    }

    #[test]
    fn subject_updated_trims_time_component() {
        let s = json!({ "updated_at": "2026-06-10T12:00:00Z" });
        assert_eq!(subject_updated(&s), "2026-06-10");
        assert_eq!(subject_updated(&json!({})), "--");
    }

    #[test]
    fn subject_field_str_falls_back_to_dashes() {
        let s = json!({ "id": "task:T-1", "title": "" });
        assert_eq!(subject_field_str(&s, "id"), "task:T-1");
        assert_eq!(subject_field_str(&s, "title"), "--");
        assert_eq!(subject_field_str(&s, "missing"), "--");
    }

    #[test]
    fn validation_errors_classify_as_invalid_input() {
        for err in [
            validate_kind("").expect_err("empty kind"),
            validate_kind("task/list").expect_err("slash kind"),
            resolve_kind(Some(""), "/tmp/does-not-matter", false).expect_err("empty explicit kind"),
        ] {
            assert_eq!(crate::classify_cli_error_kind(&err), crate::CliErrorKind::InvalidInput, "{err}");
        }
    }
}
