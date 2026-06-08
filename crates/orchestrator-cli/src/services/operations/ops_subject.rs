use std::path::Path;

use anyhow::{anyhow, Result};
use orchestrator_daemon_runtime::{resolve_subject_dispatch, SubjectPluginDispatch};
use protocol::Config;
use serde::Serialize;
use serde_json::{json, Value};

use crate::{
    print_value, SubjectCommand, SubjectCreateArgs, SubjectDeleteArgs, SubjectGetArgs, SubjectListArgs,
    SubjectNextArgs, SubjectStatusArgs, SubjectUpdateArgs,
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
    let kind = resolve_kind(args.kind.as_deref(), project_root)?;
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
    let kind = resolve_kind(args.kind.as_deref(), project_root)?;
    let id = args.id.trim();
    if id.is_empty() {
        return Err(anyhow!("--id must not be empty"));
    }
    let params = Some(json!({ "id": id }));
    dispatch(&kind, "get", params, project_root, json).await
}

async fn handle_subject_create(args: SubjectCreateArgs, project_root: &str, json: bool) -> Result<()> {
    let kind = resolve_kind(args.kind.as_deref(), project_root)?;
    let title = args.title.trim();
    if title.is_empty() {
        return Err(anyhow!("--title must not be empty"));
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
    let kind = resolve_kind(args.kind.as_deref(), project_root)?;
    let id = args.id.trim();
    if id.is_empty() {
        return Err(anyhow!("--id must not be empty"));
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
        return Err(anyhow!("subject update requires at least one of --status / --priority / --labels"));
    }
    let params = Some(json!({ "id": id, "patch": Value::Object(patch) }));
    dispatch(&kind, "update", params, project_root, json).await
}

async fn handle_subject_next(args: SubjectNextArgs, project_root: &str, json: bool) -> Result<()> {
    let kind = resolve_kind(args.kind.as_deref(), project_root)?;
    dispatch(&kind, "next", None, project_root, json).await
}

async fn handle_subject_status(args: SubjectStatusArgs, project_root: &str, json: bool) -> Result<()> {
    let kind = resolve_kind(args.kind.as_deref(), project_root)?;
    let id = args.id.trim();
    if id.is_empty() {
        return Err(anyhow!("--id must not be empty"));
    }
    let status = args.status.trim();
    if status.is_empty() {
        return Err(anyhow!("--status must not be empty"));
    }
    let params = Some(json!({ "id": id, "status": status }));
    dispatch(&kind, "status", params, project_root, json).await
}

async fn handle_subject_delete(args: SubjectDeleteArgs, project_root: &str, json: bool) -> Result<()> {
    let kind = resolve_kind(args.kind.as_deref(), project_root)?;
    let id = args.id.trim();
    if id.is_empty() {
        return Err(anyhow!("--id must not be empty"));
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
/// The resolved kind is returned as an owned `String` so callers don't
/// have to keep `args` alive across the dispatch await.
fn resolve_kind(raw: Option<&str>, project_root: &str) -> Result<String> {
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
        Some(default) => validate_kind(default).map(|s| s.to_string()),
        None => Err(anyhow!(
            "no subject kind supplied. Pass `--kind <kind>` or set `default_subject_kind` in .animus/config.json. \
             Run `animus plugin list` to see installed subject_backend kinds."
        )),
    }
}

fn validate_kind(raw: &str) -> Result<&str> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(anyhow!("--kind must not be empty"));
    }
    if trimmed.contains('/') {
        return Err(anyhow!("--kind must not contain '/'"));
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
        Err(rpc_error) => Err(anyhow!("subject call '{method}' failed ({}): {}", rpc_error.code, rpc_error.message)),
    }
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
        let resolved = resolve_kind(Some("issue"), project_root).expect("resolves");
        assert_eq!(resolved, "issue");
    }

    #[test]
    fn resolve_kind_falls_back_to_config_default() {
        let tmp = tempfile::tempdir().expect("tmp");
        let project_root = tmp.path().to_str().expect("utf-8");
        // Default `Config::load_or_default` writes `default_subject_kind: "task"`.
        let _ = Config::load_or_default(project_root).expect("seed config");
        let resolved = resolve_kind(None, project_root).expect("resolves from default");
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
        let err = resolve_kind(None, project_root.to_str().expect("utf-8"))
            .expect_err("must error when no default and no flag");
        let message = err.to_string();
        assert!(
            message.contains("--kind") || message.contains("default_subject_kind"),
            "error names the missing input: {message}"
        );
    }

    #[tokio::test]
    async fn route_or_not_found_returns_not_found_for_empty_dispatch() {
        let dispatch = SubjectPluginDispatch::empty();
        let err = route_or_not_found(&dispatch, "task/list", None).await.expect_err("expect NotFound");
        let message = err.to_string();
        assert!(message.contains("task"), "error message names kind: {message}");
        assert!(
            message.contains("subject call") || message.contains("no subject backend"),
            "error includes routing context: {message}"
        );
    }
}
