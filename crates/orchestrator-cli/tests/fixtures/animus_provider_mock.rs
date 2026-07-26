//! Deterministic provider used by the plugin-host end-to-end tests.
//!
//! This is an in-package binary target so Cargo builds it before integration
//! tests and exposes its exact path via `CARGO_BIN_EXE_animus-provider-mock`.

use animus_plugin_runtime::{run_provider, ProviderBackend, ProviderInfo};
use animus_session_backend::error::Result as SessionResult;
use animus_session_backend::session::{SessionEvent, SessionRequest, SessionRun};
use anyhow::Result;
use async_trait::async_trait;
use serde_json::json;
use std::fs::OpenOptions;
use std::io::Write;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use uuid::Uuid;

/// A process-level acceptance test can put this marker in its prompt to make
/// provider execution externally observable and long enough to overlap a
/// second independent CLI process. The fixture is test-only, and the counter
/// remains inside the request's already-authorized working directory.
const PROCESS_E2E_MARKER: &str = "ANIMUS_CHAT_POSTGRES_PROCESS_E2E";
const PROCESS_E2E_COUNTER: &str = ".animus-provider-mock-executions";
const PROCESS_E2E_RELEASE: &str = ".animus-provider-mock-release";
const PROCESS_E2E_MAX_HOLD: Duration = Duration::from_secs(15);

const INFO: ProviderInfo = ProviderInfo {
    plugin_name: "animus-provider-mock",
    plugin_version: env!("CARGO_PKG_VERSION"),
    description: "Deterministic provider fixture for Animus plugin host integration tests",
    default_tool: "mock",
    default_model: "mock-fast-1",
};

struct MockBackend;

#[async_trait]
impl ProviderBackend for MockBackend {
    async fn start(&self, request: SessionRequest, resume_session: Option<&str>) -> SessionResult<SessionRun> {
        let session_id = resume_session.map(ToOwned::to_owned).unwrap_or_else(|| Uuid::new_v4().to_string());
        let backend_label = "mock-native".to_string();
        let prompt = request.prompt.clone();
        let model = request.model.clone();
        let process_e2e = prompt.contains(PROCESS_E2E_MARKER);
        let process_e2e_release = request.cwd.join(PROCESS_E2E_RELEASE);
        if process_e2e {
            let counter_path = request.cwd.join(PROCESS_E2E_COUNTER);
            let mut counter = OpenOptions::new().create(true).append(true).open(counter_path)?;
            counter.write_all(b"start\n")?;
            counter.sync_all()?;
        }
        let (tx, rx) = mpsc::channel(16);
        let event_session_id = session_id.clone();
        let event_backend = backend_label.clone();

        tokio::spawn(async move {
            let _ = tx
                .send(SessionEvent::Started { backend: event_backend, session_id: Some(event_session_id), pid: None })
                .await;
            if process_e2e {
                let started = Instant::now();
                while !process_e2e_release.is_file() && started.elapsed() < PROCESS_E2E_MAX_HOLD {
                    tokio::time::sleep(Duration::from_millis(25)).await;
                }
            }
            let _ = tx.send(SessionEvent::Thinking { text: "mock: planning response".to_string() }).await;
            let _ = tx
                .send(SessionEvent::ToolCall {
                    tool_name: "mock.echo".to_string(),
                    arguments: json!({ "prompt": prompt, "model": model }),
                    server: Some("mock".to_string()),
                })
                .await;
            let _ = tx
                .send(SessionEvent::ToolResult {
                    tool_name: "mock.echo".to_string(),
                    output: json!({ "ok": true }),
                    success: true,
                })
                .await;
            for chunk in ["mock-stream-1 ", "mock-stream-2 ", "mock-stream-3"] {
                let _ = tx.send(SessionEvent::TextDelta { text: chunk.to_string() }).await;
            }
            let _ = tx.send(SessionEvent::FinalText { text: format!("MOCK_RESULT: {prompt}") }).await;
            let _ = tx.send(SessionEvent::Metadata { metadata: json!({ "model": model }) }).await;
            let _ = tx.send(SessionEvent::Finished { exit_code: Some(0) }).await;
        });

        Ok(SessionRun {
            session_id: Some(session_id),
            events: rx,
            selected_backend: backend_label,
            fallback_reason: None,
            pid: None,
        })
    }

    async fn cancel(&self, _session_id: &str) -> SessionResult<()> {
        Ok(())
    }
}

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> Result<()> {
    run_provider(INFO, MockBackend).await
}
