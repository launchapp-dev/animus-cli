use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::Result;
use protocol::{AgentRunEvent, OutputStreamType, RunId};
use serde_json::Value;
use sha2::{Digest, Sha256};

pub(super) struct RunEventPersistence {
    run_dir: Option<PathBuf>,
}

impl RunEventPersistence {
    pub(super) fn new(context: &Value, run_id: &RunId) -> Self {
        let run_dir = context
            .get("project_root")
            .and_then(Value::as_str)
            .and_then(|root| build_run_dir(root, &run_id.0));
        Self { run_dir }
    }

    pub(super) fn persist(&mut self, event: &AgentRunEvent) -> Result<()> {
        let Some(run_dir) = &self.run_dir else {
            return Ok(());
        };

        let event_path = run_dir.join("events.jsonl");
        let line = serde_json::to_string(event)?;
        append_line(&event_path, &line)?;

        if let AgentRunEvent::OutputChunk {
            stream_type, text, ..
        } = event
        {
            persist_json_output(run_dir, *stream_type, text)?;
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

fn sanitize_identifier(value: &str) -> String {
    let mut out = String::new();
    for ch in value.chars() {
        match ch {
            'a'..='z' | 'A'..='Z' | '0'..='9' => out.push(ch.to_ascii_lowercase()),
            ' ' | '_' | '-' => out.push('-'),
            _ => {}
        }
    }
    while out.contains("--") {
        out = out.replace("--", "-");
    }
    out = out.trim_matches('-').to_string();
    if out.is_empty() {
        "repo".to_string()
    } else {
        out
    }
}

fn repository_scope_for_path(path: &Path) -> String {
    let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let canonical_display = canonical.to_string_lossy();
    let repo_name = canonical
        .file_name()
        .and_then(|value| value.to_str())
        .map(sanitize_identifier)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "repo".to_string());
    let mut hasher = Sha256::new();
    hasher.update(canonical_display.as_bytes());
    let digest = hasher.finalize();
    let suffix = format!(
        "{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        digest[0], digest[1], digest[2], digest[3], digest[4], digest[5]
    );
    format!("{repo_name}-{suffix}")
}

fn project_runs_root(project_root: &Path) -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    Some(
        home.join(".ao")
            .join(repository_scope_for_path(project_root))
            .join("runs"),
    )
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
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    writeln!(file, "{line}")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::Timestamp;
    use std::sync::{Mutex, OnceLock};

    struct EnvVarGuard {
        key: &'static str,
        previous: Option<String>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: Option<&str>) -> Self {
            let previous = std::env::var(key).ok();
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
            Self { key, previous }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            if let Some(previous) = &self.previous {
                std::env::set_var(self.key, previous);
            } else {
                std::env::remove_var(self.key);
            }
        }
    }

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
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
            .persist(&AgentRunEvent::Started {
                run_id: run_id.clone(),
                timestamp: Timestamp::now(),
            })
            .expect("persist started");
        persistence
            .persist(&AgentRunEvent::OutputChunk {
                run_id: run_id.clone(),
                stream_type: OutputStreamType::Stdout,
                text: "plain-text\n{\"type\":\"turn.completed\"}".to_string(),
            })
            .expect("persist output");

        let run_dir = project_runs_root(&project_root)
            .expect("project-scoped runtime root should resolve")
            .join(&run_id.0);
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
        let expected = home
            .join(".ao")
            .join(repository_scope_for_path(&project_root))
            .join("runs")
            .join("run-test-123");
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
            .persist(&AgentRunEvent::Started {
                run_id: run_id.clone(),
                timestamp: Timestamp::now(),
            })
            .expect("persist with unsafe id should no-op");

        let run_dir = project_runs_root(&project_root)
            .expect("project-scoped runtime root should resolve")
            .join(&run_id.0);
        assert!(!run_dir.exists());
    }

    #[test]
    fn persist_keeps_repo_scoped_runtime_root_under_global_scope_override() {
        let _lock = env_lock().lock().expect("env lock should be available");
        let home = test_root();
        let _home = EnvVarGuard::set("HOME", Some(home.to_string_lossy().as_ref()));
        let _scope = EnvVarGuard::set("AO_RUNNER_SCOPE", Some("global"));
        let override_dir = home.join("override-config");
        let _ao_config = EnvVarGuard::set(
            "AO_CONFIG_DIR",
            Some(override_dir.to_string_lossy().as_ref()),
        );

        let project_root = test_root();
        let run_id = RunId("run-global-scope-123".to_string());
        let context = serde_json::json!({
            "project_root": project_root,
        });
        let mut persistence = RunEventPersistence::new(&context, &run_id);
        persistence
            .persist(&AgentRunEvent::Started {
                run_id: run_id.clone(),
                timestamp: Timestamp::now(),
            })
            .expect("persist started");
        persistence
            .persist(&AgentRunEvent::OutputChunk {
                run_id: run_id.clone(),
                stream_type: OutputStreamType::Stdout,
                text: "{\"kind\":\"global-scope\"}".to_string(),
            })
            .expect("persist output");

        let canonical_run_dir = project_runs_root(&project_root)
            .expect("project-scoped runtime root should resolve")
            .join(&run_id.0);
        let override_run_dir = override_dir.join("runs").join(&run_id.0);
        assert!(canonical_run_dir.join("events.jsonl").exists());
        assert!(canonical_run_dir.join("json-output.jsonl").exists());
        assert!(!override_run_dir.exists());
    }
}
