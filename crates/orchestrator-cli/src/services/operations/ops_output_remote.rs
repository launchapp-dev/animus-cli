//! log_storage-backed read fallback for `animus output` when the local
//! `runs/<run_id>/` directory is absent — the node-executed run case, where
//! the transcript only exists in the configured `log_storage_backend`
//! plugin (e.g. `animus-log-storage-s3`).
//!
//! ## Storage contract (writer side)
//!
//! Writers (the workflow runner / daemon offload path) store one
//! [`LogEntry`] per run-dir JSONL row with:
//!
//! - `source` = `workflow`, `source_name` = the workflow id (the protocol's
//!   own convention for [`LogSource::Workflow`]; also prefix-narrows the
//!   backend scan);
//! - `target` = `workflow.run.<event-kind>`;
//! - `fields.run_id` / `fields.workflow_id` — the owning run / workflow;
//! - `fields.source_file` — the run-dir file the row came from
//!   (`events.jsonl`, `json-output.jsonl`, ...);
//! - `fields.run_event` — the original JSONL row object, verbatim.
//!
//! Readers below query by `source_name`, filter on `fields.run_id`
//! in-process, and rebuild rows from `fields.run_event`. Entries that do
//! not carry `fields.run_event` are not transcript rows and are skipped.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use animus_log_storage_protocol::{LogEntry, LogQueryResult, LogSource};
use anyhow::Result;
use orchestrator_daemon_runtime::{spawn_log_storage_supervisor, LogStorageHandle};
use serde_json::Value;

use super::ops_output::{extract_timestamp_hint, RunJsonlEntryCli};

/// Upper bound on entries pulled in one remote transcript read. `query`
/// keeps the NEWEST `limit` entries, so a transcript larger than this loses
/// its oldest events first — sized far above a normal run so that only
/// pathological transcripts truncate.
const REMOTE_RUN_QUERY_LIMIT: usize = 10_000;

/// One remote read fans out into per-object fetches in the backend; allow
/// for a cold plugin spawn + a large transcript without hanging the CLI.
const REMOTE_RUN_QUERY_TIMEOUT: Duration = Duration::from_secs(30);

/// Derive the workflow id from a `wf-<workflow_uuid>[-<phase>-...]` run id
/// (the runner's per-phase run-dir naming). Returns `None` for run ids that
/// do not follow that scheme.
pub(crate) fn workflow_id_from_run_id(run_id: &str) -> Option<String> {
    let rest = run_id.strip_prefix("wf-")?;
    let candidate = rest.get(..36)?;
    if !rest[36..].is_empty() && !rest[36..].starts_with('-') {
        return None;
    }
    uuid::Uuid::parse_str(candidate).ok()?;
    Some(candidate.to_string())
}

/// Rebuild one jsonl row from a stored [`LogEntry`] per the contract above.
/// Returns `None` for entries that are not transcript rows.
fn log_entry_to_jsonl_row(entry: &LogEntry) -> Option<RunJsonlEntryCli> {
    let run_event = entry.fields.get("run_event")?;
    let source_file = entry.fields.get("source_file").and_then(Value::as_str).unwrap_or("events.jsonl").to_string();
    let line = serde_json::to_string(run_event).ok()?;
    let timestamp_hint = extract_timestamp_hint(&line).or_else(|| Some(entry.ts.to_rfc3339()));
    Some(RunJsonlEntryCli { source_file, line, timestamp_hint })
}

/// Whether `entry` belongs to `run_id` per the contract. Entries stored
/// under a run-id `source_name` without a `fields.run_id` (older writers)
/// are kept; a mismatched `fields.run_id` always drops.
fn entry_belongs_to_run(entry: &LogEntry, run_id: &str) -> bool {
    match entry.fields.get("run_id").and_then(Value::as_str) {
        Some(value) => value == run_id,
        None => entry.source_name.as_deref() == Some(run_id),
    }
}

/// Spawn the project's `log_storage_backend` plugin, if one is installed.
/// Returns `None` when the project has no plugin-routed backend (the
/// in-tree fallback holds no remote run data).
async fn spawn_plugin_handle(project_root: &str) -> Option<Arc<LogStorageHandle>> {
    let outcome = spawn_log_storage_supervisor(Path::new(project_root)).await;
    let handle = outcome.handle;
    if handle.is_plugin() {
        Some(handle)
    } else {
        handle.shutdown().await;
        None
    }
}

/// Query a plugin `handle` for transcript rows stored under `source_name`,
/// optionally restricted to one run.
async fn query_remote_rows(
    handle: &LogStorageHandle,
    source_name: &str,
    run_id_filter: Option<&str>,
) -> Result<Vec<RunJsonlEntryCli>> {
    let params = serde_json::json!({
        "source": LogSource::Workflow,
        "source_name": source_name,
        "limit": REMOTE_RUN_QUERY_LIMIT,
    });
    let value = handle.tail(Some(params), REMOTE_RUN_QUERY_TIMEOUT).await?.unwrap_or(Value::Null);
    let result: LogQueryResult = serde_json::from_value(value)?;
    let mut rows: Vec<RunJsonlEntryCli> = result
        .entries
        .iter()
        .filter(|entry| match run_id_filter {
            Some(run_id) => entry_belongs_to_run(entry, run_id),
            None => true,
        })
        .filter_map(log_entry_to_jsonl_row)
        .collect();
    rows.sort_by(|a, b| a.timestamp_hint.cmp(&b.timestamp_hint));
    Ok(rows)
}

/// Read a run's transcript rows from the log_storage backend. Tries the
/// workflow-id `source_name` first (derived from `wf-<uuid>-...` run ids,
/// filtered to the run), then a run-id `source_name` for writers that key
/// by run id directly. Empty when no plugin backend is installed or the
/// backend holds nothing for the run.
pub(crate) async fn remote_run_jsonl_entries(project_root: &str, run_id: &str) -> Result<Vec<RunJsonlEntryCli>> {
    let Some(handle) = spawn_plugin_handle(project_root).await else {
        return Ok(Vec::new());
    };
    let mut source_names: Vec<String> = Vec::new();
    if let Some(workflow_id) = workflow_id_from_run_id(run_id) {
        source_names.push(workflow_id);
    }
    if !source_names.iter().any(|name| name == run_id) {
        source_names.push(run_id.to_string());
    }
    let mut rows = Vec::new();
    for source_name in &source_names {
        match query_remote_rows(&handle, source_name, Some(run_id)).await {
            Ok(found) if !found.is_empty() => {
                rows = found;
                break;
            }
            Ok(_) => {}
            Err(err) => {
                handle.shutdown().await;
                return Err(err);
            }
        }
    }
    handle.shutdown().await;
    Ok(rows)
}

/// Read every transcript event payload stored for a workflow (all runs,
/// chronological) from the log_storage backend — the `output read
/// --workflow-id` fallback when no local run was ever recorded.
pub(crate) async fn remote_workflow_events(project_root: &str, workflow_id: &str) -> Result<Vec<Value>> {
    let Some(handle) = spawn_plugin_handle(project_root).await else {
        return Ok(Vec::new());
    };
    let result = query_remote_rows(&handle, workflow_id, None).await;
    handle.shutdown().await;
    Ok(result?
        .into_iter()
        .filter(|row| row.source_file == "events.jsonl")
        .filter_map(|row| serde_json::from_str::<Value>(&row.line).ok())
        .collect())
}

/// Read one run's `events.jsonl` payloads from the log_storage backend.
pub(crate) async fn remote_run_events(project_root: &str, run_id: &str) -> Result<Vec<Value>> {
    Ok(remote_run_jsonl_entries(project_root, run_id)
        .await?
        .into_iter()
        .filter(|row| row.source_file == "events.jsonl")
        .filter_map(|row| serde_json::from_str::<Value>(&row.line).ok())
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use animus_log_storage_protocol::LogLevel;
    use chrono::{DateTime, Utc};
    use orchestrator_plugin_host::PluginHost;
    use std::sync::Arc;
    use tokio::sync::Mutex;

    fn ts(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn transcript_entry(
        id: &str,
        source_name: &str,
        run_id: &str,
        source_file: &str,
        ts: DateTime<Utc>,
        kind: &str,
    ) -> LogEntry {
        LogEntry {
            id: id.into(),
            ts,
            level: LogLevel::Info,
            source: LogSource::Workflow,
            source_name: Some(source_name.into()),
            target: format!("workflow.run.{kind}"),
            message: kind.into(),
            fields: serde_json::json!({
                "run_id": run_id,
                "workflow_id": "wf-owner",
                "source_file": source_file,
                "run_event": {
                    "kind": kind,
                    "run_id": run_id,
                    "timestamp": ts.to_rfc3339(),
                    "text": format!("payload-{id}"),
                },
            }),
        }
    }

    #[test]
    fn workflow_id_derivation_from_wf_run_ids() {
        assert_eq!(
            workflow_id_from_run_id("wf-31184ea3-55fd-4ea3-ae65-284a1fda3042-pr-rework-0-c0-a1-5dc8555b"),
            Some("31184ea3-55fd-4ea3-ae65-284a1fda3042".to_string())
        );
        // Legacy bare `wf-<uuid>` run dir name.
        assert_eq!(
            workflow_id_from_run_id("wf-31184ea3-55fd-4ea3-ae65-284a1fda3042"),
            Some("31184ea3-55fd-4ea3-ae65-284a1fda3042".to_string())
        );
        assert_eq!(workflow_id_from_run_id("run-output"), None);
        assert_eq!(workflow_id_from_run_id("wf-not-a-uuid"), None);
        assert_eq!(workflow_id_from_run_id("wf-31184ea3-55fd-4ea3-ae65-284a1fda304"), None);
        // Trailing segment must be `-`-separated, not a UUID continuation.
        assert_eq!(workflow_id_from_run_id("wf-31184ea3-55fd-4ea3-ae65-284a1fda3042extra"), None);
    }

    #[test]
    fn maps_contract_entries_to_jsonl_rows_and_skips_others() {
        let entry = transcript_entry(
            "e1",
            "wf-owner",
            "wf-owner-build-0",
            "stdout.jsonl",
            ts("2026-09-01T10:00:00Z"),
            "output_chunk",
        );
        let row = log_entry_to_jsonl_row(&entry).expect("transcript row");
        assert_eq!(row.source_file, "stdout.jsonl");
        let payload: Value = serde_json::from_str(&row.line).unwrap();
        assert_eq!(payload.pointer("/text").and_then(Value::as_str), Some("payload-e1"));
        assert_eq!(row.timestamp_hint.as_deref(), Some("2026-09-01T10:00:00+00:00"));

        // A daemon-style entry without `fields.run_event` is not a transcript row.
        let mut other = entry.clone();
        other.fields = serde_json::json!({ "seq": 1 });
        assert!(log_entry_to_jsonl_row(&other).is_none());
    }

    #[test]
    fn run_membership_filter_rules() {
        let entry = transcript_entry("e1", "wf-owner", "run-a", "events.jsonl", ts("2026-09-01T10:00:00Z"), "started");
        assert!(entry_belongs_to_run(&entry, "run-a"));
        assert!(!entry_belongs_to_run(&entry, "run-b"));
        // Legacy entries keyed by run-id source_name without fields.run_id.
        let mut legacy = entry.clone();
        legacy.source_name = Some("run-a".into());
        legacy.fields = serde_json::json!({ "run_event": { "kind": "started" } });
        assert!(entry_belongs_to_run(&legacy, "run-a"));
        assert!(!entry_belongs_to_run(&legacy, "run-b"));
    }

    // -----------------------------------------------------------------
    // In-process fake `log_storage_backend` over duplex streams, mirroring
    // the daemon-runtime `fake_log_storage_host` test pattern.
    // -----------------------------------------------------------------

    type RecordedCall = (String, Option<Value>);

    async fn fake_log_storage_host(query_response: Value, recorded_calls: Arc<Mutex<Vec<RecordedCall>>>) -> PluginHost {
        use animus_plugin_protocol::{InitializeResult, PluginCapabilities, PluginInfo, RpcRequest, RpcResponse};
        use tokio::io::{duplex, AsyncBufReadExt, AsyncWriteExt, BufReader};

        let (host_reader, mut plugin_writer) = duplex(8192);
        let (plugin_reader, host_writer) = duplex(8192);
        let recorded = recorded_calls.clone();

        tokio::spawn(async move {
            let mut reader = BufReader::new(plugin_reader);
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).await.expect("read line") == 0 {
                    break;
                }
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                let value: Value = match serde_json::from_str(trimmed) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                if value.get("id").is_none() || value.get("id") == Some(&Value::Null) {
                    continue; // notifications
                }
                let request: RpcRequest = match serde_json::from_value(value) {
                    Ok(r) => r,
                    Err(_) => continue,
                };
                let response = match request.method.as_str() {
                    "initialize" => RpcResponse::ok(
                        request.id,
                        serde_json::json!(InitializeResult {
                            protocol_version: "1.0.0".to_string(),
                            plugin_info: PluginInfo {
                                name: "fake-log-storage".to_string(),
                                version: "0.1.0".to_string(),
                                plugin_kind: "log_storage_backend".to_string(),
                                plugin_kinds: vec![],
                                description: None,
                            },
                            capabilities: PluginCapabilities::default(),
                            kind_capabilities: std::collections::HashMap::new(),
                        }),
                    ),
                    method => {
                        recorded.lock().await.push((method.to_string(), request.params.clone()));
                        if method == "log_storage/query" {
                            RpcResponse::ok(request.id, query_response.clone())
                        } else {
                            RpcResponse::ok(request.id, serde_json::json!({}))
                        }
                    }
                };
                let mut encoded = serde_json::to_string(&response).expect("encode response");
                encoded.push('\n');
                if plugin_writer.write_all(encoded.as_bytes()).await.is_err() {
                    break;
                }
            }
        });

        PluginHost::from_streams("fake-log-storage", host_reader, host_writer)
    }

    async fn plugin_handle(query_response: Value) -> (LogStorageHandle, Arc<Mutex<Vec<RecordedCall>>>) {
        let recorded: Arc<Mutex<Vec<RecordedCall>>> = Arc::new(Mutex::new(Vec::new()));
        let host = fake_log_storage_host(query_response, recorded.clone()).await;
        host.handshake().await.expect("handshake");
        (
            LogStorageHandle::from_handshaked_host("fake-log-storage", host, std::path::PathBuf::from("/tmp/project")),
            recorded,
        )
    }

    #[tokio::test]
    async fn query_remote_rows_filters_to_run_and_sorts_chronologically() {
        let wf_id = "31184ea3-55fd-4ea3-ae65-284a1fda3042";
        let run_a = format!("wf-{wf_id}-build-0-c0-a1-aaaa");
        let run_b = format!("wf-{wf_id}-verify-1-c0-a1-bbbb");
        let entries = vec![
            // Deliberately unordered on the wire: query contract is oldest-first
            // but the row-level sort must hold regardless.
            transcript_entry("e2", wf_id, &run_a, "events.jsonl", ts("2026-09-01T10:00:05Z"), "finished"),
            transcript_entry("e1", wf_id, &run_a, "events.jsonl", ts("2026-09-01T10:00:00Z"), "started"),
            transcript_entry("e9", wf_id, &run_b, "events.jsonl", ts("2026-09-01T10:00:02Z"), "started"),
        ];
        let response = serde_json::to_value(LogQueryResult { entries, next_cursor: None }).unwrap();
        let (handle, recorded) = plugin_handle(response).await;

        let rows = query_remote_rows(&handle, wf_id, Some(&run_a)).await.expect("query");
        assert_eq!(rows.len(), 2, "run_b entries must be filtered out");
        let kinds: Vec<String> = rows
            .iter()
            .map(|row| {
                let payload: Value = serde_json::from_str(&row.line).unwrap();
                payload.pointer("/kind").and_then(Value::as_str).unwrap().to_string()
            })
            .collect();
        assert_eq!(kinds, ["started", "finished"]);

        // The wire request narrows by source + source_name with a high limit.
        let calls = recorded.lock().await;
        let (method, params) = calls.iter().find(|(m, _)| m == "log_storage/query").expect("query call");
        assert_eq!(method, "log_storage/query");
        let params = params.clone().expect("query params");
        assert_eq!(params.pointer("/source").and_then(Value::as_str), Some("workflow"));
        assert_eq!(params.pointer("/source_name").and_then(Value::as_str), Some(wf_id));
        assert!(params.pointer("/limit").and_then(Value::as_u64).unwrap() >= 10_000);

        handle.shutdown().await;
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)] // intentional: guards process-global env mutation across the await
    async fn remote_run_jsonl_entries_without_plugin_returns_empty() {
        let _lock = crate::shared::test_env_lock().lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let temp = tempfile::tempdir().expect("temp home");
        let _home = protocol::test_utils::EnvVarGuard::set("HOME", Some(temp.path().to_string_lossy().as_ref()));
        let _config = protocol::test_utils::EnvVarGuard::set(
            "ANIMUS_CONFIG_DIR",
            Some(temp.path().join("config").to_string_lossy().as_ref()),
        );
        let project_root = temp.path().join("project");
        std::fs::create_dir_all(&project_root).expect("project root");

        let rows = remote_run_jsonl_entries(
            project_root.to_string_lossy().as_ref(),
            "wf-31184ea3-55fd-4ea3-ae65-284a1fda3042-build-0",
        )
        .await
        .expect("no-plugin fallback must not error");
        assert!(rows.is_empty());
    }
}
