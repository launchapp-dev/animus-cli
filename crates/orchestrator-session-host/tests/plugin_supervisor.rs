use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use animus_plugin_protocol::RpcError;
use animus_session_backend::session::session_event::SessionEvent;
use animus_session_backend::session::session_request::SessionRequest;
use orchestrator_plugin_host::HostError;
use orchestrator_session_host::plugin_supervisor::{
    classify, is_structured_jsonrpc_error, PluginSupervisor, RetryDecision, SupervisorConfig, SupervisorError,
};
use orchestrator_session_host::PluginSessionBackend;
use serde_json::json;

#[cfg(unix)]
fn python_plugin_source(mode: &str, state_path: &Path) -> String {
    format!(
        r#"#!/usr/bin/env python3
import json, sys

MODE = "{mode}"
STATE = r"{state}"

def send(obj):
    sys.stdout.write(json.dumps(obj) + "\n")
    sys.stdout.flush()

def main():
    try:
        count = int(open(STATE).read().strip())
    except Exception:
        count = 0
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            msg = json.loads(line)
        except Exception:
            continue
        method = msg.get("method")
        msg_id = msg.get("id")
        if method == "initialize":
            send({{
                "jsonrpc": "2.0",
                "id": msg_id,
                "result": {{
                    "protocol_version": "1.0.0",
                    "plugin_info": {{"name": "supervisor-test", "version": "0.1.0", "plugin_kind": "provider"}},
                    "capabilities": {{"streaming": False, "progress": False, "cancellation": False, "methods": []}}
                }}
            }})
        elif method == "initialized":
            pass
        elif method == "shutdown":
            send({{"jsonrpc": "2.0", "id": msg_id, "result": None}})
            return
        elif method == "exit":
            return
        elif method == "agent/run" or method == "agent/resume":
            count += 1
            with open(STATE, "w") as f:
                f.write(str(count))
            if MODE == "die_on_first_run" and count == 1:
                sys.exit(99)
            if MODE == "always_die":
                sys.exit(99)
            if MODE == "structured_error":
                send({{
                    "jsonrpc": "2.0",
                    "id": msg_id,
                    "error": {{"code": -32602, "message": "bad params from plugin"}}
                }})
            else:
                send({{
                    "jsonrpc": "2.0",
                    "id": msg_id,
                    "result": {{"output": "ok-after-retry", "exit_code": 0}}
                }})

main()
"#,
        mode = mode,
        state = state_path.display()
    )
}

#[cfg(unix)]
fn write_python_plugin(dir: &Path, mode: &str, state_path: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let plugin = dir.join(format!("supervisor-test-plugin-{mode}"));
    std::fs::write(&plugin, python_plugin_source(mode, state_path)).expect("write plugin script");
    let mut perms = std::fs::metadata(&plugin).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&plugin, perms).unwrap();
    plugin
}

fn make_request(cwd: PathBuf) -> SessionRequest {
    SessionRequest {
        tool: "test".to_string(),
        model: "test-model".to_string(),
        prompt: "hi".to_string(),
        cwd,
        project_root: None,
        mcp_endpoint: None,
        mcp_servers: None,
        permission_mode: None,
        timeout_secs: Some(10),
        env_vars: vec![],
        extras: json!({}),
    }
}

async fn drain_events(mut run: animus_session_backend::session::session_run::SessionRun) -> Vec<SessionEvent> {
    let mut out = Vec::new();
    while let Some(event) = run.events.recv().await {
        out.push(event);
    }
    out
}

#[test]
fn supervisor_records_restart_within_window() {
    let cfg = SupervisorConfig {
        max_restarts_per_window: 3,
        window_duration: Duration::from_secs(60),
        disable_cooldown: Duration::from_secs(300),
    };
    let sup = PluginSupervisor::new("p", cfg);
    sup.record_restart().expect("1");
    sup.record_restart().expect("2");
    sup.record_restart().expect("3");
    assert!(!sup.is_disabled());
}

#[test]
fn supervisor_disables_plugin_after_3_restarts_in_60s() {
    let cfg = SupervisorConfig {
        max_restarts_per_window: 3,
        window_duration: Duration::from_secs(60),
        disable_cooldown: Duration::from_secs(300),
    };
    let sup = PluginSupervisor::new("p", cfg);
    for _ in 0..3 {
        sup.record_restart().unwrap();
    }
    let err = sup.record_restart().expect_err("4th must fail");
    assert!(matches!(err, SupervisorError::TooManyRestarts { .. }));
    assert!(sup.is_disabled());
}

#[test]
fn supervisor_re_enables_after_cooldown() {
    let cfg = SupervisorConfig {
        max_restarts_per_window: 2,
        window_duration: Duration::from_secs(60),
        disable_cooldown: Duration::from_millis(150),
    };
    let sup = PluginSupervisor::new("p", cfg);
    sup.record_restart().unwrap();
    sup.record_restart().unwrap();
    let _ = sup.record_restart().expect_err("trip");
    assert!(sup.is_disabled());
    std::thread::sleep(Duration::from_millis(200));
    assert!(!sup.is_disabled());
    sup.record_restart().expect("can record after cooldown");
}

#[test]
fn classify_errors_in_well_known_range() {
    assert!(is_structured_jsonrpc_error(-32700));
    assert!(is_structured_jsonrpc_error(-32602));
    assert!(is_structured_jsonrpc_error(-32600));
    assert!(!is_structured_jsonrpc_error(-32003));
    assert!(!is_structured_jsonrpc_error(0));
}

#[cfg(unix)]
#[tokio::test]
async fn dispatch_returns_plugin_disabled_when_supervisor_disabled() {
    use animus_session_backend::session::session_backend::SessionBackend;
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state.txt");
    let plugin = write_python_plugin(dir.path(), "happy", &state);

    let backend = PluginSessionBackend::new("supervisor-test", plugin, "test");
    backend.supervisor().force_disable_for_test_public(Duration::from_secs(60));

    let req = make_request(dir.path().to_path_buf());
    let result = backend.start_session(req).await;
    let err = result.expect_err("disabled supervisor must short-circuit dispatch");
    let message = format!("{err}");
    assert!(message.contains("disabled"), "expected disabled-supervisor error, got: {message}");
}

#[cfg(unix)]
#[tokio::test]
async fn dispatch_retries_once_on_io_error_then_succeeds() {
    use animus_session_backend::session::session_backend::SessionBackend;
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state.txt");
    let plugin = write_python_plugin(dir.path(), "die_on_first_run", &state);

    let backend = PluginSessionBackend::new("supervisor-test", plugin, "test");
    let supervisor = backend.supervisor();

    let req = make_request(dir.path().to_path_buf());
    let run = backend.start_session(req).await.expect("dispatch should return a SessionRun");
    let events = drain_events(run).await;

    let has_final_text =
        events.iter().any(|e| matches!(e, SessionEvent::FinalText { text } if text == "ok-after-retry"));
    let finished_ok = events.iter().any(|e| matches!(e, SessionEvent::Finished { exit_code: Some(0) }));
    assert!(has_final_text, "retry should surface FinalText('ok-after-retry'); events={events:?}");
    assert!(finished_ok, "retry should report Finished(exit_code=0); events={events:?}");

    let restart_count = supervisor.restart_count_for_test_public();
    assert_eq!(restart_count, 1, "supervisor should have recorded exactly one restart");
}

#[cfg(unix)]
#[tokio::test]
async fn dispatch_does_not_retry_on_structured_jsonrpc_error() {
    use animus_session_backend::session::session_backend::SessionBackend;
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state.txt");
    let plugin = write_python_plugin(dir.path(), "structured_error", &state);

    let backend = PluginSessionBackend::new("supervisor-test", plugin, "test");
    let supervisor = backend.supervisor();

    let req = make_request(dir.path().to_path_buf());
    let run = backend.start_session(req).await.expect("dispatch returns SessionRun");
    let events = drain_events(run).await;

    let saw_error = events.iter().any(|e| matches!(e, SessionEvent::Error { .. }));
    assert!(saw_error, "structured plugin error must surface as SessionEvent::Error; events={events:?}");

    let restart_count = supervisor.restart_count_for_test_public();
    assert_eq!(restart_count, 0, "structured JSON-RPC errors must NOT trigger a restart");

    let state_str = std::fs::read_to_string(&state).unwrap_or_default();
    assert_eq!(state_str.trim(), "1", "plugin should have been invoked exactly once (no retry on structured error)");
}

#[cfg(unix)]
#[tokio::test]
async fn dispatch_propagates_when_retry_also_dies() {
    use animus_session_backend::session::session_backend::SessionBackend;
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state.txt");
    let plugin = write_python_plugin(dir.path(), "always_die", &state);

    let backend =
        PluginSessionBackend::new("supervisor-test", plugin, "test").with_supervisor(Arc::new(PluginSupervisor::new(
            "supervisor-test",
            SupervisorConfig {
                max_restarts_per_window: 5,
                window_duration: Duration::from_secs(60),
                disable_cooldown: Duration::from_secs(300),
            },
        )));

    let req = make_request(dir.path().to_path_buf());
    let run = backend.start_session(req).await.expect("dispatch returns SessionRun");
    let events = drain_events(run).await;

    let finished_failure =
        events.iter().any(|e| matches!(e, SessionEvent::Finished { exit_code } if exit_code.unwrap_or(0) != 0));
    assert!(finished_failure, "two consecutive deaths must propagate failure; events={events:?}");
}

#[test]
fn typed_classifier_treats_connection_lost_as_death_like() {
    assert_eq!(classify(&HostError::ConnectionLost), RetryDecision::DeathLike);
}

#[test]
fn typed_classifier_treats_timeout_as_death_like() {
    assert_eq!(classify(&HostError::Timeout(Duration::from_millis(50))), RetryDecision::DeathLike);
}

#[test]
fn typed_classifier_treats_process_exited_as_death_like() {
    assert_eq!(classify(&HostError::ProcessExited("status=137".into())), RetryDecision::DeathLike);
}

#[test]
fn typed_classifier_treats_rpc_well_known_codes_as_structured() {
    let err = HostError::Rpc(RpcError { code: -32602, message: "bad params from plugin".into(), data: None });
    assert_eq!(classify(&err), RetryDecision::StructuredError);
}

#[test]
fn typed_classifier_treats_rpc_internal_error_inside_well_known_range_as_structured() {
    // INTERNAL_ERROR (-32603) is in the well-known JSON-RPC range. Once a
    // HostError::Rpc(_) has been constructed, that means the plugin actually
    // returned an error frame — process-death cases would have surfaced as
    // HostError::ConnectionLost instead. So the typed classifier treats this
    // as structured, eliminating the legacy "search the message for
    // 'connection lost' / 'broken pipe'" heuristic.
    let err = HostError::Rpc(RpcError { code: -32603, message: "plugin handler raised: KeyError".into(), data: None });
    assert_eq!(classify(&err), RetryDecision::StructuredError);
}

#[test]
fn typed_classifier_treats_out_of_range_rpc_code_as_death_like() {
    let err = HostError::Rpc(RpcError { code: -1, message: "custom plugin failure".into(), data: None });
    assert_eq!(classify(&err), RetryDecision::DeathLike);
}
