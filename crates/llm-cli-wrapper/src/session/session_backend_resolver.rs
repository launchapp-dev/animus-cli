use std::sync::Arc;

use super::{
    claude::ClaudeSessionBackend, codex::CodexSessionBackend, gemini::GeminiSessionBackend,
    oai_runner::OaiRunnerSessionBackend, opencode::OpenCodeSessionBackend, session_backend::SessionBackend,
    session_request::SessionRequest, session_run::SessionRun, subprocess_session_backend::SubprocessSessionBackend,
};
use crate::error::Result;

pub struct SessionBackendResolver {
    claude: Arc<ClaudeSessionBackend>,
    codex: Arc<CodexSessionBackend>,
    gemini: Arc<GeminiSessionBackend>,
    opencode: Arc<OpenCodeSessionBackend>,
    oai_runner: Arc<OaiRunnerSessionBackend>,
    subprocess: Arc<SubprocessSessionBackend>,
}

impl SessionBackendResolver {
    pub fn new() -> Self {
        Self {
            claude: Arc::new(ClaudeSessionBackend::new()),
            codex: Arc::new(CodexSessionBackend::new()),
            gemini: Arc::new(GeminiSessionBackend::new()),
            opencode: Arc::new(OpenCodeSessionBackend::new()),
            oai_runner: Arc::new(OaiRunnerSessionBackend::new()),
            subprocess: Arc::new(SubprocessSessionBackend::new()),
        }
    }

    pub fn fallback_reason(&self, request: &SessionRequest) -> Option<String> {
        if request.tool.eq_ignore_ascii_case("claude")
            || request.tool.eq_ignore_ascii_case("codex")
            || request.tool.eq_ignore_ascii_case("gemini")
            || request.tool.eq_ignore_ascii_case("opencode")
            || request.tool.eq_ignore_ascii_case("oai-runner")
            || request.tool.eq_ignore_ascii_case("ao-oai-runner")
        {
            return None;
        }

        Some(format!("native backend not implemented for tool '{}'; using subprocess backend", request.tool))
    }

    pub fn resolve(&self, request: &SessionRequest) -> Arc<dyn SessionBackend> {
        if request.tool.eq_ignore_ascii_case("claude") {
            return self.claude.clone();
        }
        if request.tool.eq_ignore_ascii_case("codex") {
            return self.codex.clone();
        }
        if request.tool.eq_ignore_ascii_case("gemini") {
            return self.gemini.clone();
        }
        if request.tool.eq_ignore_ascii_case("opencode") {
            return self.opencode.clone();
        }
        if request.tool.eq_ignore_ascii_case("oai-runner") || request.tool.eq_ignore_ascii_case("ao-oai-runner") {
            return self.oai_runner.clone();
        }

        self.subprocess.clone()
    }

    pub async fn start_session(&self, mut request: SessionRequest) -> Result<SessionRun> {
        if let Some(reason) = self.fallback_reason(&request) {
            if let Some(extras) = request.extras.as_object_mut() {
                extras.insert("fallback_reason".to_string(), serde_json::Value::String(reason));
            }
        }

        self.resolve(&request).start_session(request).await
    }
}

impl Default for SessionBackendResolver {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use std::path::PathBuf;

    use super::SessionBackendResolver;
    use crate::session::{SessionEvent, SessionRequest};

    #[test]
    fn resolver_reports_subprocess_fallback_reason() {
        let resolver = SessionBackendResolver::new();
        let request = SessionRequest {
            tool: "sh".to_string(),
            model: String::new(),
            prompt: "hello".to_string(),
            cwd: PathBuf::from("."),
            project_root: None,
            mcp_endpoint: None,
            permission_mode: None,
            timeout_secs: None,
            env_vars: Vec::new(),
            extras: json!({}),
        };

        let reason = resolver.fallback_reason(&request).expect("fallback reason should exist");
        assert!(reason.contains("using subprocess backend"));
    }

    #[test]
    fn resolver_selects_claude_backend_without_fallback() {
        let resolver = SessionBackendResolver::new();
        let request = SessionRequest {
            tool: "claude".to_string(),
            model: "claude-sonnet".to_string(),
            prompt: "hello".to_string(),
            cwd: PathBuf::from("."),
            project_root: None,
            mcp_endpoint: None,
            permission_mode: None,
            timeout_secs: None,
            env_vars: Vec::new(),
            extras: json!({}),
        };

        assert!(resolver.fallback_reason(&request).is_none());
        assert_eq!(resolver.resolve(&request).info().provider_tool, "claude");
    }

    #[test]
    fn resolver_selects_codex_backend_without_fallback() {
        let resolver = SessionBackendResolver::new();
        let request = SessionRequest {
            tool: "codex".to_string(),
            model: "gpt-5".to_string(),
            prompt: "hello".to_string(),
            cwd: PathBuf::from("."),
            project_root: None,
            mcp_endpoint: None,
            permission_mode: None,
            timeout_secs: None,
            env_vars: Vec::new(),
            extras: json!({}),
        };

        assert!(resolver.fallback_reason(&request).is_none());
        assert_eq!(resolver.resolve(&request).info().provider_tool, "codex");
    }

    #[test]
    fn resolver_selects_gemini_backend_without_fallback() {
        let resolver = SessionBackendResolver::new();
        let request = SessionRequest {
            tool: "gemini".to_string(),
            model: "gemini-2.5-pro".to_string(),
            prompt: "hello".to_string(),
            cwd: PathBuf::from("."),
            project_root: None,
            mcp_endpoint: None,
            permission_mode: None,
            timeout_secs: None,
            env_vars: Vec::new(),
            extras: json!({}),
        };

        assert!(resolver.fallback_reason(&request).is_none());
        assert_eq!(resolver.resolve(&request).info().provider_tool, "gemini");
    }

    #[test]
    fn resolver_selects_opencode_backend_without_fallback() {
        let resolver = SessionBackendResolver::new();
        let request = SessionRequest {
            tool: "opencode".to_string(),
            model: "glm-5".to_string(),
            prompt: "hello".to_string(),
            cwd: PathBuf::from("."),
            project_root: None,
            mcp_endpoint: None,
            permission_mode: None,
            timeout_secs: None,
            env_vars: Vec::new(),
            extras: json!({}),
        };

        assert!(resolver.fallback_reason(&request).is_none());
        assert_eq!(resolver.resolve(&request).info().provider_tool, "opencode");
    }

    #[test]
    fn resolver_selects_oai_runner_backend_without_fallback() {
        let resolver = SessionBackendResolver::new();
        let request = SessionRequest {
            tool: "oai-runner".to_string(),
            model: "deepseek/deepseek-chat".to_string(),
            prompt: "hello".to_string(),
            cwd: PathBuf::from("."),
            project_root: None,
            mcp_endpoint: None,
            permission_mode: None,
            timeout_secs: None,
            env_vars: Vec::new(),
            extras: json!({}),
        };

        assert!(resolver.fallback_reason(&request).is_none());
        assert_eq!(resolver.resolve(&request).info().provider_tool, "oai-runner");
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn resolver_starts_session_with_fallback_reason() {
        let resolver = SessionBackendResolver::new();
        let request = SessionRequest {
            tool: "sh".to_string(),
            model: String::new(),
            prompt: String::new(),
            cwd: PathBuf::from("."),
            project_root: None,
            mcp_endpoint: None,
            permission_mode: None,
            timeout_secs: None,
            env_vars: Vec::new(),
            extras: json!({
                "runtime_contract": {
                    "cli": {
                        "launch": {
                            "command": "sh",
                            "args": ["-c", "printf 'resolver\\n'"],
                            "prompt_via_stdin": false
                        }
                    }
                }
            }),
        };

        let mut run = resolver.start_session(request).await.expect("session should start");

        assert_eq!(run.selected_backend, "subprocess");
        assert!(run.fallback_reason.as_deref().is_some_and(|reason| reason.contains("using subprocess backend")));

        let _ = run.events.recv().await.expect("started event");
        let text = run.events.recv().await.expect("text event");
        assert_eq!(text, SessionEvent::TextDelta { text: "resolver".to_string() });
    }
}
