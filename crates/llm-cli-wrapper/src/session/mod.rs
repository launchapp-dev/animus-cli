pub mod claude;
pub mod codex;
pub mod gemini;
pub mod oai_runner;
pub mod opencode;
pub mod session_backend;
pub mod session_backend_info;
pub mod session_backend_kind;
pub mod session_backend_resolver;
pub mod session_capabilities;
pub mod session_event;
pub mod session_request;
pub mod session_run;
pub mod session_stability;
pub mod subprocess_session_backend;

pub(crate) async fn kill_and_reap_child(child: &mut tokio::process::Child) {
    #[cfg(unix)]
    if let Some(pid) = child.id() {
        let _ = std::process::Command::new("kill")
            .args(["-9", &format!("-{}", pid)])
            .output();
    }
    #[cfg(not(unix))]
    let _ = child.kill().await;
    let _ = child.wait().await;
}

pub use claude::ClaudeSessionBackend;
pub use codex::CodexSessionBackend;
pub use gemini::GeminiSessionBackend;
pub use oai_runner::OaiRunnerSessionBackend;
pub use opencode::OpenCodeSessionBackend;
pub use session_backend::SessionBackend;
pub use session_backend_info::SessionBackendInfo;
pub use session_backend_kind::SessionBackendKind;
pub use session_backend_resolver::SessionBackendResolver;
pub use session_capabilities::SessionCapabilities;
pub use session_event::SessionEvent;
pub use session_request::SessionRequest;
pub use session_run::SessionRun;
pub use session_stability::SessionStability;
pub use subprocess_session_backend::SubprocessSessionBackend;
