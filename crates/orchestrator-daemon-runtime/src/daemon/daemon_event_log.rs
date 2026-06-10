use std::path::{Path, PathBuf};
use std::{fs::OpenOptions, io::Write};

use anyhow::Result;
use chrono::Utc;
use fs2::FileExt;
use protocol::DaemonEventRecord;
use serde_json::Value;
use uuid::Uuid;

use crate::DaemonEventsPollResponse;

pub struct DaemonEventLog;

impl DaemonEventLog {
    pub fn log_path() -> PathBuf {
        protocol::daemon_events_log_path()
    }

    pub fn read_records(limit: Option<usize>, project_root_filter: Option<&str>) -> Result<Vec<DaemonEventRecord>> {
        let path = Self::log_path();
        let canonical_project_root_filter = normalize_project_root_filter(project_root_filter);
        let events = read_all_nonempty_lines(&path)?
            .into_iter()
            .filter_map(|line| serde_json::from_str::<DaemonEventRecord>(&line).ok())
            .filter(|record| matches_project_root_filter(record, canonical_project_root_filter.as_deref()))
            .collect();
        Ok(apply_event_limit(events, limit))
    }

    pub fn poll(limit: Option<usize>, project_root_filter: Option<&str>) -> Result<DaemonEventsPollResponse> {
        let path = Self::log_path();
        let events = Self::read_records(limit, project_root_filter)?;
        Ok(DaemonEventsPollResponse {
            schema: "animus.daemon.events.poll.v1".to_string(),
            events_path: path.to_string_lossy().to_string(),
            count: events.len(),
            events,
        })
    }

    pub fn next_event(seq: &mut u64, event_type: &str, project_root: Option<String>, data: Value) -> DaemonEventRecord {
        *seq = seq.saturating_add(1);
        DaemonEventRecord {
            schema: "animus.daemon.event.v1".to_string(),
            id: Uuid::new_v4().to_string(),
            seq: *seq,
            timestamp: Utc::now().to_rfc3339(),
            event_type: event_type.to_string(),
            project_root,
            data,
        }
    }

    pub fn append(record: &DaemonEventRecord) -> Result<()> {
        // Policy (B) tee: when a `log_storage_backend` plugin is
        // installed, forward each record to the plugin as a
        // `log_storage/store` request AND continue writing the in-tree
        // `daemon-events.jsonl` file. The file is still the source of
        // truth for `animus daemon events` / MCP `daemon.events` poll
        // readers, which do not (yet) speak the plugin's `query`/`tail`
        // surface. The plugin call is fire-and-forget so a slow or
        // unhealthy sink can never block the daemon's hot path.
        if let Some(handle) = crate::log_storage::current_log_storage_handle() {
            if handle.is_plugin() {
                dispatch_log_entry_to_plugin(handle, record);
            }
        }
        let path = Self::log_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        append_line(&path, &serde_json::to_string(record)?)
    }
}

/// Fire-and-forget dispatch of a daemon event record to the active
/// `log_storage_backend` plugin as a `log_storage/store` request.
///
/// Runs on the current Tokio runtime when one is available (the daemon
/// always has one) and degrades to a synchronous best-effort path with a
/// fresh single-threaded runtime when called outside an async context
/// (CLI one-shot test paths, tests that call `append` directly).
/// Plugin failures are warn-logged via `tracing` — policy (B) tees to
/// the local file so the entry is never lost even when the sink is
/// unhealthy.
///
/// The record is projected onto the
/// [`animus_log_storage_protocol::LogEntry`] shape before being sent:
/// id + timestamp + `Daemon` source + `daemon.events.<event_type>`
/// target + the record's data as `fields`. The original record's
/// seq/project metadata is preserved inside `fields` so plugin authors
/// can reach it.
fn dispatch_log_entry_to_plugin(handle: std::sync::Arc<crate::LogStorageHandle>, record: &DaemonEventRecord) {
    let entry = match daemon_event_record_to_log_entry(record) {
        Ok(value) => value,
        Err(error) => {
            tracing::warn!(
                target: "daemon_event_log",
                error = %error,
                "failed to project DaemonEventRecord onto log_storage LogEntry shape",
            );
            return;
        }
    };
    let plugin_name = handle.plugin_name().unwrap_or("<unknown>").to_string();
    let publish_fut = async move {
        if let Err(error) = handle.publish_log_entry(entry).await {
            tracing::warn!(
                target: "daemon_event_log",
                plugin = %plugin_name,
                error = %format!("{error:#}"),
                "log_storage plugin store failed; entry still written to events.jsonl (policy: tee)",
            );
        }
    };
    match tokio::runtime::Handle::try_current() {
        Ok(rt) => {
            rt.spawn(publish_fut);
        }
        Err(_) => {
            // No ambient runtime (sync tests, CLI one-shots). Block on a
            // fresh single-threaded runtime so the test path still
            // observes the request. Drop any panic — this is best-effort.
            if let Ok(rt) = tokio::runtime::Builder::new_current_thread().enable_all().build() {
                rt.block_on(publish_fut);
            }
        }
    }
}

/// Project a [`DaemonEventRecord`] onto an
/// [`animus_log_storage_protocol::LogEntry`] JSON value. The plugin sees
/// a record that conforms to the log_storage wire shape; the original
/// daemon-event seq + payload are preserved inside `fields` for plugins
/// that want to disambiguate by daemon-events convention.
fn daemon_event_record_to_log_entry(record: &DaemonEventRecord) -> Result<Value> {
    let fields = serde_json::json!({
        "seq": record.seq,
        "data": record.data,
        "schema": record.schema,
        "project_root": record.project_root,
    });
    let entry = serde_json::json!({
        "id": record.id,
        "ts": record.timestamp,
        "level": "info",
        "source": "daemon",
        "target": format!("daemon.events.{}", record.event_type),
        "message": record.event_type,
        "fields": fields,
    });
    Ok(entry)
}

const MAX_LOG_SIZE_BYTES: u64 = 10 * 1024 * 1024; // 10 MB
const APPEND_ROTATION_RETRIES: usize = 5;

fn append_line(path: &Path, line: &str) -> Result<()> {
    for _ in 0..APPEND_ROTATION_RETRIES {
        let mut file = OpenOptions::new().create(true).append(true).open(path)?;
        file.lock_exclusive()?;
        if !locked_file_still_at_path(&file, path) {
            let _ = file.unlock();
            continue;
        }
        if file.metadata()?.len() >= MAX_LOG_SIZE_BYTES {
            let rotated = path.with_extension("jsonl.1");
            if std::fs::rename(path, rotated).is_ok() {
                let _ = file.unlock();
                continue;
            }
        }
        file.write_all(line.as_bytes())?;
        file.write_all(b"\n")?;
        file.unlock()?;
        return Ok(());
    }

    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    file.lock_exclusive()?;
    file.write_all(line.as_bytes())?;
    file.write_all(b"\n")?;
    file.unlock()?;
    Ok(())
}

#[cfg(unix)]
fn locked_file_still_at_path(file: &std::fs::File, path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (file.metadata(), std::fs::metadata(path)) {
        (Ok(held), Ok(current)) => held.ino() == current.ino() && held.dev() == current.dev(),
        _ => false,
    }
}

#[cfg(not(unix))]
fn locked_file_still_at_path(_file: &std::fs::File, _path: &Path) -> bool {
    true
}

fn read_all_nonempty_lines(path: &Path) -> Result<Vec<String>> {
    if !path.exists() {
        return Ok(Vec::new());
    }

    let content = std::fs::read_to_string(path)?;
    Ok(content.lines().map(str::trim).filter(|line| !line.is_empty()).map(ToOwned::to_owned).collect())
}

fn normalize_project_root_filter(filter: Option<&str>) -> Option<String> {
    filter.map(str::trim).filter(|value| !value.is_empty()).map(canonicalize_lossy)
}

fn matches_project_root_filter(record: &DaemonEventRecord, canonical_filter: Option<&str>) -> bool {
    let Some(filter) = canonical_filter else {
        return true;
    };
    let Some(record_project_root) = record.project_root.as_deref() else {
        return false;
    };
    canonicalize_lossy(record_project_root) == filter
}

fn apply_event_limit(mut events: Vec<DaemonEventRecord>, limit: Option<usize>) -> Vec<DaemonEventRecord> {
    if let Some(limit) = limit {
        if limit == 0 {
            return Vec::new();
        }
        if events.len() > limit {
            events = events.split_off(events.len() - limit);
        }
    }
    events
}

fn canonicalize_lossy(path: &str) -> String {
    let candidate = PathBuf::from(path);
    candidate.canonicalize().unwrap_or(candidate).to_string_lossy().to_string()
}

#[cfg(test)]
mod daemon_logs_dispatch_tests {
    //! Audit P2: prove that `DaemonEventLog::append` actually routes
    //! through the active `log_storage_backend` plugin (policy A: plugin
    //! takes over entirely, in-tree events.jsonl is NOT written when a
    //! plugin handle is installed).
    use super::*;
    use crate::log_storage::{
        clear_log_storage_handle, current_log_storage_handle, install_log_storage_handle, LogStorageHandle,
        LOG_STORAGE_TEST_SLOT_LOCK,
    };
    use animus_plugin_protocol::{InitializeResult, PluginCapabilities, PluginInfo, RpcRequest, RpcResponse};
    use orchestrator_plugin_host::PluginHost;
    use std::sync::Arc;
    use tokio::io::{duplex, AsyncBufReadExt, AsyncWriteExt, BufReader};

    struct SlotGuard;
    impl Drop for SlotGuard {
        fn drop(&mut self) {
            clear_log_storage_handle();
        }
    }

    /// RAII guard for the `ANIMUS_CONFIG_DIR` env var. Wraps the protocol
    /// crate's `EnvVarGuard` so the mutation holds the process-wide env
    /// lock shared with every other env-mutating test in this binary, and
    /// restores the previous value when the guard drops.
    struct ConfigDirGuard {
        _inner: protocol::test_utils::EnvVarGuard,
    }
    impl ConfigDirGuard {
        fn set(value: &std::path::Path) -> Self {
            Self { _inner: protocol::test_utils::EnvVarGuard::set_os("ANIMUS_CONFIG_DIR", Some(value.as_os_str())) }
        }
    }

    /// Records (method, params) for every JSON-RPC request the fake host
    /// receives. Used by tests to assert the daemon hit `log_storage/store`
    /// with the projected `LogEntry` shape.
    type RecordedCall = (String, Option<serde_json::Value>);

    async fn fake_recording_host(name: &str, recorded: Arc<tokio::sync::Mutex<Vec<RecordedCall>>>) -> PluginHost {
        let (host_reader, mut plugin_writer) = duplex(8192);
        let (plugin_reader, host_writer) = duplex(8192);
        let name_for_task = name.to_string();
        let recorded_clone = recorded.clone();
        tokio::spawn(async move {
            let mut reader = BufReader::new(plugin_reader);
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
                    break;
                }
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                let value: serde_json::Value = match serde_json::from_str(trimmed) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                if value.get("id").is_none() || value.get("id") == Some(&serde_json::Value::Null) {
                    // Skip notifications — we record requests only.
                    continue;
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
                                name: name_for_task.clone(),
                                version: "0.1.0".to_string(),
                                plugin_kind: animus_plugin_protocol::PLUGIN_KIND_LOG_STORAGE_BACKEND.to_string(),
                                description: None,
                            },
                            capabilities: PluginCapabilities::default(),
                        }),
                    ),
                    method => {
                        recorded_clone.lock().await.push((method.to_string(), request.params.clone()));
                        RpcResponse::ok(request.id, serde_json::json!({}))
                    }
                };
                let mut encoded = serde_json::to_string(&response).unwrap();
                encoded.push('\n');
                if plugin_writer.write_all(encoded.as_bytes()).await.is_err() {
                    break;
                }
            }
        });
        PluginHost::from_streams(name, host_reader, host_writer)
    }

    #[tokio::test]
    async fn append_tees_to_plugin_and_events_jsonl() {
        let _slot = LOG_STORAGE_TEST_SLOT_LOCK.lock().await;
        let _guard = SlotGuard;
        let temp = tempfile::tempdir().expect("tempdir");
        let _config_dir = ConfigDirGuard::set(temp.path());

        // Spin up a fake log_storage_backend plugin and install its
        // handle into the process-global slot, simulating the daemon's
        // startup wiring.
        let recorded: Arc<tokio::sync::Mutex<Vec<RecordedCall>>> = Arc::new(tokio::sync::Mutex::new(Vec::new()));
        let host = fake_recording_host("test-log-sink", recorded.clone()).await;
        host.handshake().await.expect("handshake");
        let handle = Arc::new(LogStorageHandle::from_handshaked_host("test-log-sink", host, temp.path().to_path_buf()));
        install_log_storage_handle(handle.clone());
        assert!(current_log_storage_handle().is_some(), "handle installed");

        // Build a record and call append.
        let mut seq = 0u64;
        let record = DaemonEventLog::next_event(
            &mut seq,
            "test-event",
            Some("/tmp/fake-project".to_string()),
            serde_json::json!({"hello": "world"}),
        );
        DaemonEventLog::append(&record).expect("append ok");

        // Wait for the spawned dispatch task to flush. Poll instead of a
        // fixed sleep: under workspace-parallel test load the task may not
        // get scheduled for hundreds of milliseconds.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let arrived = recorded
                .lock()
                .await
                .iter()
                .any(|(method, _)| method == animus_log_storage_protocol::METHOD_LOG_STORAGE_STORE);
            if arrived {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "log_storage/store request did not arrive within 5s");
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }

        // Plugin received a `log_storage/store` request with the
        // projected LogEntry shape.
        let captured = recorded.lock().await;
        let received = captured
            .iter()
            .find(|(method, _)| method == animus_log_storage_protocol::METHOD_LOG_STORAGE_STORE)
            .expect("log_storage/store request arrived");
        let params = received.1.clone().expect("request carries params");
        let entries = params.get("entries").and_then(|v| v.as_array()).expect("entries array");
        assert_eq!(entries.len(), 1, "store batch contains exactly one entry");
        let entry = &entries[0];
        assert_eq!(entry.get("message").and_then(|v| v.as_str()), Some("test-event"));
        assert_eq!(entry.get("source").and_then(|v| v.as_str()), Some("daemon"));
        let target = entry.get("target").and_then(|v| v.as_str()).unwrap_or("");
        assert!(target.starts_with("daemon.events."), "target uses daemon.events.* convention, got {target:?}");
        drop(captured);

        // events.jsonl WAS written (policy B: tee). Existing
        // `daemon/events` and MCP readers still see history.
        let log_path = DaemonEventLog::log_path();
        assert!(log_path.exists(), "events.jsonl is written under policy B (tee)");
        let contents = std::fs::read_to_string(&log_path).expect("read events.jsonl");
        assert!(contents.contains("test-event"), "events.jsonl carries the record body verbatim");

        handle.shutdown().await;
    }

    #[tokio::test]
    async fn append_falls_back_to_events_jsonl_when_no_handle_installed() {
        let _slot = LOG_STORAGE_TEST_SLOT_LOCK.lock().await;
        let _guard = SlotGuard;
        let temp = tempfile::tempdir().expect("tempdir");
        let _config_dir = ConfigDirGuard::set(temp.path());

        // No plugin handle installed → in-tree behavior.
        clear_log_storage_handle();
        assert!(current_log_storage_handle().is_none());

        let mut seq = 0u64;
        let record = DaemonEventLog::next_event(
            &mut seq,
            "in-tree-event",
            Some("/tmp/fake-project".to_string()),
            serde_json::json!({"k": "v"}),
        );
        DaemonEventLog::append(&record).expect("append ok");

        let log_path = DaemonEventLog::log_path();
        assert!(log_path.exists(), "in-tree fallback writes events.jsonl, expected at {}", log_path.display());
        let contents = std::fs::read_to_string(&log_path).expect("read events.jsonl");
        assert!(contents.contains("in-tree-event"), "events.jsonl carries the record body");
    }

    #[tokio::test]
    async fn append_falls_back_to_events_jsonl_when_in_tree_handle_installed() {
        let _slot = LOG_STORAGE_TEST_SLOT_LOCK.lock().await;
        let _guard = SlotGuard;
        let temp = tempfile::tempdir().expect("tempdir");
        let _config_dir = ConfigDirGuard::set(temp.path());

        // Install an explicit in-tree handle. `append` should ignore the
        // plugin branch and still write events.jsonl.
        let handle = Arc::new(LogStorageHandle::in_tree(temp.path().to_path_buf()));
        install_log_storage_handle(handle.clone());

        let mut seq = 0u64;
        let record = DaemonEventLog::next_event(
            &mut seq,
            "in-tree-handle-event",
            Some("/tmp/fake-project".to_string()),
            serde_json::json!({"k": "v"}),
        );
        DaemonEventLog::append(&record).expect("append ok");

        let log_path = DaemonEventLog::log_path();
        assert!(log_path.exists(), "in-tree handle still writes events.jsonl");
        let contents = std::fs::read_to_string(&log_path).expect("read events.jsonl");
        assert!(contents.contains("in-tree-handle-event"), "events.jsonl carries the record body");
    }
}
