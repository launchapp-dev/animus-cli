use anyhow::Result;
use serde_json::Value;
use std::path::{Path, PathBuf};
use tokio_util::sync::CancellationToken;

use crate::api::client::ApiClient;
use crate::api::types::*;
use crate::tools::{executor, mcp_client};

use super::context;
use super::output::OutputFormatter;

const SCHEMA_RETRY_LIMIT: usize = 3;

fn config_dir() -> PathBuf {
    let dir = std::env::var("AO_CONFIG_DIR")
        .or_else(|_| std::env::var("HOME").map(|h| format!("{}/.ao", h)))
        .unwrap_or_else(|_| ".ao".to_string());
    PathBuf::from(dir)
}

fn session_file_path_in(base: &Path, session_id: &str) -> PathBuf {
    base.join("sessions").join(format!("{}.json", session_id))
}

fn load_session_messages_from(base: &Path, session_id: &str) -> Vec<ChatMessage> {
    let path = session_file_path_in(base, session_id);
    if !path.exists() {
        return Vec::new();
    }
    match std::fs::read_to_string(&path) {
        Ok(data) => serde_json::from_str(&data).unwrap_or_default(),
        Err(_) => Vec::new(),
    }
}

fn save_session_messages_to(base: &Path, session_id: &str, messages: &[ChatMessage]) -> Result<()> {
    let path = session_file_path_in(base, session_id);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let data = serde_json::to_string_pretty(messages)?;
    std::fs::write(&path, data)?;
    Ok(())
}

fn build_response_format(schema: &Value) -> ResponseFormat {
    ResponseFormat {
        type_: "json_schema".to_string(),
        json_schema: Some(JsonSchemaSpec {
            name: "phase_output".to_string(),
            strict: false,
            schema: schema.clone(),
        }),
    }
}

fn synthesize_fallback(model: &str, summary: &str, confidence: f64) -> Value {
    serde_json::json!({
        "kind": "implementation_result",
        "commit_message": format!("Implementation by {}", model),
        "phase_decision": {
            "kind": "phase_decision",
            "verdict": "rework",
            "confidence": confidence,
            "risk": "high",
            "reason": format!("Agent did not produce valid structured output. Summary: {}", summary)
        }
    })
}

#[allow(clippy::too_many_arguments)]
pub async fn run_agent_loop(
    client: &ApiClient,
    model: &str,
    system_prompt: &str,
    user_prompt: &str,
    tools: &[ToolDefinition],
    working_dir: &Path,
    max_turns: usize,
    output: &mut OutputFormatter,
    response_schema: Option<&Value>,
    mcp_clients: &[mcp_client::McpClient],
    session_id: Option<&str>,
    use_response_format: bool,
    cancel_token: CancellationToken,
    context_limit: usize,
    max_tokens: usize,
) -> Result<()> {
    let mut messages: Vec<ChatMessage> = Vec::new();

    if let Some(sid) = session_id {
        let prior = load_session_messages_from(&config_dir(), sid);
        if !prior.is_empty() {
            eprintln!("[oai-runner] Resuming session {} ({} prior messages)", sid, prior.len());
            messages.extend(prior);
        }
    }

    if messages.is_empty() && !system_prompt.is_empty() {
        messages.push(ChatMessage {
            role: "system".to_string(),
            content: Some(system_prompt.to_string()),
            tool_calls: None,
            tool_call_id: None,
        });
    }

    messages.push(ChatMessage {
        role: "user".to_string(),
        content: Some(user_prompt.to_string()),
        tool_calls: None,
        tool_call_id: None,
    });

    for turn in 0..max_turns {
        if cancel_token.is_cancelled() {
            eprintln!("[oai-runner] Cancelled by signal");
            if let Some(sid) = session_id {
                if let Err(e) = save_session_messages_to(&config_dir(), sid, &messages) {
                    eprintln!("[oai-runner] Warning: failed to save session on cancel {}: {}", sid, e);
                }
            }
            output.emit_session_summary();
            anyhow::bail!("Cancelled by shutdown signal");
        }

        context::truncate_to_fit(&mut messages, context_limit, max_tokens);

        let format = if use_response_format {
            response_schema.map(build_response_format)
        } else {
            None
        };

        let request = ChatRequest {
            model: model.to_string(),
            messages: messages.clone(),
            stream: true,
            tools: Some(tools.to_vec()),
            max_tokens: Some(max_tokens as u32),
            response_format: format,
            stream_options: Some(StreamOptions { include_usage: true }),
        };

        let (assistant_msg, usage) = client
            .stream_chat(&request, &mut |chunk| {
                output.text_chunk(chunk);
            })
            .await?;

        if let Some(u) = &usage {
            output.metadata(u.prompt_tokens, u.completion_tokens);
        }

        let has_tool_calls = assistant_msg.tool_calls.as_ref().is_some_and(|tc| !tc.is_empty());

        messages.push(assistant_msg.clone());

        if !has_tool_calls {
            output.flush_result();
            let content = assistant_msg.content.as_deref().unwrap_or("");
            let mut schema_ok = true;
            if let Some(schema) = response_schema {
                if let Err(errors) = validate_output_against_schema(content, schema) {
                    let system_msg = messages.iter().find(|m| m.role == "system").cloned();
                    let corrected =
                        retry_schema_validation(client, model, system_msg.as_ref(), &mut messages, schema, &errors, output).await;
                    if !corrected {
                        eprintln!("Warning: schema validation failed after {} retries, synthesizing fallback result", SCHEMA_RETRY_LIMIT);
                        schema_ok = false;
                    }
                }
            }
            if !schema_ok {
                let summary = content.chars().take(200).collect::<String>();
                let fallback = synthesize_fallback(model, &summary, 0.4);
                let fallback_str = serde_json::to_string(&fallback).unwrap_or_default();
                output.text_chunk(&fallback_str);
                output.flush_result();
            }
            if let Some(sid) = session_id {
                if let Err(e) = save_session_messages_to(&config_dir(), sid, &messages) {
                    eprintln!("[oai-runner] Warning: failed to save session {}: {}", sid, e);
                }
            }
            output.emit_session_summary();
            output.newline();
            return Ok(());
        }

        let tool_calls = assistant_msg.tool_calls.as_ref().unwrap();

        for tc in tool_calls {
            if cancel_token.is_cancelled() {
                eprintln!("[oai-runner] Cancelled between tool calls");
                break;
            }

            let args: serde_json::Value =
                serde_json::from_str(&tc.function.arguments).unwrap_or(serde_json::Value::Null);

            output.tool_call(&tc.function.name, &args);

            let result = if let Some(mcp) = mcp_client::find_client_for_tool(mcp_clients, &tc.function.name) {
                match mcp_client::call_tool(mcp, &tc.function.name, &tc.function.arguments).await {
                    Ok(r) => {
                        output.tool_result(&tc.function.name, &r);
                        r
                    }
                    Err(e) => {
                        let err_msg = e.to_string();
                        output.tool_error(&tc.function.name, &err_msg);
                        format!("Error: {}", err_msg)
                    }
                }
            } else {
                match executor::execute_tool(&tc.function.name, &tc.function.arguments, working_dir).await {
                    Ok(r) => {
                        output.tool_result(&tc.function.name, &r);
                        r
                    }
                    Err(e) => {
                        let err_msg = e.to_string();
                        output.tool_error(&tc.function.name, &err_msg);
                        format!("Error: {}", err_msg)
                    }
                }
            };

            messages.push(ChatMessage {
                role: "tool".to_string(),
                content: Some(result),
                tool_calls: None,
                tool_call_id: Some(tc.id.clone()),
            });
        }

        if turn == max_turns - 1 {
            eprintln!("Warning: reached maximum turns ({}). Stopping.", max_turns);
        }
    }

    if let Some(sid) = session_id {
        if let Err(e) = save_session_messages_to(&config_dir(), sid, &messages) {
            eprintln!("[oai-runner] Warning: failed to save session {}: {}", sid, e);
        }
    }
    output.flush_result();
    if response_schema.is_some() {
        eprintln!("[oai-runner] Max turns reached, synthesizing fallback result");
        let fallback = synthesize_fallback(model, "Agent reached maximum turns. Work may be partially complete.", 0.3);
        let fallback_str = serde_json::to_string(&fallback).unwrap_or_default();
        output.text_chunk(&fallback_str);
        output.flush_result();
    }
    output.emit_session_summary();
    output.newline();
    Ok(())
}

async fn retry_schema_validation(
    client: &ApiClient,
    model: &str,
    system_msg: Option<&ChatMessage>,
    messages: &mut Vec<ChatMessage>,
    schema: &Value,
    initial_errors: &str,
    output: &mut OutputFormatter,
) -> bool {
    let mut last_errors = initial_errors.to_string();

    let last_assistant_content = messages
        .iter()
        .rev()
        .find(|m| m.role == "assistant")
        .and_then(|m| m.content.clone())
        .unwrap_or_default();

    for attempt in 1..=SCHEMA_RETRY_LIMIT {
        eprintln!("Schema validation failed (attempt {}/{}): {}", attempt, SCHEMA_RETRY_LIMIT, last_errors);

        let correction = format!(
            "Your last response did not match the required output JSON schema. Errors:\n{}\n\n\
             The required schema is:\n{}\n\n\
             Please respond with ONLY a valid JSON object matching the schema above. No markdown, no explanation, just the raw JSON.",
            last_errors,
            serde_json::to_string_pretty(schema).unwrap_or_default()
        );

        let mut retry_messages: Vec<ChatMessage> = Vec::new();
        if let Some(sys) = system_msg {
            retry_messages.push(sys.clone());
        }
        retry_messages.push(ChatMessage {
            role: "assistant".to_string(),
            content: Some(last_assistant_content.clone()),
            tool_calls: None,
            tool_call_id: None,
        });
        retry_messages.push(ChatMessage {
            role: "user".to_string(),
            content: Some(correction.clone()),
            tool_calls: None,
            tool_call_id: None,
        });

        let retry_request = ChatRequest {
            model: model.to_string(),
            messages: retry_messages,
            stream: true,
            tools: None,
            max_tokens: Some(4096),
            response_format: Some(build_response_format(schema)),
            stream_options: Some(StreamOptions { include_usage: true }),
        };

        let retry_result = client
            .stream_chat(&retry_request, &mut |chunk| {
                output.text_chunk(chunk);
            })
            .await;

        let (retry_msg, usage) = match retry_result {
            Ok(r) => r,
            Err(_) => return false,
        };

        if let Some(u) = &usage {
            output.metadata(u.prompt_tokens, u.completion_tokens);
        }

        let content = retry_msg.content.clone().unwrap_or_default();
        messages.push(ChatMessage {
            role: "user".to_string(),
            content: Some(correction),
            tool_calls: None,
            tool_call_id: None,
        });
        messages.push(retry_msg);

        match validate_output_against_schema(&content, schema) {
            Ok(()) => return true,
            Err(errors) => last_errors = errors,
        }
    }

    false
}

fn validate_output_against_schema(content: &str, schema: &Value) -> std::result::Result<(), String> {
    let parsed = extract_json_from_content(content)
        .ok_or_else(|| "Response does not contain valid JSON. Expected a JSON object.".to_string())?;

    let validator = jsonschema::validator_for(schema).map_err(|e| format!("Invalid schema: {}", e))?;

    let errors: Vec<String> = validator.iter_errors(&parsed).map(|e| {
        let path = e.instance_path().to_string();
        if path.is_empty() {
            format!("{}", e)
        } else {
            format!("at '{}': {}", path, e)
        }
    }).collect();

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}

fn extract_json_from_content(content: &str) -> Option<Value> {
    let trimmed = content.trim();
    if let Ok(v) = serde_json::from_str::<Value>(trimmed) {
        return Some(v);
    }

    if let Some(start) = trimmed.find("```json") {
        let after = &trimmed[start + 7..];
        if let Some(end) = after.find("```") {
            if let Ok(v) = serde_json::from_str::<Value>(after[..end].trim()) {
                return Some(v);
            }
        }
    }

    if let Some(start) = trimmed.find("```") {
        let after = &trimmed[start + 3..];
        if let Some(end) = after.find("```") {
            if let Ok(v) = serde_json::from_str::<Value>(after[..end].trim()) {
                return Some(v);
            }
        }
    }

    for line in trimmed.lines() {
        let line = line.trim();
        if line.starts_with('{') {
            if let Ok(v) = serde_json::from_str::<Value>(line) {
                return Some(v);
            }
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn validates_valid_json_against_schema() {
        let schema = json!({
            "type": "object",
            "required": ["kind", "verdict"],
            "properties": {
                "kind": { "const": "phase_decision" },
                "verdict": { "type": "string", "enum": ["advance", "rework", "fail"] }
            }
        });
        let content = r#"{"kind":"phase_decision","verdict":"advance","reason":"all good"}"#;
        assert!(validate_output_against_schema(content, &schema).is_ok());
    }

    #[test]
    fn rejects_missing_required_field() {
        let schema = json!({
            "type": "object",
            "required": ["kind", "verdict"],
            "properties": {
                "kind": { "const": "phase_decision" },
                "verdict": { "type": "string" }
            }
        });
        let content = r#"{"kind":"phase_decision"}"#;
        let err = validate_output_against_schema(content, &schema).unwrap_err();
        assert!(err.contains("verdict"), "Error should mention 'verdict': {}", err);
    }

    #[test]
    fn rejects_wrong_type() {
        let schema = json!({
            "type": "object",
            "required": ["confidence"],
            "properties": {
                "confidence": { "type": "number" }
            }
        });
        let content = r#"{"confidence":"high"}"#;
        let err = validate_output_against_schema(content, &schema).unwrap_err();
        assert!(!err.is_empty());
    }

    #[test]
    fn rejects_wrong_const() {
        let schema = json!({
            "type": "object",
            "required": ["kind"],
            "properties": {
                "kind": { "const": "phase_decision" }
            }
        });
        let content = r#"{"kind":"something_else"}"#;
        let err = validate_output_against_schema(content, &schema).unwrap_err();
        assert!(!err.is_empty());
    }

    #[test]
    fn rejects_invalid_enum_value() {
        let schema = json!({
            "type": "object",
            "required": ["verdict"],
            "properties": {
                "verdict": { "type": "string", "enum": ["advance", "rework", "fail"] }
            }
        });
        let content = r#"{"verdict":"maybe"}"#;
        let err = validate_output_against_schema(content, &schema).unwrap_err();
        assert!(!err.is_empty());
    }

    #[test]
    fn extracts_json_from_markdown_code_block() {
        let schema = json!({
            "type": "object",
            "required": ["kind"],
            "properties": {
                "kind": { "const": "phase_decision" }
            }
        });
        let content = "Here is my assessment:\n```json\n{\"kind\":\"phase_decision\"}\n```\n";
        assert!(validate_output_against_schema(content, &schema).is_ok());
    }

    #[test]
    fn extracts_json_from_inline_line() {
        let schema = json!({
            "type": "object",
            "required": ["kind"],
            "properties": {
                "kind": { "const": "phase_decision" }
            }
        });
        let content = "My analysis is complete.\n{\"kind\":\"phase_decision\"}\nDone.";
        assert!(validate_output_against_schema(content, &schema).is_ok());
    }

    #[test]
    fn rejects_non_json_content() {
        let schema = json!({
            "type": "object",
            "required": ["kind"],
            "properties": {}
        });
        let content = "This is just plain text with no JSON at all.";
        let err = validate_output_against_schema(content, &schema).unwrap_err();
        assert!(err.contains("does not contain valid JSON"));
    }

    #[test]
    fn validates_nested_objects() {
        let schema = json!({
            "type": "object",
            "required": ["phase_decision"],
            "properties": {
                "phase_decision": {
                    "type": "object",
                    "required": ["verdict", "confidence"],
                    "properties": {
                        "verdict": { "type": "string", "enum": ["advance", "rework", "fail"] },
                        "confidence": { "type": "number", "minimum": 0, "maximum": 1 }
                    }
                }
            }
        });
        let valid = r#"{"phase_decision":{"verdict":"advance","confidence":0.9}}"#;
        assert!(validate_output_against_schema(valid, &schema).is_ok());

        let invalid = r#"{"phase_decision":{"verdict":"maybe","confidence":0.9}}"#;
        assert!(validate_output_against_schema(invalid, &schema).is_err());

        let missing = r#"{"phase_decision":{"verdict":"advance"}}"#;
        assert!(validate_output_against_schema(missing, &schema).is_err());
    }

    #[test]
    fn validates_one_of() {
        let schema = json!({
            "oneOf": [
                {
                    "type": "object",
                    "required": ["kind"],
                    "properties": { "kind": { "const": "success" } }
                },
                {
                    "type": "object",
                    "required": ["kind"],
                    "properties": { "kind": { "const": "failure" } }
                }
            ]
        });
        let valid = r#"{"kind":"success"}"#;
        assert!(validate_output_against_schema(valid, &schema).is_ok());

        let invalid = r#"{"kind":"other"}"#;
        assert!(validate_output_against_schema(invalid, &schema).is_err());
    }

    #[test]
    fn validates_array_min_items() {
        let schema = json!({
            "type": "object",
            "required": ["items"],
            "properties": {
                "items": { "type": "array", "minItems": 1 }
            }
        });
        let valid = r#"{"items":["a"]}"#;
        assert!(validate_output_against_schema(valid, &schema).is_ok());

        let invalid = r#"{"items":[]}"#;
        assert!(validate_output_against_schema(invalid, &schema).is_err());
    }

    #[test]
    fn validates_string_pattern() {
        let schema = json!({
            "type": "object",
            "required": ["version"],
            "properties": {
                "version": { "type": "string", "pattern": "^\\d+\\.\\d+\\.\\d+$" }
            }
        });
        let valid = r#"{"version":"1.2.3"}"#;
        assert!(validate_output_against_schema(valid, &schema).is_ok());

        let invalid = r#"{"version":"not-a-version"}"#;
        assert!(validate_output_against_schema(invalid, &schema).is_err());
    }

    #[test]
    fn fallback_uses_rework_verdict() {
        let fallback = synthesize_fallback("test-model", "test summary", 0.4);
        let decision = &fallback["phase_decision"];
        assert_eq!(decision["verdict"], "rework");
        assert_eq!(decision["confidence"], 0.4);
        assert_eq!(decision["risk"], "high");
    }

    #[test]
    fn session_save_and_load_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path();

        let sid = "test-session-round-trip";
        let messages = vec![
            ChatMessage {
                role: "system".to_string(),
                content: Some("You are helpful.".to_string()),
                tool_calls: None,
                tool_call_id: None,
            },
            ChatMessage {
                role: "user".to_string(),
                content: Some("Hello".to_string()),
                tool_calls: None,
                tool_call_id: None,
            },
            ChatMessage {
                role: "assistant".to_string(),
                content: Some("Hi there!".to_string()),
                tool_calls: None,
                tool_call_id: None,
            },
        ];

        save_session_messages_to(base, sid, &messages).unwrap();
        let loaded = load_session_messages_from(base, sid);
        assert_eq!(loaded.len(), 3);
        assert_eq!(loaded[0].role, "system");
        assert_eq!(loaded[1].content.as_deref(), Some("Hello"));
        assert_eq!(loaded[2].content.as_deref(), Some("Hi there!"));
    }

    #[test]
    fn load_nonexistent_session_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let loaded = load_session_messages_from(dir.path(), "nonexistent-session-id");
        assert!(loaded.is_empty());
    }
}
