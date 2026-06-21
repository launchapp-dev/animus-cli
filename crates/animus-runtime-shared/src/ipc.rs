//! Persistence + runtime-contract helpers shared across the CLI and the
//! daemon. The original Unix-socket bridge to the standalone `agent-runner`
//! sidecar was deleted in v0.5.3 alongside the sidecar itself; provider
//! plugins now own session execution end to end.
//!
//! What's still here:
//!
//! - [`run_dir`] / [`persist_run_event`] / [`persist_json_output`] /
//!   [`append_line`] / [`collect_json_payload_lines`] — the JSONL log
//!   layout that `animus output` and `ops_mcp` read.
//! - [`build_runtime_contract`] / [`build_runtime_contract_with_resume`] /
//!   [`build_runtime_contract_with_resume_and_mcp_config`] — the
//!   `runtime_contract` envelope provider plugins consume via
//!   `SessionRequest::extras`.
//! - [`event_matches_run`] / [`ensure_safe_run_id`] / [`write_json_line`] —
//!   shared validation + writer helpers.

#![allow(dead_code)]

use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};
use orchestrator_core::runtime_contract;
use protocol::{AgentRunEvent, OutputStreamType, RunId};
use serde_json::Value;

use tokio::io::{AsyncWrite, AsyncWriteExt};

fn scoped_ao_root(project_root: &Path) -> Option<PathBuf> {
    protocol::scoped_state_root(project_root)
}

pub async fn write_json_line<W: AsyncWrite + Unpin, T: serde::Serialize>(writer: &mut W, payload: &T) -> Result<()> {
    let json = serde_json::to_string(payload)?;
    writer.write_all(json.as_bytes()).await?;
    writer.write_all(b"\n").await?;
    writer.flush().await?;
    Ok(())
}

pub fn build_runtime_contract(tool: &str, model: &str, prompt: &str) -> Option<Value> {
    build_runtime_contract_with_resume(tool, model, prompt, None)
}

pub fn build_runtime_contract_with_resume(
    tool: &str,
    model: &str,
    prompt: &str,
    resume_plan: Option<&orchestrator_core::runtime_contract::CliSessionResumePlan>,
) -> Option<Value> {
    build_runtime_contract_with_resume_and_mcp_config(
        tool,
        model,
        prompt,
        resume_plan,
        &protocol::McpRuntimeConfig::default(),
    )
}

/// Variant of [`build_runtime_contract_with_resume`] that threads
/// host-supplied `mcp_config.endpoint` and `mcp_config.agent_id` into the
/// runtime contract.
pub fn build_runtime_contract_with_resume_and_mcp_config(
    tool: &str,
    model: &str,
    prompt: &str,
    resume_plan: Option<&orchestrator_core::runtime_contract::CliSessionResumePlan>,
    mcp_config: &protocol::McpRuntimeConfig,
) -> Option<Value> {
    let mcp_endpoint =
        mcp_config.endpoint.as_deref().map(str::trim).filter(|value| !value.is_empty()).map(ToOwned::to_owned);
    let mcp_agent_id =
        mcp_config.agent_id.as_deref().map(str::trim).filter(|value| !value.is_empty()).map(ToOwned::to_owned);
    let mcp_stdio_command =
        mcp_config.stdio_command.as_deref().map(str::trim).filter(|value| !value.is_empty()).map(ToOwned::to_owned);

    let mut runtime_contract = runtime_contract::build_runtime_contract(
        tool,
        model,
        prompt,
        resume_plan,
        None,
        mcp_endpoint.as_deref(),
        mcp_agent_id.as_deref(),
    )?;

    if mcp_endpoint.is_none() && mcp_stdio_command.is_some() {
        let cli_supports_mcp =
            runtime_contract.pointer("/cli/capabilities/supports_mcp").and_then(Value::as_bool).unwrap_or(false);
        if cli_supports_mcp {
            if let Some(mcp) = runtime_contract.get_mut("mcp").and_then(Value::as_object_mut) {
                mcp.insert("enforce_only".to_string(), Value::Bool(true));
                let agent_id_for_prefixes = mcp_agent_id.as_deref().unwrap_or("animus");
                let prefixes = protocol::default_allowed_tool_prefixes(agent_id_for_prefixes);
                mcp.insert("allowed_tool_prefixes".to_string(), serde_json::json!(prefixes));
            }
        }
    }
    Some(runtime_contract)
}

pub fn event_matches_run(event: &AgentRunEvent, run_id: &RunId) -> bool {
    match event {
        AgentRunEvent::Started { run_id: event_run_id, .. } => event_run_id == run_id,
        AgentRunEvent::OutputChunk { run_id: event_run_id, .. } => event_run_id == run_id,
        AgentRunEvent::Metadata { run_id: event_run_id, .. } => event_run_id == run_id,
        AgentRunEvent::Error { run_id: event_run_id, .. } => event_run_id == run_id,
        AgentRunEvent::Finished { run_id: event_run_id, .. } => event_run_id == run_id,
        AgentRunEvent::ToolCall { run_id: event_run_id, .. } => event_run_id == run_id,
        AgentRunEvent::ToolResult { run_id: event_run_id, .. } => event_run_id == run_id,
        AgentRunEvent::Artifact { run_id: event_run_id, .. } => event_run_id == run_id,
        AgentRunEvent::Thinking { run_id: event_run_id, .. } => event_run_id == run_id,
    }
}

pub fn ensure_safe_run_id(run_id: &str) -> Result<()> {
    if run_id.trim().is_empty() {
        return Err(anyhow!("run_id is required"));
    }
    if run_id.contains('/') || run_id.contains('\\') || run_id.contains("..") {
        return Err(anyhow!("invalid run_id: {run_id}"));
    }
    Ok(())
}

pub fn run_dir(project_root: &str, run_id: &RunId, base_override: Option<&str>) -> PathBuf {
    let base = base_override.map(PathBuf::from).unwrap_or_else(|| {
        scoped_ao_root(Path::new(project_root)).unwrap_or_else(|| Path::new(project_root).join(".animus")).join("runs")
    });
    base.join(&run_id.0)
}

pub fn persist_run_event(run_dir: &Path, event: &AgentRunEvent) -> Result<()> {
    let event_path = run_dir.join("events.jsonl");
    let line = serde_json::to_string(event)?;
    append_line(&event_path, &line)?;

    if let AgentRunEvent::OutputChunk { stream_type, text, .. } = event {
        persist_json_output(run_dir, *stream_type, text)?;
    }

    Ok(())
}

fn persist_json_output(run_dir: &Path, stream_type: OutputStreamType, text: &str) -> Result<()> {
    let path = run_dir.join("json-output.jsonl");
    for (raw, payload) in collect_json_payload_lines(text) {
        let timestamp_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
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

pub fn collect_json_payload_lines(text: &str) -> Vec<(String, Value)> {
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

/// Scan `text` for every balanced `{...}` span and return those that parse as
/// JSON objects, in document order. Unlike [`collect_json_payload_lines`] this
/// tolerates prose around the JSON, pretty-printed / multi-line objects, and
/// objects embedded mid-line — the shapes reasoning + tool-calling models emit
/// (a chatty preamble or tool-call noise, then the real result object). Brace
/// counting respects JSON string literals so `{` / `}` inside string values
/// (e.g. song lyrics) don't throw off the span.
pub fn collect_balanced_json_objects(text: &str) -> Vec<Value> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'{' {
            if let Some(end) = balanced_object_end(bytes, i) {
                if let Ok(value) = serde_json::from_str::<Value>(&text[i..=end]) {
                    if value.is_object() {
                        out.push(value);
                    }
                }
                i = end + 1;
                continue;
            }
        }
        i += 1;
    }
    out
}

/// Index of the `}` that closes the object opened at `start`
/// (`bytes[start] == b'{'`), or `None` if unbalanced. Skips braces inside JSON
/// string literals (honoring `\` escapes).
fn balanced_object_end(bytes: &[u8], start: usize) -> Option<usize> {
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for (offset, &c) in bytes.iter().enumerate().skip(start) {
        if in_string {
            if escaped {
                escaped = false;
            } else if c == b'\\' {
                escaped = true;
            } else if c == b'"' {
                in_string = false;
            }
            continue;
        }
        match c {
            b'"' => in_string = true,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(offset);
                }
            }
            _ => {}
        }
    }
    None
}

/// Extract the authoritative result object from a model's full output `text`:
/// the LAST balanced JSON object whose `kind` equals `expected_kind` and that
/// carries every field in `required_fields` as a present, non-empty value.
///
/// Reasoning + tool-calling models routinely emit prose, a planning stub, or
/// tool-call noise before the real answer, with the real object last. Selecting
/// the last *complete* match (not the first JSON line) is what makes capture
/// robust against those shapes.
pub fn extract_result_payload(text: &str, expected_kind: &str, required_fields: &[&str]) -> Option<Value> {
    collect_balanced_json_objects(text)
        .into_iter()
        .rfind(|value| {
            value.get("kind").and_then(Value::as_str) == Some(expected_kind)
                && required_fields.iter().all(|field| field_is_present(value, field))
        })
}

fn field_is_present(value: &Value, field: &str) -> bool {
    match value.get(field) {
        None | Some(Value::Null) => false,
        Some(Value::String(s)) => !s.trim().is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
        Some(_) => true,
    }
}

pub fn append_line(path: &Path, line: &str) -> Result<()> {
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
    use protocol::{RunId, Timestamp};
    use uuid::Uuid;

    fn temp_run_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ao-ipc-test-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    #[test]
    fn persist_run_event_writes_events_jsonl() {
        let run_dir = temp_run_dir();
        let run_id = RunId("run-persist-001".to_string());

        persist_run_event(&run_dir, &AgentRunEvent::Started { run_id: run_id.clone(), timestamp: Timestamp::now() })
            .expect("persist started");
        persist_run_event(
            &run_dir,
            &AgentRunEvent::OutputChunk {
                run_id: run_id.clone(),
                stream_type: OutputStreamType::Stdout,
                text: "hello\n".to_string(),
            },
        )
        .expect("persist output chunk");
        persist_run_event(&run_dir, &AgentRunEvent::Finished { run_id, exit_code: Some(0), duration_ms: 100 })
            .expect("persist finished");

        let events_path = run_dir.join("events.jsonl");
        assert!(events_path.exists());
        let contents = std::fs::read_to_string(&events_path).expect("read events");
        let lines: Vec<&str> = contents.lines().collect();
        assert_eq!(lines.len(), 3);
        assert!(lines[0].contains("\"kind\":\"started\""));
        assert!(lines[1].contains("\"kind\":\"output_chunk\""));
        assert!(lines[2].contains("\"kind\":\"finished\""));

        let _ = std::fs::remove_dir_all(&run_dir);
    }

    #[test]
    fn collect_json_payload_lines_skips_plain_text() {
        let text = "plain text\n{\"key\":\"value\"}\n[1,2,3]\n\"just a string\"\n42\n";
        let pairs = collect_json_payload_lines(text);
        assert_eq!(pairs.len(), 2);
        assert!(pairs[0].0.contains("key"));
        assert!(pairs[1].0.contains('['));
    }

    #[test]
    fn extract_result_payload_pulls_json_after_a_prose_preamble() {
        // The exact shape a reasoning model emits: explanation, then the object.
        let text = "The profile was empty so I wrote a generic song.\n\
                    {\"kind\":\"lyrics_result\",\"lyrics\":\"la la la\",\"title\":\"Your Song\"}";
        let got = extract_result_payload(text, "lyrics_result", &["lyrics", "title"]).unwrap();
        assert_eq!(got.get("title").and_then(Value::as_str), Some("Your Song"));
    }

    #[test]
    fn extract_result_payload_handles_pretty_printed_multiline_json() {
        let text = "Here you go:\n{\n  \"kind\": \"lyrics_result\",\n  \"lyrics\": \"line one\\nline two\",\n  \"title\": \"Mia\"\n}\nDone!";
        let got = extract_result_payload(text, "lyrics_result", &["lyrics", "title"]).unwrap();
        assert_eq!(got.get("lyrics").and_then(Value::as_str), Some("line one\nline two"));
    }

    #[test]
    fn extract_result_payload_prefers_last_valid_over_earlier_stub() {
        // An early planning stub (missing fields) must lose to the final object.
        let text = "Plan: {\"kind\":\"lyrics_result\"}\n\
                    ...thinking...\n\
                    {\"kind\":\"lyrics_result\",\"lyrics\":\"real lyrics\",\"title\":\"Real\"}";
        let got = extract_result_payload(text, "lyrics_result", &["lyrics", "title"]).unwrap();
        assert_eq!(got.get("title").and_then(Value::as_str), Some("Real"));
    }

    #[test]
    fn extract_result_payload_survives_braces_inside_lyrics_and_tool_noise() {
        let text = "<|tool_call|>functions.shell {\"command\":\"find /app\"}<|end|>\n\
                    {\"kind\":\"lyrics_result\",\"lyrics\":\"use {happy} {braces}\",\"title\":\"Braces\"}";
        let got = extract_result_payload(text, "lyrics_result", &["lyrics", "title"]).unwrap();
        assert_eq!(got.get("lyrics").and_then(Value::as_str), Some("use {happy} {braces}"));
    }

    #[test]
    fn extract_result_payload_rejects_missing_required_fields() {
        let text = "{\"kind\":\"lyrics_result\",\"lyrics\":\"\",\"title\":\"   \"}";
        // Empty strings count as absent — a blank result is not a valid capture.
        assert!(extract_result_payload(text, "lyrics_result", &["lyrics", "title"]).is_none());
    }

    #[test]
    fn run_dir_uses_scoped_state_root() {
        let project_root = std::env::temp_dir().join("ao-run-dir-test");
        let run_id = RunId("run-dir-abc".to_string());
        let dir = run_dir(project_root.to_str().unwrap(), &run_id, None);
        assert!(dir.ends_with("run-dir-abc"));
    }
}
