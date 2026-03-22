use anyhow::Result;
use serde_json::Value;
use std::path::{Path, PathBuf};
use tokio_util::sync::CancellationToken;

use crate::api::client::ApiClient;
use crate::api::types::*;
use crate::config::StructuredOutputSupport;
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

/// Maximum number of prior session messages to load on resume.
/// When sessions grow very large (e.g., 300+ messages), loading all prior
/// messages can overwhelm the context window before truncation has a chance
/// to trim them, leading to repeated "[oai-runner] Context management: truncated"
/// cycles that stall the agent. Capping at the most recent messages preserves
/// enough context for continuity while keeping the initial context within bounds.
const MAX_RESUME_MESSAGES: usize = 50;

fn load_session_messages_from(base: &Path, session_id: &str) -> Vec<ChatMessage> {
    let path = session_file_path_in(base, session_id);
    if !path.exists() {
        return Vec::new();
    }
    let mut messages: Vec<ChatMessage> = match std::fs::read_to_string(&path) {
        Ok(data) => serde_json::from_str(&data).unwrap_or_default(),
        Err(_) => Vec::new(),
    };

    if messages.len() > MAX_RESUME_MESSAGES {
        let original_len = messages.len();
        // Keep the system message (if present) and the most recent messages.
        let system_idx = messages.iter().position(|m| m.role == "system");
        let keep_from_start = system_idx.map_or(0, |idx| idx + 1);
        let keep_count = MAX_RESUME_MESSAGES.saturating_sub(keep_from_start);
        let trim_start = keep_from_start;
        let trim_end = messages.len().saturating_sub(keep_count);

        if trim_end > trim_start {
            let dropped = trim_end - trim_start;
            messages.drain(trim_start..trim_end);
            eprintln!(
                "[oai-runner] Capped resumed session from {} to {} messages (dropped {} oldest non-system messages)",
                original_len,
                messages.len(),
                dropped
            );
        }
    }

    messages
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

fn build_json_schema_format(schema: &Value) -> ResponseFormat {
    let mut strict_schema = schema.clone();
    if let Some(obj) = strict_schema.as_object_mut() {
        obj.entry("additionalProperties").or_insert(serde_json::Value::Bool(false));
    }
    ResponseFormat {
        type_: "json_schema".to_string(),
        json_schema: Some(JsonSchemaSpec { name: "phase_output".to_string(), strict: true, schema: strict_schema }),
    }
}

fn build_json_object_format() -> ResponseFormat {
    ResponseFormat { type_: "json_object".to_string(), json_schema: None }
}

fn build_schema_injection(schema: &Value) -> String {
    format!(
        "\n\nIMPORTANT: Your final response MUST be a single valid JSON object matching this exact schema. \
         Do not wrap it in markdown. Do not add explanation. Output ONLY the JSON.\n\nRequired JSON Schema:\n{}",
        serde_json::to_string_pretty(schema).unwrap_or_default()
    )
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
    structured_output: Option<StructuredOutputSupport>,
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

    let has_final_message_tool = response_schema.is_some();

    let needs_schema_in_prompt = structured_output == Some(StructuredOutputSupport::JsonObjectOnly)
        && response_schema.is_some()
        && !has_final_message_tool;

    if messages.is_empty() {
        let mut sys = system_prompt.to_string();
        if needs_schema_in_prompt {
            sys.push_str(&build_schema_injection(response_schema.unwrap()));
        }
        if !sys.is_empty() {
            messages.push(ChatMessage {
                reasoning_content: None,
                role: "system".to_string(),
                content: Some(sys),
                tool_calls: None,
                tool_call_id: None,
            });
        }
    }

    messages.push(ChatMessage {
        reasoning_content: None,
        role: "user".to_string(),
        content: Some(user_prompt.to_string()),
        tool_calls: None,
        tool_call_id: None,
    });

    let mut tools_with_final: Vec<ToolDefinition> = tools.to_vec();
    if has_final_message_tool {
        let schema_desc = response_schema.and_then(|s| serde_json::to_string(s).ok()).unwrap_or_default();
        tools_with_final.push(ToolDefinition {
            type_: "function".to_string(),
            function: FunctionSchema {
                name: "final_message_json".to_string(),
                description: format!(
                    "Call this tool to submit your final structured JSON result when your task is complete. \
                     The message MUST be valid JSON matching this schema: {}",
                    schema_desc
                ),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "message": {
                            "type": "string",
                            "description": "Your final result as a JSON string matching the required schema."
                        }
                    },
                    "required": ["message"],
                    "additionalProperties": false
                }),
            },
        });
    }
    let effective_tools = if has_final_message_tool { &tools_with_final } else { tools };

    let needs_tool_name_sanitization = model.contains("kimi");
    let sanitized_tools: Vec<ToolDefinition> = if needs_tool_name_sanitization {
        effective_tools
            .iter()
            .map(|t| {
                let mut t = t.clone();
                t.function.name = t.function.name.replace('.', "_");
                t
            })
            .collect()
    } else {
        effective_tools.to_vec()
    };
    let api_tools = if needs_tool_name_sanitization { &sanitized_tools } else { effective_tools };

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

        let format = if has_final_message_tool {
            None // Don't send response_format when using final_message_json tool
        } else {
            match structured_output {
                Some(StructuredOutputSupport::JsonSchema) => response_schema.map(build_json_schema_format),
                Some(StructuredOutputSupport::JsonObjectOnly) if response_schema.is_some() => {
                    Some(build_json_object_format())
                }
                _ => None,
            }
        };

        let request = ChatRequest {
            model: model.to_string(),
            messages: messages.clone(),
            stream: true,
            tools: Some(api_tools.to_vec()),
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
            output.metadata(u);
        }

        let has_tool_calls = assistant_msg.tool_calls.as_ref().is_some_and(|tc| !tc.is_empty());

        messages.push(assistant_msg.clone());

        if let Some(sid) = session_id {
            let _ = save_session_messages_to(&config_dir(), sid, &messages);
        }

        if !has_tool_calls {
            // If final_message_json tool is available but model didn't call it, prompt to call it
            if has_final_message_tool && turn < max_turns - 1 {
                messages.push(ChatMessage {
                    reasoning_content: None,
                    role: "user".to_string(),
                    content: Some(
                        "You must call the final_message_json tool to submit your result. \
                         Do not respond with plain text — call the final_message_json tool now."
                            .to_string(),
                    ),
                    tool_calls: None,
                    tool_call_id: None,
                });
                continue;
            }
            output.flush_result();
            let content = assistant_msg.content.as_deref().unwrap_or("");
            let mut schema_ok = true;
            if let Some(schema) = response_schema {
                if let Err(errors) = validate_output_against_schema(content, schema) {
                    let system_msg = messages.iter().find(|m| m.role == "system").cloned();
                    let corrected = retry_schema_validation(
                        client,
                        model,
                        system_msg.as_ref(),
                        &mut messages,
                        schema,
                        &errors,
                        output,
                        structured_output,
                    )
                    .await;
                    if !corrected {
                        eprintln!(
                            "Warning: schema validation failed after {} retries, synthesizing fallback result",
                            SCHEMA_RETRY_LIMIT
                        );
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

        // Check if any tool call is final_message_json — if so, emit and stop
        if has_final_message_tool {
            let final_tc = tool_calls.iter().find(|tc| {
                let name = if needs_tool_name_sanitization {
                    tc.function.name.replace('_', ".")
                } else {
                    tc.function.name.clone()
                };
                name == "final_message_json" || name == "final.message.json"
            });
            if let Some(tc) = final_tc {
                let args: serde_json::Value =
                    serde_json::from_str(&tc.function.arguments).unwrap_or(serde_json::Value::Null);
                let message = args.get("message").and_then(|v| v.as_str()).unwrap_or("");
                output.text_chunk(message);
                output.flush_result();
                if let Some(sid) = session_id {
                    let _ = save_session_messages_to(&config_dir(), sid, &messages);
                }
                output.emit_session_summary();
                output.newline();
                return Ok(());
            }
        }

        for tc in tool_calls {
            if cancel_token.is_cancelled() {
                eprintln!("[oai-runner] Cancelled between tool calls");
                break;
            }

            let tool_name = if needs_tool_name_sanitization {
                tc.function.name.replace('_', ".")
            } else {
                tc.function.name.clone()
            };

            let args: serde_json::Value =
                serde_json::from_str(&tc.function.arguments).unwrap_or(serde_json::Value::Null);

            output.tool_call(&tool_name, &args);

            let result = if let Some(mcp) = mcp_client::find_client_for_tool(mcp_clients, &tool_name) {
                match mcp_client::call_tool(mcp, &tool_name, &tc.function.arguments).await {
                    Ok(r) => {
                        output.tool_result(&tool_name, &r);
                        r
                    }
                    Err(e) => {
                        let err_msg = e.to_string();
                        output.tool_error(&tool_name, &err_msg);
                        format!("Error: {}", err_msg)
                    }
                }
            } else {
                match executor::execute_tool(&tool_name, &tc.function.arguments, working_dir).await {
                    Ok(r) => {
                        output.tool_result(&tool_name, &r);
                        r
                    }
                    Err(e) => {
                        let err_msg = e.to_string();
                        output.tool_error(&tool_name, &err_msg);
                        format!("Error: {}", err_msg)
                    }
                }
            };

            messages.push(ChatMessage {
                reasoning_content: None,
                role: "tool".to_string(),
                content: Some(result),
                tool_calls: None,
                tool_call_id: Some(tc.id.clone()),
            });

            if let Some(sid) = session_id {
                let _ = save_session_messages_to(&config_dir(), sid, &messages);
            }
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
    structured_output: Option<StructuredOutputSupport>,
) -> bool {
    let mut last_errors = initial_errors.to_string();

    let last_assistant_content =
        messages.iter().rev().find(|m| m.role == "assistant").and_then(|m| m.content.clone()).unwrap_or_default();

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
            reasoning_content: None,
            role: "assistant".to_string(),
            content: Some(last_assistant_content.clone()),
            tool_calls: None,
            tool_call_id: None,
        });
        retry_messages.push(ChatMessage {
            reasoning_content: None,
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
            response_format: Some(match structured_output {
                Some(StructuredOutputSupport::JsonObjectOnly) | None => build_json_object_format(),
                _ => build_json_schema_format(schema),
            }),
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
            output.metadata(u);
        }

        let content = retry_msg.content.clone().unwrap_or_default();
        messages.push(ChatMessage {
            reasoning_content: None,
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

    let errors: Vec<String> = validator
        .iter_errors(&parsed)
        .map(|e| {
            let path = e.instance_path().to_string();
            if path.is_empty() {
                format!("{}", e)
            } else {
                format!("at '{}': {}", path, e)
            }
        })
        .collect();

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

    // Fallback: find the first '{' and last '}' and try to parse everything in between.
    // This handles multi-line JSON that isn't wrapped in markdown.
    if let (Some(start), Some(end)) = (trimmed.find('{'), trimmed.rfind('}')) {
        if end > start {
            let potential_json = &trimmed[start..=end];
            if let Ok(v) = serde_json::from_str::<Value>(potential_json) {
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
                reasoning_content: None,
                role: "system".to_string(),
                content: Some("You are helpful.".to_string()),
                tool_calls: None,
                tool_call_id: None,
            },
            ChatMessage {
                reasoning_content: None,
                role: "user".to_string(),
                content: Some("Hello".to_string()),
                tool_calls: None,
                tool_call_id: None,
            },
            ChatMessage {
                reasoning_content: None,
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

    #[test]
    fn load_session_caps_large_history_to_max_resume_messages() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path();
        let sid = "test-session-cap";

        // Build a session with system + 80 user/assistant pairs = 161 messages
        let mut messages = vec![ChatMessage {
            reasoning_content: None,
            role: "system".to_string(),
            content: Some("You are helpful.".to_string()),
            tool_calls: None,
            tool_call_id: None,
        }];
        for i in 0..80 {
            messages.push(ChatMessage {
                reasoning_content: None,
                role: "user".to_string(),
                content: Some(format!("Question {}", i)),
                tool_calls: None,
                tool_call_id: None,
            });
            messages.push(ChatMessage {
                reasoning_content: None,
                role: "assistant".to_string(),
                content: Some(format!("Answer {}", i)),
                tool_calls: None,
                tool_call_id: None,
            });
        }

        save_session_messages_to(base, sid, &messages).unwrap();
        let loaded = load_session_messages_from(base, sid);

        // Should be capped: 1 system + 49 most recent messages = 50
        assert_eq!(loaded.len(), MAX_RESUME_MESSAGES);
        assert_eq!(loaded[0].role, "system");
        // The most recent messages should be preserved
        assert_eq!(loaded.last().unwrap().content.as_deref(), Some("Answer 79"));
        // After capping, the first non-system message should be partway through the history
        assert!(loaded[1].content.as_deref().unwrap().contains("55"));
    }

    #[test]
    fn load_session_preserves_small_history_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path();
        let sid = "test-session-small";

        let messages = vec![
            ChatMessage {
                reasoning_content: None,
                role: "system".to_string(),
                content: Some("You are helpful.".to_string()),
                tool_calls: None,
                tool_call_id: None,
            },
            ChatMessage {
                reasoning_content: None,
                role: "user".to_string(),
                content: Some("Hello".to_string()),
                tool_calls: None,
                tool_call_id: None,
            },
        ];

        save_session_messages_to(base, sid, &messages).unwrap();
        let loaded = load_session_messages_from(base, sid);
        assert_eq!(loaded.len(), 2);
    }

    #[test]
    fn load_session_caps_at_exact_boundary() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path();
        let sid = "test-session-boundary";

        // Build exactly MAX_RESUME_MESSAGES messages
        let mut messages = Vec::new();
        for i in 0..MAX_RESUME_MESSAGES {
            messages.push(ChatMessage {
                reasoning_content: None,
                role: "user".to_string(),
                content: Some(format!("Message {}", i)),
                tool_calls: None,
                tool_call_id: None,
            });
        }

        save_session_messages_to(base, sid, &messages).unwrap();
        let loaded = load_session_messages_from(base, sid);
        // Exactly at the boundary — no capping needed
        assert_eq!(loaded.len(), MAX_RESUME_MESSAGES);
    }
}
