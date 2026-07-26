// Tests serialize on `ENV_LOCK` to coordinate process-wide `ANIMUS_PLUGIN_PATH`
// mutations across parallel async tests. The guard is intentionally held
// across `.await` because the contended resource is the env, not the lock.
#![allow(clippy::await_holding_lock)]

//! Real `agent/run` integration test against the deterministic `animus-provider-mock`.
//!
//! Wires the SessionBackendResolver through plugin discovery (mirroring how
//! agent-runner does it in production) and asserts that:
//!
//! - The mock provider is selected when the request's tool matches its
//!   `provider_tool` (`mock`).
//! - Streaming notifications come through as live SessionEvents:
//!   Started → Thinking → ToolCall → ToolResult → TextDelta×3 → FinalText →
//!   Metadata → Finished — all visible *before* the request future resolves.

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use animus_session_backend::session::{SessionEvent, SessionRequest};
use orchestrator_plugin_host::session::SessionBackendResolver;
use serde_json::json;
use tokio::time::timeout;

/// Serializes tests in this integration binary because they share process-wide
/// env vars (`ANIMUS_PLUGIN_PATH`, `ANIMUS_PLUGIN_DIR`). Without this, cargo's
/// default parallel runner interleaves writes and reads on those keys and
/// produces flaky test results.
static ENV_LOCK: Mutex<()> = Mutex::new(());

fn mock_provider_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_animus-provider-mock"))
}

fn ensure_mock_provider() {
    let bin = mock_provider_binary();
    assert!(bin.is_file(), "Cargo-provided mock provider fixture is missing: {}", bin.display());
}

fn build_request() -> SessionRequest {
    SessionRequest {
        tool: "mock".to_string(),
        model: "mock-fast-1".to_string(),
        prompt: "hello-from-test".to_string(),
        cwd: std::env::current_dir().expect("cwd"),
        project_root: None,
        mcp_endpoint: None,
        mcp_servers: None,
        permission_mode: None,
        timeout_secs: Some(15),
        env_vars: Vec::new(),
        extras: json!({}),
        actor: None,
    }
}

/// Pin the env so discovery sees ONLY the testkit mock provider: an
/// isolated HOME/config/plugin dir keeps the developer's real
/// `~/.animus` registry out, and an isolated project root keeps the
/// repo's `flavors/default.toml` from activating flavor-only plugin
/// scoping (which would filter the mock provider out of discovery).
fn isolated_discovery_env() -> (Vec<protocol::test_utils::EnvVarGuard>, tempfile::TempDir) {
    let isolated = tempfile::tempdir().expect("isolated env tempdir");
    let empty = isolated.path().join("empty");
    std::fs::create_dir_all(&empty).expect("empty plugin dir");
    let guards = vec![
        protocol::test_utils::EnvVarGuard::set("HOME", Some(isolated.path().to_string_lossy().as_ref())),
        protocol::test_utils::EnvVarGuard::set("ANIMUS_CONFIG_DIR", Some(empty.to_string_lossy().as_ref())),
        protocol::test_utils::EnvVarGuard::set("ANIMUS_PLUGIN_DIR", Some(empty.to_string_lossy().as_ref())),
        protocol::test_utils::EnvVarGuard::set(
            "ANIMUS_PLUGIN_PATH",
            Some(mock_provider_binary().parent().expect("fixture binary directory").to_string_lossy().as_ref()),
        ),
    ];
    (guards, isolated)
}

#[tokio::test]
async fn resolver_routes_mock_tool_through_plugin() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    ensure_mock_provider();
    let (_env, project) = isolated_discovery_env();
    let resolver = SessionBackendResolver::with_plugin_discovery(project.path());

    let request = build_request();
    let backend = resolver.resolve(&request).expect("mock plugin should resolve");
    let info = backend.info();
    assert_eq!(info.provider_tool, "mock", "provider_tool should match mock plugin");
    assert!(
        info.display_name.contains("animus-provider-mock"),
        "display_name should reflect plugin: {}",
        info.display_name
    );
}

#[tokio::test]
async fn agent_run_streams_notifications_in_order_through_plugin() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    ensure_mock_provider();
    let (_env, project) = isolated_discovery_env();
    let resolver = SessionBackendResolver::with_plugin_discovery(project.path());

    let request = build_request();
    let mut run = timeout(Duration::from_secs(10), resolver.start_session(request))
        .await
        .expect("start_session should not hang")
        .expect("start_session should succeed");

    let mut events: Vec<SessionEvent> = Vec::new();
    while let Some(event) = timeout(Duration::from_secs(10), run.events.recv()).await.expect("recv should not hang") {
        events.push(event.clone());
        if matches!(event, SessionEvent::Finished { .. }) {
            break;
        }
    }

    assert!(!events.is_empty(), "should observe at least one event");

    // Started must be the first event.
    match events.first() {
        Some(SessionEvent::Started { backend, .. }) => {
            assert!(
                backend.starts_with("plugin:animus-provider-mock"),
                "first event backend label should reflect plugin: {backend}"
            );
        }
        other => panic!("expected first event to be Started, got {other:?}"),
    }

    // Finished must be the last event.
    match events.last() {
        Some(SessionEvent::Finished { exit_code }) => {
            assert_eq!(*exit_code, Some(0), "mock provider should exit cleanly");
        }
        other => panic!("expected last event to be Finished, got {other:?}"),
    }

    // Streaming notifications should reach us as their respective SessionEvents.
    assert!(
        events.iter().any(|e| matches!(e, SessionEvent::Thinking { text } if text.contains("planning"))),
        "Thinking event should be forwarded as agent/thinking notification: {events:?}"
    );
    assert!(
        events.iter().any(|e| matches!(e, SessionEvent::ToolCall { tool_name, .. } if tool_name == "mock.echo")),
        "ToolCall event should be forwarded as agent/toolCall notification"
    );
    assert!(
        events.iter().any(
            |e| matches!(e, SessionEvent::ToolResult { tool_name, success, .. } if tool_name == "mock.echo" && *success)
        ),
        "ToolResult event should be forwarded as agent/toolResult notification"
    );

    let delta_count = events
        .iter()
        .filter(|e| matches!(e, SessionEvent::TextDelta { text } if text.starts_with("mock-stream-")))
        .count();
    assert_eq!(delta_count, 3, "should observe 3 streamed TextDelta events: {events:?}");

    // Final text should contain the prompt-echo at minimum. Provider runtime
    // concatenates streamed TextDelta into the final aggregated `output`, so
    // the FinalText event is the cumulative collected output.
    let final_text =
        events.iter().find_map(|e| if let SessionEvent::FinalText { text } = e { Some(text.clone()) } else { None });
    let final_text = final_text.expect("FinalText event must be present");
    assert!(
        final_text.contains("MOCK_RESULT: hello-from-test"),
        "FinalText should include prompt echo, got: {final_text}"
    );
}

/// When no plugin is discoverable for the requested tool, the resolver MUST
/// surface a hard error pointing the operator at the right install command.
/// As of v0.4.12 there is no in-tree provider fallback.
#[tokio::test]
async fn agent_run_errors_when_provider_plugin_missing() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let empty = tempfile::tempdir().expect("tempdir");
    let isolated_home = tempfile::tempdir().expect("isolated home tempdir");
    let _home = protocol::test_utils::EnvVarGuard::set("HOME", Some(isolated_home.path().to_string_lossy().as_ref()));
    let _path =
        protocol::test_utils::EnvVarGuard::set("ANIMUS_PLUGIN_PATH", Some(empty.path().to_string_lossy().as_ref()));
    let _dir =
        protocol::test_utils::EnvVarGuard::set("ANIMUS_PLUGIN_DIR", Some(empty.path().to_string_lossy().as_ref()));

    let resolver = SessionBackendResolver::with_plugin_discovery(empty.path());
    let request = SessionRequest {
        tool: "claude".to_string(),
        model: "claude-sonnet-4-6".to_string(),
        prompt: "missing-plugin-probe".to_string(),
        cwd: std::env::current_dir().expect("cwd"),
        project_root: None,
        mcp_endpoint: None,
        mcp_servers: None,
        permission_mode: None,
        timeout_secs: Some(5),
        env_vars: Vec::new(),
        extras: json!({}),
        actor: None,
    };

    let err = match resolver.resolve(&request) {
        Ok(_) => panic!("missing provider plugin must surface an error"),
        Err(e) => e,
    };
    let msg = err.to_string();
    assert!(msg.contains("Provider plugin 'claude' not installed"), "actual: {msg}");
    assert!(msg.contains("animus plugin install launchapp-dev/animus-provider-claude"), "actual: {msg}");
}
