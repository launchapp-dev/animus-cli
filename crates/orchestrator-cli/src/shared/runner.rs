use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
#[cfg(test)]
use protocol::RunId;
use protocol::{AgentRunEvent, OutputStreamType};
use serde_json::Value;

use cli_wrapper::{extract_text_from_line, NormalizedTextEvent};

use crate::{invalid_input_error, AgentControlActionArg, AgentRunArgs};
use protocol::AgentControlAction;

#[cfg(test)]
pub(crate) use workflow_runner_v2::ipc::build_runtime_contract_with_resume;
pub(crate) use workflow_runner_v2::ipc::{
    append_line, build_runtime_contract, collect_json_payload_lines, connect_runner, event_matches_run, run_dir,
    runner_config_dir, write_json_line,
};

pub(crate) fn ensure_safe_run_id(run_id: &str) -> Result<()> {
    workflow_runner_v2::ipc::ensure_safe_run_id(run_id).map_err(|e| invalid_input_error(e.to_string()))
}

impl From<AgentControlActionArg> for AgentControlAction {
    fn from(value: AgentControlActionArg) -> Self {
        match value {
            AgentControlActionArg::Pause => AgentControlAction::Pause,
            AgentControlActionArg::Resume => AgentControlAction::Resume,
            AgentControlActionArg::Terminate => AgentControlAction::Terminate,
        }
    }
}

fn canonicalize_cwd_in_project(path: &str, project_root: &str) -> Result<String> {
    let root = PathBuf::from(project_root);
    let root_canonical =
        root.canonicalize().with_context(|| format!("failed to resolve project root '{}'", project_root))?;
    let candidate = PathBuf::from(path);
    let resolved_candidate = if candidate.is_absolute() { candidate } else { root_canonical.join(candidate) };
    let candidate_canonical = resolved_candidate
        .canonicalize()
        .with_context(|| format!("failed to resolve cwd '{}'", resolved_candidate.display()))?;
    let inside_project_root = candidate_canonical.starts_with(&root_canonical);
    let inside_managed_worktree = is_managed_worktree_for_project(&candidate_canonical, &root_canonical);
    if !inside_project_root && !inside_managed_worktree {
        return Err(anyhow!("Security violation: cwd '{}' is not within project root '{}'", path, project_root));
    }
    Ok(candidate_canonical.to_string_lossy().to_string())
}

fn is_managed_worktree_for_project(candidate_cwd: &Path, project_root: &Path) -> bool {
    let mut cursor = candidate_cwd.parent();
    while let Some(path) = cursor {
        if path.file_name().and_then(|value| value.to_str()) == Some("worktrees") {
            let Some(repo_ao_root) = path.parent() else {
                return false;
            };
            let marker_path = repo_ao_root.join(".project-root");
            let Ok(marker_content) = std::fs::read_to_string(marker_path) else {
                return false;
            };
            let recorded_root = marker_content.trim();
            if recorded_root.is_empty() {
                return false;
            }
            let Ok(recorded_canonical) = Path::new(recorded_root).canonicalize() else {
                return false;
            };
            return recorded_canonical == project_root;
        }
        cursor = path.parent();
    }
    false
}

pub(crate) fn build_agent_context(args: &AgentRunArgs, project_root: &str) -> Result<Value> {
    let mut context = if let Some(context_json) = &args.context_json {
        serde_json::from_str::<Value>(context_json)?
    } else {
        serde_json::json!({})
    };

    let context_obj = context.as_object_mut().ok_or_else(|| anyhow!("agent context must be a JSON object"))?;

    context_obj.entry("tool".to_string()).or_insert_with(|| Value::String(args.tool.clone()));

    if let Some(prompt) = &args.prompt {
        context_obj.entry("prompt".to_string()).or_insert_with(|| Value::String(prompt.clone()));
    }

    let cwd = args
        .cwd
        .clone()
        .or_else(|| context_obj.get("cwd").and_then(Value::as_str).map(ToOwned::to_owned))
        .unwrap_or_else(|| project_root.to_string());
    let cwd = canonicalize_cwd_in_project(&cwd, project_root)?;
    context_obj.insert("cwd".to_string(), Value::String(cwd));
    context_obj.insert("project_root".to_string(), Value::String(project_root.to_string()));

    if let Some(timeout_secs) = args.timeout_secs {
        context_obj.entry("timeout_secs".to_string()).or_insert_with(|| Value::from(timeout_secs));
    }

    if let Some(runtime_contract_json) = &args.runtime_contract_json {
        context_obj.insert("runtime_contract".to_string(), serde_json::from_str::<Value>(runtime_contract_json)?);
    } else if !context_obj.contains_key("runtime_contract") {
        let prompt = context_obj.get("prompt").and_then(Value::as_str).unwrap_or_default();
        let resolved_model = args
            .model
            .as_deref()
            .unwrap_or_else(|| protocol::default_model_for_tool(&args.tool).unwrap_or("claude-sonnet-4-6"));
        if let Some(runtime_contract) = build_runtime_contract(&args.tool, resolved_model, prompt) {
            context_obj.insert("runtime_contract".to_string(), runtime_contract);
        }
    }

    Ok(context)
}

pub(crate) fn print_agent_event(event: &AgentRunEvent, json: bool, tool: &str) -> Result<()> {
    if json {
        println!(
            "{}",
            serde_json::to_string(&serde_json::json!({
                "schema": "ao.agent.event.v1",
                "ok": true,
                "data": event
            }))?
        );
        return Ok(());
    }

    match event {
        AgentRunEvent::Started { run_id, .. } => {
            println!("run {} started", run_id.0);
        }
        AgentRunEvent::OutputChunk { stream_type, text, .. } => match stream_type {
            OutputStreamType::Stderr => eprintln!("{text}"),
            OutputStreamType::Stdout | OutputStreamType::System => {
                let trimmed = text.trim_start();
                if trimmed.starts_with('{') {
                    if let Ok(obj) = serde_json::from_str::<Value>(trimmed) {
                        if obj.get("type").and_then(|v| v.as_str()) == Some("result") {
                            return Ok(());
                        }
                    }
                }
                match extract_text_from_line(text, tool) {
                    NormalizedTextEvent::TextChunk { text: t } | NormalizedTextEvent::FinalResult { text: t } => {
                        use std::io::IsTerminal;
                        if std::io::stdout().is_terminal() {
                            print!("{}", termimad::text(&t));
                        } else {
                            print!("{t}");
                        }
                    }
                    NormalizedTextEvent::Ignored => {
                        if !trimmed.starts_with('{') {
                            println!("{text}");
                        }
                    }
                }
            }
        },
        AgentRunEvent::Metadata { run_id, cost, tokens } => {
            println!("run {} metadata: cost={cost:?} tokens={tokens:?}", run_id.0);
        }
        AgentRunEvent::Error { run_id, error } => {
            eprintln!("run {} error: {error}", run_id.0);
        }
        AgentRunEvent::Finished { run_id, exit_code, duration_ms } => {
            println!("run {} finished: exit_code={exit_code:?} duration_ms={duration_ms}", run_id.0);
        }
        AgentRunEvent::ToolCall { run_id, tool_info } => {
            println!("run {} tool_call {}", run_id.0, tool_info.tool_name);
        }
        AgentRunEvent::ToolResult { run_id, result_info } => {
            println!("run {} tool_result {} success={}", run_id.0, result_info.tool_name, result_info.success);
        }
        AgentRunEvent::Artifact { run_id, artifact_info } => {
            println!("run {} artifact {}", run_id.0, artifact_info.artifact_id);
        }
        AgentRunEvent::Thinking { run_id, content } => {
            println!("run {} thinking: {} chars", run_id.0, content.chars().count());
        }
    }

    Ok(())
}

pub(crate) fn persist_agent_event(run_dir: &Path, event: &AgentRunEvent) -> Result<()> {
    let path = run_dir.join("events.jsonl");
    let line = serde_json::to_string(event)?;
    append_line(&path, &line)
}

pub(crate) fn persist_json_output(run_dir: &Path, stream_type: OutputStreamType, text: &str) -> Result<()> {
    let path = run_dir.join("json-output.jsonl");
    for (raw, payload) in collect_json_payload_lines(text) {
        let timestamp_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_millis())
            .unwrap_or_default();
        let entry = serde_json::json!({
            "timestamp_ms": timestamp_ms,
            "stream_type": stream_type_label(stream_type),
            "raw": raw,
            "payload": payload,
        });
        append_line(&path, &serde_json::to_string(&entry)?)?;
    }
    Ok(())
}

fn stream_type_label(stream_type: OutputStreamType) -> &'static str {
    match stream_type {
        OutputStreamType::Stdout => "stdout",
        OutputStreamType::Stderr => "stderr",
        OutputStreamType::System => "system",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use tempfile::TempDir;
    use tokio::io::{AsyncBufReadExt, BufReader};

    use protocol::test_utils::EnvVarGuard;
    use protocol::IpcAuthRequest;

    fn write_config(dir: &Path, token: Option<&str>) {
        let payload = serde_json::json!({ "agent_runner_token": token });
        std::fs::write(
            dir.join("config.json"),
            serde_json::to_string_pretty(&payload).expect("serialize config payload"),
        )
        .expect("write config file");
    }

    #[test]
    fn runner_config_dir_defaults_to_project_scope() {
        let _lock = crate::shared::test_env_lock().lock().expect("env lock should be available");
        let _config = EnvVarGuard::set("AO_CONFIG_DIR", None);
        let _runner_config = EnvVarGuard::set("AO_RUNNER_CONFIG_DIR", None);
        let _legacy_config = EnvVarGuard::set("AGENT_ORCHESTRATOR_CONFIG_DIR", None);
        let _scope = EnvVarGuard::set("AO_RUNNER_SCOPE", None);

        let project_root = Path::new("/tmp/project-root");
        let resolved = runner_config_dir(project_root);
        let resolved_str = resolved.to_string_lossy();
        assert!(resolved_str.contains("runner"), "expected runner suffix in resolved config dir: {resolved_str}");
    }

    #[test]
    fn runner_config_dir_shortens_long_unix_socket_paths() {
        let _lock = crate::shared::test_env_lock().lock().expect("env lock should be available");
        let long_root = Path::new("/tmp").join("a".repeat(120));
        let _config = EnvVarGuard::set("AO_CONFIG_DIR", None);
        let _runner_config = EnvVarGuard::set("AO_RUNNER_CONFIG_DIR", None);
        let _legacy_config = EnvVarGuard::set("AGENT_ORCHESTRATOR_CONFIG_DIR", None);
        let _scope = EnvVarGuard::set("AO_RUNNER_SCOPE", None);

        let resolved = runner_config_dir(&long_root);
        let socket_path = resolved.join("agent-runner.sock");
        assert!(
            socket_path.to_string_lossy().len() <= protocol::MAX_UNIX_SOCKET_PATH_LEN,
            "socket path should be shortened: {}",
            socket_path.display()
        );
    }

    #[test]
    fn build_agent_context_accepts_managed_worktree_cwd() {
        let _lock = crate::shared::test_env_lock().lock().expect("env lock should be available");
        let _config = EnvVarGuard::set("AO_CONFIG_DIR", None);
        let _runner_config = EnvVarGuard::set("AO_RUNNER_CONFIG_DIR", None);
        let _legacy_config = EnvVarGuard::set("AGENT_ORCHESTRATOR_CONFIG_DIR", None);

        let tmp = TempDir::new().expect("temp dir");
        let project_root = tmp.path().join("repo");
        let managed_ao = tmp.path().join("ao-scope");
        let worktree = managed_ao.join("worktrees").join("task-42");
        std::fs::create_dir_all(&worktree).expect("create worktree");
        std::fs::create_dir_all(&project_root).expect("create project root");
        let canonical_root = project_root.canonicalize().expect("canonicalize root");
        std::fs::write(managed_ao.join(".project-root"), canonical_root.to_string_lossy().as_bytes())
            .expect("write marker");

        let args = AgentRunArgs {
            run_id: None,
            tool: "claude".to_string(),
            model: Some("claude-sonnet-4-6".to_string()),
            prompt: Some("hello".to_string()),
            cwd: Some(worktree.to_string_lossy().to_string()),
            context_json: None,
            timeout_secs: None,
            runtime_contract_json: None,
            detach: false,
            stream: true,
            save_jsonl: false,
            jsonl_dir: None,
            start_runner: false,
            runner_scope: None,
        };

        let context = build_agent_context(&args, &canonical_root.to_string_lossy()).expect("build context");
        let cwd = context["cwd"].as_str().expect("cwd should be present");
        assert!(cwd.contains("worktrees/task-42"), "cwd should point to worktree: {cwd}");
    }

    #[test]
    fn run_dir_stays_repo_scoped_when_runner_scope_is_global() {
        let _lock = crate::shared::test_env_lock().lock().expect("env lock should be available");
        let _scope = EnvVarGuard::set("AO_RUNNER_SCOPE", Some("global"));
        let project_root = "/tmp/project-root";
        let run_id = RunId("run-1234".to_string());
        let dir = run_dir(project_root, &run_id, None);
        let dir_str = dir.to_string_lossy();
        assert!(dir_str.contains("run-1234"), "run dir should contain run_id: {dir_str}");
    }

    #[test]
    fn collect_json_payload_lines_parses_mixed_output() {
        let _lock = crate::shared::test_env_lock().lock().expect("env lock should be available");
        let input = "plain text\n{\"key\":\"value\"}\nmore text\n[1,2,3]\n42\n";
        let rows = collect_json_payload_lines(input);
        assert_eq!(rows.len(), 2);
    }

    #[test]
    fn claude_bypass_permissions_is_disabled_by_default() {
        let _lock = crate::shared::test_env_lock().lock().expect("env lock should be available");
        let _bypass = EnvVarGuard::set("AO_CLAUDE_BYPASS_PERMISSIONS", None);
        let contract =
            build_runtime_contract("claude", "claude-opus-4-1", "hello").expect("runtime contract should build");
        let args = contract
            .pointer("/cli/launch/args")
            .and_then(Value::as_array)
            .expect("launch args should be present")
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>();
        assert!(!args.contains(&"--permission-mode"));
        assert!(!args.contains(&"bypassPermissions"));
    }

    #[test]
    fn claude_bypass_permissions_respects_disable_toggle() {
        let _lock = crate::shared::test_env_lock().lock().expect("env lock should be available");
        let _bypass = EnvVarGuard::set("AO_CLAUDE_BYPASS_PERMISSIONS", Some("false"));
        let contract =
            build_runtime_contract("claude", "claude-opus-4-1", "hello").expect("runtime contract should build");
        let args = contract
            .pointer("/cli/launch/args")
            .and_then(Value::as_array)
            .expect("launch args should be present")
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>();
        assert!(!args.contains(&"--permission-mode"));
        assert!(!args.contains(&"bypassPermissions"));
    }

    #[test]
    fn claude_bypass_permissions_treats_empty_value_as_disabled() {
        let _lock = crate::shared::test_env_lock().lock().expect("env lock should be available");
        let _bypass = EnvVarGuard::set("AO_CLAUDE_BYPASS_PERMISSIONS", Some(""));
        let contract =
            build_runtime_contract("claude", "claude-opus-4-1", "hello").expect("runtime contract should build");
        let args = contract
            .pointer("/cli/launch/args")
            .and_then(Value::as_array)
            .expect("launch args should be present")
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>();
        assert!(!args.contains(&"--permission-mode"));
        assert!(!args.contains(&"bypassPermissions"));
    }

    #[test]
    fn inject_claude_permission_mode_override_is_disabled_by_default() {
        let _lock = crate::shared::test_env_lock().lock().expect("env lock should be available");
        let _bypass = EnvVarGuard::set("AO_CLAUDE_BYPASS_PERMISSIONS", None);
        let contract =
            build_runtime_contract("claude", "claude-opus-4-1", "hello").expect("runtime contract should build");
        let args = contract
            .pointer("/cli/launch/args")
            .and_then(Value::as_array)
            .expect("launch args should be present")
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>();
        assert!(!args.contains(&"--permission-mode"));
        assert!(!args.contains(&"bypassPermissions"));
    }

    #[test]
    fn inject_claude_permission_mode_override_respects_enable_toggle() {
        let _lock = crate::shared::test_env_lock().lock().expect("env lock should be available");
        let _bypass = EnvVarGuard::set("AO_CLAUDE_BYPASS_PERMISSIONS", Some("true"));
        let mut contract =
            build_runtime_contract("claude", "claude-opus-4-1", "hello").expect("runtime contract should build");
        workflow_runner_v2::runtime_support::inject_claude_permission_mode(&mut contract, "claude");
        let args = contract
            .pointer("/cli/launch/args")
            .and_then(Value::as_array)
            .expect("launch args should be present")
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>();
        assert!(args.contains(&"--permission-mode"));
        assert!(args.contains(&"bypassPermissions"));
    }

    #[test]
    fn inject_claude_permission_mode_override_respects_disable_toggle() {
        let _lock = crate::shared::test_env_lock().lock().expect("env lock should be available");
        let _bypass = EnvVarGuard::set("AO_CLAUDE_BYPASS_PERMISSIONS", Some("false"));
        let mut contract =
            build_runtime_contract("claude", "claude-opus-4-1", "hello").expect("runtime contract should build");
        workflow_runner_v2::runtime_support::inject_claude_permission_mode(&mut contract, "claude");
        let args = contract
            .pointer("/cli/launch/args")
            .and_then(Value::as_array)
            .expect("launch args should be present")
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>();
        assert!(!args.contains(&"--permission-mode"));
        assert!(!args.contains(&"bypassPermissions"));
    }

    #[test]
    fn inject_claude_permission_mode_override_treats_empty_toggle_as_disabled() {
        let _lock = crate::shared::test_env_lock().lock().expect("env lock should be available");
        let _bypass = EnvVarGuard::set("AO_CLAUDE_BYPASS_PERMISSIONS", Some(""));
        let mut contract =
            build_runtime_contract("claude", "claude-opus-4-1", "hello").expect("runtime contract should build");
        workflow_runner_v2::runtime_support::inject_claude_permission_mode(&mut contract, "claude");
        let args = contract
            .pointer("/cli/launch/args")
            .and_then(Value::as_array)
            .expect("launch args should be present")
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>();
        assert!(!args.contains(&"--permission-mode"));
        assert!(!args.contains(&"bypassPermissions"));
    }

    #[test]
    fn build_runtime_contract_with_resume_injects_claude_session_id() {
        let _lock = crate::shared::test_env_lock().lock().expect("env lock");
        let _bypass = EnvVarGuard::set("AO_CLAUDE_BYPASS_PERMISSIONS", None);
        let plan = orchestrator_core::runtime_contract::CliSessionResumePlan {
            mode: orchestrator_core::runtime_contract::CliSessionResumeMode::NativeId,
            session_key: "wf:test-wf:implementation".to_string(),
            session_id: Some("session-abc-123".to_string()),
            summary_seed: None,
            reused: false,
            phase_thread_isolated: true,
        };
        let contract = build_runtime_contract_with_resume("claude", "claude-sonnet-4-6", "hello", Some(&plan))
            .expect("runtime contract should build");
        let args = contract
            .pointer("/cli/launch/args")
            .and_then(Value::as_array)
            .expect("launch args should be present")
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>();
        assert!(args.contains(&"--session-id"));
        assert!(args.contains(&"session-abc-123"));
        let session = contract.pointer("/cli/session");
        assert!(session.is_some());
        assert_eq!(
            session.and_then(|s| s.get("session_key")).and_then(Value::as_str),
            Some("wf:test-wf:implementation")
        );
        assert_eq!(session.and_then(|s| s.get("reused")).and_then(Value::as_bool), Some(false));
    }

    #[test]
    fn build_runtime_contract_with_resume_injects_gemini_resume_flag() {
        let _lock = crate::shared::test_env_lock().lock().expect("env lock");
        let plan = orchestrator_core::runtime_contract::CliSessionResumePlan {
            mode: orchestrator_core::runtime_contract::CliSessionResumeMode::NativeId,
            session_key: "wf:test-wf:research".to_string(),
            session_id: Some("session-def-456".to_string()),
            summary_seed: None,
            reused: true,
            phase_thread_isolated: true,
        };
        let contract = build_runtime_contract_with_resume("gemini", "gemini-2.5-pro", "hello", Some(&plan))
            .expect("runtime contract should build");
        let args = contract
            .pointer("/cli/launch/args")
            .and_then(Value::as_array)
            .expect("launch args should be present")
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>();
        assert!(args.contains(&"--resume"));
        assert!(args.contains(&"session-def-456"));
    }

    #[test]
    fn build_runtime_contract_with_resume_codex_uses_exec_resume_last() {
        let _lock = crate::shared::test_env_lock().lock().expect("env lock");
        let plan = orchestrator_core::runtime_contract::CliSessionResumePlan {
            mode: orchestrator_core::runtime_contract::CliSessionResumeMode::NativeId,
            session_key: "wf:test-wf:implementation".to_string(),
            session_id: Some("session-ghi-789".to_string()),
            summary_seed: None,
            reused: true,
            phase_thread_isolated: true,
        };
        let contract = build_runtime_contract_with_resume(
            "codex",
            protocol::default_model_for_tool("codex").unwrap_or("gpt-5.4"),
            "hello",
            Some(&plan),
        )
        .expect("runtime contract should build");
        let args = contract
            .pointer("/cli/launch/args")
            .and_then(Value::as_array)
            .expect("launch args should be present")
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>();
        assert!(args.contains(&"resume"));
        assert!(args.contains(&"--last"));
        assert!(!args.contains(&"--session-id"));
    }

    #[test]
    fn build_runtime_contract_without_resume_has_no_session_metadata() {
        let _lock = crate::shared::test_env_lock().lock().expect("env lock");
        let contract =
            build_runtime_contract("claude", "claude-sonnet-4-6", "hello").expect("runtime contract should build");
        let session = contract.pointer("/cli/session");
        assert!(session.is_none());
        let args = contract
            .pointer("/cli/launch/args")
            .and_then(Value::as_array)
            .expect("launch args should be present")
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>();
        assert!(!args.contains(&"--session-id"));
    }

    #[test]
    fn authenticate_runner_stream_uses_scoped_config_dir_token() {
        let _lock = crate::shared::test_env_lock().lock().expect("env lock");
        let global_dir = TempDir::new().expect("global temp dir");
        let scoped_dir = TempDir::new().expect("scoped temp dir");
        write_config(global_dir.path(), Some("global-token"));
        write_config(scoped_dir.path(), Some("scoped-token"));

        let global_override = global_dir.path().to_string_lossy().to_string();
        let _ao_config = EnvVarGuard::set("AO_CONFIG_DIR", Some(&global_override));
        let _legacy_config = EnvVarGuard::set("AGENT_ORCHESTRATOR_CONFIG_DIR", Some(&global_override));
        let _token_override = EnvVarGuard::set("AGENT_RUNNER_TOKEN", None);

        let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
        runtime.block_on(async {
            let (mut client, server) = tokio::io::duplex(1024);
            let server_task = tokio::spawn(async move {
                let mut reader = BufReader::new(server);
                let mut line = String::new();
                let read_len = reader.read_line(&mut line).await.expect("read auth request");
                assert!(read_len > 0, "expected auth request line");

                let request: IpcAuthRequest = serde_json::from_str(line.trim()).expect("parse auth request");
                assert_eq!(request.token, "scoped-token");

                let mut server = reader.into_inner();
                write_json_line(&mut server, &protocol::IpcAuthResult::ok()).await.expect("write auth response");
            });

            workflow_runner_v2::ipc::authenticate_runner_stream(&mut client, scoped_dir.path())
                .await
                .expect("authenticate runner stream");

            server_task.await.expect("join server task");
        });
    }

    #[test]
    fn authenticate_runner_stream_fails_when_scoped_token_missing() {
        let _lock = crate::shared::test_env_lock().lock().expect("env lock");
        let scoped_dir = TempDir::new().expect("scoped temp dir");
        write_config(scoped_dir.path(), None);
        let _token_override = EnvVarGuard::set("AGENT_RUNNER_TOKEN", None);

        let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
        runtime.block_on(async {
            let (mut client, _server) = tokio::io::duplex(256);
            let error = workflow_runner_v2::ipc::authenticate_runner_stream(&mut client, scoped_dir.path())
                .await
                .expect_err("authentication should fail without runner token");
            assert!(
                error.to_string().contains("agent runner token unavailable"),
                "error should mention missing runner token: {error}"
            );
        });
    }
}
