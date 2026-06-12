use std::path::Path;

use animus_plugin_protocol::error_codes;
use anyhow::{anyhow, Result};
use orchestrator_daemon_runtime::{resolve_subject_dispatch, SubjectPluginDispatch};
use protocol::Config;
use serde::Serialize;
use serde_json::{json, Value};

use crate::{
    invalid_input_error, not_found_error, print_value, unavailable_error, SubjectCommand, SubjectCreateArgs,
    SubjectDeleteArgs, SubjectGetArgs, SubjectListArgs, SubjectNextArgs, SubjectStatusArgs, SubjectUpdateArgs,
};

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
        SubjectCommand::Update(args) => handle_subject_update(args, project_root, json).await,
        SubjectCommand::Next(args) => handle_subject_next(args, project_root, json).await,
        SubjectCommand::Status(args) => handle_subject_status(args, project_root, json).await,
        SubjectCommand::Delete(args) => handle_subject_delete(args, project_root, json).await,
    }
}

async fn handle_subject_list(args: SubjectListArgs, project_root: &str, json: bool) -> Result<()> {
    let kind = resolve_kind(args.kind.as_deref(), project_root, json)?;
    let mut filter = serde_json::Map::new();
    filter.insert("kind".to_string(), json!([kind]));
    if let Some(status) = args.status.as_deref() {
        filter.insert("status".to_string(), json!([status]));
    }
    if let Some(limit) = args.limit {
        filter.insert("limit".to_string(), json!(limit));
    }
    let params = Some(Value::Object(filter));
    dispatch(&kind, "list", params, project_root, json).await
}

async fn handle_subject_get(args: SubjectGetArgs, project_root: &str, json: bool) -> Result<()> {
    let kind = resolve_kind(args.kind.as_deref(), project_root, json)?;
    let id = args.id.trim();
    if id.is_empty() {
        return Err(invalid_input_error("--id must not be empty"));
    }
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
    let id = args.id.trim();
    if id.is_empty() {
        return Err(invalid_input_error("--id must not be empty"));
    }
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
    if patch.is_empty() {
        return Err(invalid_input_error("subject update requires at least one of --status / --priority / --labels"));
    }
    let params = Some(json!({ "id": id, "patch": Value::Object(patch) }));
    dispatch(&kind, "update", params, project_root, json).await
}

async fn handle_subject_next(args: SubjectNextArgs, project_root: &str, json: bool) -> Result<()> {
    let kind = resolve_kind(args.kind.as_deref(), project_root, json)?;
    dispatch(&kind, "next", None, project_root, json).await
}

async fn handle_subject_status(args: SubjectStatusArgs, project_root: &str, json: bool) -> Result<()> {
    let kind = resolve_kind(args.kind.as_deref(), project_root, json)?;
    let id = args.id.trim();
    if id.is_empty() {
        return Err(invalid_input_error("--id must not be empty"));
    }
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
    print_value(
        SubjectCallResponse {
            kind: kind.to_string(),
            verb: "status",
            method,
            plugin_count: resolution.selected.plugin_count(),
            result,
        },
        json,
    )
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
    let id = args.id.trim();
    if id.is_empty() {
        return Err(invalid_input_error("--id must not be empty"));
    }
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
    print_value(
        SubjectCallResponse {
            kind: kind.to_string(),
            verb,
            method,
            plugin_count: resolution.selected.plugin_count(),
            result,
        },
        json,
    )
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
