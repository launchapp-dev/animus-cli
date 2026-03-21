use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::Result;
use protocol::{AgentRunEvent, CompletionInfo, OutputStreamType, RunId};
use serde_json::Value;

pub(super) struct RunEventPersistence {
    run_dir: Option<PathBuf>,
    total_input_tokens: u64,
    total_output_tokens: u64,
    total_cost_usd: f64,
}

impl RunEventPersistence {
    pub(super) fn new(context: &Value, run_id: &RunId) -> Self {
        let run_dir =
            context.get("project_root").and_then(Value::as_str).and_then(|root| build_run_dir(root, &run_id.0));
        Self { run_dir, total_input_tokens: 0, total_output_tokens: 0, total_cost_usd: 0.0 }
    }

    pub(super) fn persist(&mut self, event: &AgentRunEvent) -> Result<()> {
        let Some(run_dir) = &self.run_dir else {
            return Ok(());
        };

        let event_path = run_dir.join("events.jsonl");
        let line = serde_json::to_string(event)?;
        append_line(&event_path, &line)?;

        match event {
            AgentRunEvent::OutputChunk { stream_type, text, .. } => {
                persist_json_output(run_dir, *stream_type, text)?;
            }
            AgentRunEvent::Metadata { tokens, cost, .. } => {
                if let Some(t) = tokens {
                    self.total_input_tokens += t.input as u64;
                    self.total_output_tokens += t.output as u64;
                }
                if let Some(c) = cost {
                    self.total_cost_usd += c;
                }
            }
            AgentRunEvent::Finished { exit_code, duration_ms, .. } => {
                let total_tokens = self.total_input_tokens + self.total_output_tokens;
                let info = CompletionInfo {
                    exit_code: *exit_code,
                    duration_ms: *duration_ms,
                    success: exit_code.unwrap_or(1) == 0,
                    total_cost: if self.total_cost_usd > 0.0 { Some(self.total_cost_usd) } else { None },
                    total_tokens: if total_tokens > 0 { Some(total_tokens) } else { None },
                };
                let completion_path = run_dir.join("completion_info.json");
                if let Ok(json) = serde_json::to_string_pretty(&info) {
                    let _ = std::fs::write(&completion_path, json);
                }
            }
            _ => {}
        }

        Ok(())
    }
}

fn build_run_dir(project_root: &str, run_id: &str) -> Option<PathBuf> {
    if run_id.trim().is_empty() {
        return None;
    }
    if run_id.contains('/') || run_id.contains('\\') || run_id.contains("..") {
        return None;
    }
    Some(project_runs_root(Path::new(project_root))?.join(run_id))
}

fn project_runs_root(project_root: &Path) -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    Some(home.join(".ao").join(protocol::repository_scope_for_path(project_root)).join("runs"))
}

fn persist_json_output(run_dir: &Path, stream_type: OutputStreamType, text: &str) -> Result<()> {
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

fn collect_json_payload_lines(text: &str) -> Vec<(String, Value)> {
    text.lines()
        .filter_map(|line| {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                return None;
            }
            let parsed = serde_json::from_str::<Value>(trimmed).ok()?;
            if parsed.is_object() || parsed.is_array() {
                Some((trimmed.to_string(), parsed))
            } else {
                None
            }
        })
        .collect()
}

fn append_line(path: &Path, line: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = std::fs::OpenOptions::new().create(true).append(true).open(path)?;
    writeln!(file, "{line}")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::Timestamp;
    use std::sync::Mutex;

    use protocol::test_utils::EnvVarGuard;

    fn env_lock() -> &'static Mutex<()> {
        crate::test_env_lock()
    }

    fn test_root() -> PathBuf {
        let base = std::env::temp_dir().join(format!(
            "agent-runner-persist-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or_default()
        ));
        std::fs::create_dir_all(&base).expect("create temp root");
        base
    }

    #[test]
    fn persist_writes_events_and_json_output() {
        let _lock = env_lock().lock().expect("env lock should be available");
        let home = test_root();
        let _home = EnvVarGuard::set("HOME", Some(home.to_string_lossy().as_ref()));

        let project_root = test_root();
        let run_id = RunId("run-test-123".to_string());
        let context = serde_json::json!({
            "project_root": project_root,
        });
        let mut persistence = RunEventPersistence::new(&context, &run_id);

        persistence
            .persist(&AgentRunEvent::Started { run_id: run_id.clone(), timestamp: Timestamp::now() })
            .expect("persist started");
        persistence
            .persist(&AgentRunEvent::OutputChunk {
                run_id: run_id.clone(),
                stream_type: OutputStreamType::Stdout,
                text: "plain-text\n{\"type\":\"turn.completed\"}".to_string(),
            })
            .expect("persist output");

        let run_dir =
            project_runs_root(&project_root).expect("project-scoped runtime root should resolve").join(&run_id.0);
        let events_path = run_dir.join("events.jsonl");
        let json_output_path = run_dir.join("json-output.jsonl");

        assert!(events_path.exists());
        assert!(json_output_path.exists());

        let events_raw = std::fs::read_to_string(events_path).expect("read events");
        let event_lines: Vec<&str> = events_raw.lines().collect();
        assert_eq!(event_lines.len(), 2);

        let json_output_raw = std::fs::read_to_string(json_output_path).expect("read json output");
        let json_lines: Vec<&str> = json_output_raw.lines().collect();
        assert_eq!(json_lines.len(), 1);
        assert!(json_lines[0].contains("\"turn.completed\""));
    }

    #[test]
    fn build_run_dir_uses_scoped_global_runtime_root() {
        let _lock = env_lock().lock().expect("env lock should be available");
        let home = test_root();
        let _home = EnvVarGuard::set("HOME", Some(home.to_string_lossy().as_ref()));

        let project_root = test_root();
        let run_dir = build_run_dir(project_root.to_string_lossy().as_ref(), "run-test-123")
            .expect("run dir should resolve for safe run id");
        let expected =
            home.join(".ao").join(protocol::repository_scope_for_path(&project_root)).join("runs").join("run-test-123");
        assert_eq!(run_dir, expected);
    }

    #[test]
    fn persist_ignores_unsafe_run_id() {
        let project_root = test_root();
        let run_id = RunId("../escape".to_string());
        let context = serde_json::json!({
            "project_root": project_root,
        });
        let mut persistence = RunEventPersistence::new(&context, &run_id);

        persistence
            .persist(&AgentRunEvent::Started { run_id: run_id.clone(), timestamp: Timestamp::now() })
            .expect("persist with unsafe id should no-op");

        let run_dir =
            project_runs_root(&project_root).expect("project-scoped runtime root should resolve").join(&run_id.0);
        assert!(!run_dir.exists());
    }

    #[test]
    fn persist_keeps_repo_scoped_runtime_root_under_global_scope_override() {
        let _lock = env_lock().lock().expect("env lock should be available");
        let home = test_root();
        let _home = EnvVarGuard::set("HOME", Some(home.to_string_lossy().as_ref()));
        let _scope = EnvVarGuard::set("AO_RUNNER_SCOPE", Some("global"));
        let override_dir = home.join("override-config");
        let _ao_config = EnvVarGuard::set("AO_CONFIG_DIR", Some(override_dir.to_string_lossy().as_ref()));

        let project_root = test_root();
        let run_id = RunId("run-global-scope-123".to_string());
        let context = serde_json::json!({
            "project_root": project_root,
        });
        let mut persistence = RunEventPersistence::new(&context, &run_id);
        persistence
            .persist(&AgentRunEvent::Started { run_id: run_id.clone(), timestamp: Timestamp::now() })
            .expect("persist started");
        persistence
            .persist(&AgentRunEvent::OutputChunk {
                run_id: run_id.clone(),
                stream_type: OutputStreamType::Stdout,
                text: "{\"kind\":\"global-scope\"}".to_string(),
            })
            .expect("persist output");

        let canonical_run_dir =
            project_runs_root(&project_root).expect("project-scoped runtime root should resolve").join(&run_id.0);
        let override_run_dir = override_dir.join("runs").join(&run_id.0);
        assert!(canonical_run_dir.join("events.jsonl").exists());
        assert!(canonical_run_dir.join("json-output.jsonl").exists());
        assert!(!override_run_dir.exists());
    }
}
