use anyhow::Result;
use serde_json::Value;
use std::path::{Path, PathBuf};
use tokio_util::sync::CancellationToken;

use crate::api::client::ApiClient;
use crate::api::types::*;
use crate::config::StructuredOutputSupport;
use crate::tools::{executor, mcp_client};

#[path = "tool_batch.rs"]
mod tool_batch;

use super::context;
use super::output::OutputFormatter;

const SCHEMA_RETRY_LIMIT: usize = 3;
const CONTEXT_PRESSURE_RETRY_LIMIT: usize = 1;
const CONTEXT_PRESSURE_TRUNCATION_RESERVE_BONUS: usize = 1024;

fn config_dir() -> PathBuf {
    let dir = std::env::var("AO_CONFIG_DIR")
        .or_else(|_| std::env::var("HOME").map(|h| format!("{}/.ao", h)))
        .unwrap_or_else(|_| ".ao".to_string());
    PathBuf::from(dir)
}

fn session_file_path_in(base: &Path, session_id: &str) -> PathBuf {
    base.join("sessions").join(format!("{}.json", session_id))
}

fn compaction_history_path_in(base: &Path, session_id: &str) -> PathBuf {
    base.join("sessions").join(format!("{}.compaction.jsonl", session_id))
}

fn append_compaction_history(base: &Path, session_id: &str, transcript: &str) {
    let path = compaction_history_path_in(base, session_id);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let record = serde_json::json!({
        "timestamp": chrono::Utc::now().to_rfc3339(),
        "transcript": transcript,
    });
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(f, "{}", serde_json::to_string(&record).unwrap_or_default());
    }
}

fn load_compaction_history(base: &Path, session_id: &str) -> Vec<String> {
    let path = compaction_history_path_in(base, session_id);
    match std::fs::read_to_string(&path) {
        Ok(data) => data
            .lines()
            .filter_map(|line| {
                serde_json::from_str::<serde_json::Value>(line)
                    .ok()
                    .and_then(|v| v.get("transcript").and_then(|t| t.as_str()).map(String::from))
            })
            .collect(),
        Err(_) => Vec::new(),
    }
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
        let system_idx = messages.iter().position(|m| m.role == "system");
        let keep_from_start = system_idx.map_or(0, |idx| idx + 1);
        let keep_count = MAX_RESUME_MESSAGES.saturating_sub(keep_from_start);
        let trim_start = keep_from_start;
        let mut trim_end = messages.len().saturating_sub(keep_count);

        if trim_end > trim_start && trim_end < messages.len() && messages[trim_end].role == "tool" {
            while trim_end < messages.len() && messages[trim_end].role == "tool" {
                trim_end += 1;
            }
        }

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

fn persist_session_snapshot(session_id: Option<&str>, messages: &[ChatMessage]) {
    if let Some(sid) = session_id {
        if let Err(e) = save_session_messages_to(&config_dir(), sid, messages) {
            eprintln!("[oai-runner] Warning: failed to save session {}: {}", sid, e);
        }
    }
}

fn is_context_pressure_error_message(message: &str) -> bool {
    let message = message.to_lowercase();
    let markers = [
        "context window",
        "maximum context",
        "context length",
        "context limit",
        "prompt too long",
        "prompt is too long",
        "prompt exceeds",
        "input too long",
        "too many tokens",
        "token limit",
        "messages resulted in",
        "exceeds the context",
        "maximum output",
        "max output",
        "max_tokens",
        "output tokens",
        "request entity too large",
        "payload too large",
    ];
    message.contains("api returned 413")
        || message.contains("413 request entity too large")
        || markers.iter().any(|marker| message.contains(marker))
}

fn is_context_pressure_error(error: &anyhow::Error) -> bool {
    is_context_pressure_error_message(&error.to_string())
}

async fn compact_conversation_history(
    client: &ApiClient,
    model: &str,
    messages: &mut Vec<ChatMessage>,
    compaction_history: &mut Vec<String>,
    session_id: Option<&str>,
    label: &str,
) -> bool {
    let Some((compaction_msgs, compact_end)) = context::build_compaction_prompt(messages) else {
        return false;
    };

    if label.is_empty() {
        eprintln!("[oai-runner] Context at capacity, compacting conversation history via LLM");
    } else {
        eprintln!("[oai-runner] Context at capacity{}, compacting conversation history via LLM", label);
    }

    let compaction_request = ChatRequest {
        model: model.to_string(),
        messages: compaction_msgs,
        stream: false,
        tools: None,
        max_tokens: Some(2048),
        response_format: None,
        stream_options: None,
    };

    match client.stream_chat(&compaction_request, &mut |_| {}).await {
        Ok((summary_msg, _)) => {
            let summary = summary_msg.content.as_deref().unwrap_or("(compaction failed)");
            let transcript = context::build_pre_compaction_transcript(messages, compact_end);
            if let Some(sid) = session_id {
                append_compaction_history(&config_dir(), sid, &transcript);
            }
            compaction_history.push(transcript);
            context::apply_compaction(messages, compact_end, summary);
            true
        }
        Err(e) => {
            if label.is_empty() {
                eprintln!("[oai-runner] Compaction LLM call failed, falling back to truncation: {}", e);
            } else {
                eprintln!("[oai-runner] Compaction LLM call failed{}: {}, falling back to truncation", label, e);
            }
            false
        }
    }
}

fn apply_context_pressure_truncation(
    messages: &mut Vec<ChatMessage>,
    context_limit: usize,
    max_tokens: usize,
    bonus_reserve: usize,
) -> bool {
    let before_tokens = context::estimate_total_tokens(messages);
    let reserve = max_tokens.saturating_add(bonus_reserve);
    context::truncate_to_fit(messages, context_limit, reserve);
    context::sanitize_tool_call_pairs(messages);
    let after_tokens = context::estimate_total_tokens(messages);
    after_tokens < before_tokens
}

async fn recover_from_context_pressure(
    client: &ApiClient,
    model: &str,
    messages: &mut Vec<ChatMessage>,
    compaction_history: &mut Vec<String>,
    session_id: Option<&str>,
    context_limit: usize,
    max_tokens: usize,
    label: &str,
) -> bool {
    let before_tokens = context::estimate_total_tokens(messages);
    let compacted = compact_conversation_history(client, model, messages, compaction_history, session_id, label).await;

    let changed = apply_context_pressure_truncation(
        messages,
        context_limit,
        max_tokens,
        CONTEXT_PRESSURE_TRUNCATION_RESERVE_BONUS,
    ) || context::estimate_total_tokens(messages) < before_tokens
        || compacted;

    if changed {
        persist_session_snapshot(session_id, messages);
    }

    changed
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
            context::sanitize_tool_call_pairs(&mut messages);
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
    let mut tools_with_context_mgmt: Vec<ToolDefinition> =
        if has_final_message_tool { tools_with_final.clone() } else { tools.to_vec() };
    tools_with_context_mgmt.extend(crate::tools::definitions::context_management_tool_definitions());
    let effective_tools = &tools_with_context_mgmt;

    let mut compaction_history: Vec<String> =
        if let Some(sid) = session_id { load_compaction_history(&config_dir(), sid) } else { Vec::new() };

    let needs_tool_name_sanitization = effective_tools.iter().any(|t| t.function.name.contains('.'));
    let mut tool_name_reverse_map: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let sanitized_tools: Vec<ToolDefinition> = if needs_tool_name_sanitization {
        effective_tools
            .iter()
            .map(|t| {
                let mut t = t.clone();
                let original = t.function.name.clone();
                t.function.name = original.replace('.', "_");
                if t.function.name != original {
                    tool_name_reverse_map.insert(t.function.name.clone(), original);
                }
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

        let mut context_preflight_changed = false;
        if context::needs_compaction(&messages, context_limit, max_tokens) {
            context_preflight_changed |=
                compact_conversation_history(client, model, &mut messages, &mut compaction_history, session_id, "")
                    .await;
        }
        context_preflight_changed |= apply_context_pressure_truncation(&mut messages, context_limit, max_tokens, 0);
        if context_preflight_changed {
            persist_session_snapshot(session_id, &messages);
        }

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

        let mut context_pressure_retries_remaining = CONTEXT_PRESSURE_RETRY_LIMIT;
        let (assistant_msg, usage) = loop {
            let request = ChatRequest {
                model: model.to_string(),
                messages: messages.clone(),
                stream: true,
                tools: Some(api_tools.to_vec()),
                max_tokens: Some(max_tokens as u32),
                response_format: format.clone(),
                stream_options: Some(StreamOptions { include_usage: true }),
            };
            let mut emitted_any = false;
            let stream_result = client
                .stream_chat(&request, &mut |chunk| {
                    emitted_any = true;
                    output.text_chunk(chunk);
                })
                .await;

            match stream_result {
                Ok(result) => break result,
                Err(e) => {
                    if emitted_any || context_pressure_retries_remaining == 0 || !is_context_pressure_error(&e) {
                        return Err(e);
                    }

                    eprintln!("[oai-runner] Context-pressure request failed; compacting and retrying once: {}", e);
                    let recovered = recover_from_context_pressure(
                        client,
                        model,
                        &mut messages,
                        &mut compaction_history,
                        session_id,
                        context_limit,
                        max_tokens,
                        " during reactive recovery",
                    )
                    .await;
                    if !recovered {
                        eprintln!(
                            "[oai-runner] Context-pressure recovery did not reduce the transcript; aborting retry."
                        );
                        return Err(e);
                    }

                    context_pressure_retries_remaining -= 1;
                    continue;
                }
            }
        };

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
            // If tools are available and we're on an early turn, the model may have
            // output thinking text without calling tools. Nudge it to use tools.
            let has_tools = !api_tools.is_empty();
            if has_tools && turn < 3 {
                eprintln!("[oai-runner] Model responded without tool calls on turn {}. Prompting to use tools.", turn);
                messages.push(ChatMessage {
                    reasoning_content: None,
                    role: "user".to_string(),
                    content: Some(
                        "You have tools available. Use them to complete your task. \
                         Do not just describe what you would do — actually call the tools now."
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
                let name =
                    tool_name_reverse_map.get(&tc.function.name).cloned().unwrap_or_else(|| tc.function.name.clone());
                name == "final_message_json"
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

        let mut tc_idx = 0;
        while tc_idx < tool_calls.len() {
            let tc = &tool_calls[tc_idx];
            if cancel_token.is_cancelled() {
                eprintln!(
                    "[oai-runner] Cancelled between tool calls, synthesizing {} remaining results",
                    tool_calls.len() - tc_idx
                );
                for remaining_tc in &tool_calls[tc_idx..] {
                    messages.push(ChatMessage {
                        reasoning_content: None,
                        role: "tool".to_string(),
                        content: Some("[result unavailable — session was interrupted]".to_string()),
                        tool_calls: None,
                        tool_call_id: Some(remaining_tc.id.clone()),
                    });
                }
                break;
            }

            let tool_name =
                tool_name_reverse_map.get(&tc.function.name).cloned().unwrap_or_else(|| tc.function.name.clone());

            if tool_batch::is_parallel_read_only_tool(&tool_name) {
                let batch_len = tool_batch::parallel_read_only_batch_len(tool_calls, tc_idx);
                let batch_calls = &tool_calls[tc_idx..tc_idx + batch_len];

                for batch_tc in batch_calls {
                    let batch_tool_name = tool_name_reverse_map
                        .get(&batch_tc.function.name)
                        .cloned()
                        .unwrap_or_else(|| batch_tc.function.name.clone());
                    let batch_args: serde_json::Value =
                        serde_json::from_str(&batch_tc.function.arguments).unwrap_or(serde_json::Value::Null);
                    output.tool_call(&batch_tool_name, &batch_args);
                }

                let batch_outcomes =
                    tool_batch::execute_parallel_read_only_tools(batch_calls, working_dir, cancel_token.clone())
                        .await?;

                for outcome in batch_outcomes {
                    if outcome.emit_result {
                        if let Some(err_msg) = outcome.error.as_deref() {
                            output.tool_error(&outcome.tool_name, err_msg);
                        } else {
                            output.tool_result(&outcome.tool_name, &outcome.result);
                        }
                    }

                    messages.push(ChatMessage {
                        reasoning_content: None,
                        role: "tool".to_string(),
                        content: Some(outcome.result),
                        tool_calls: None,
                        tool_call_id: Some(outcome.tool_call_id),
                    });

                    if let Some(sid) = session_id {
                        let _ = save_session_messages_to(&config_dir(), sid, &messages);
                    }
                }

                tc_idx += batch_len;
                continue;
            }

            let args: serde_json::Value =
                serde_json::from_str(&tc.function.arguments).unwrap_or(serde_json::Value::Null);

            output.tool_call(&tool_name, &args);

            let result = if tool_name == "conversation_stats" {
                let total_tokens = context::estimate_total_tokens(&messages);
                let target = context_limit.saturating_sub(max_tokens);
                let usage_pct = if target > 0 { total_tokens * 100 / target } else { 100 };
                let r = format!(
                    "Messages: {}, Estimated tokens: {}/{} ({}% of limit), Compaction history entries: {}",
                    messages.len(),
                    total_tokens,
                    target,
                    usage_pct,
                    compaction_history.len()
                );
                output.tool_result(&tool_name, &r);
                r
            } else if tool_name == "search_compaction_history" {
                let query = args.get("query").and_then(|v| v.as_str()).unwrap_or("");
                if compaction_history.is_empty() {
                    let r = "No compaction history available — conversation has not been compacted yet.".to_string();
                    output.tool_result(&tool_name, &r);
                    r
                } else {
                    let mut matches = Vec::new();
                    for (epoch, transcript) in compaction_history.iter().enumerate() {
                        for (line_num, line) in transcript.lines().enumerate() {
                            if line.to_lowercase().contains(&query.to_lowercase()) {
                                let context_start = line_num.saturating_sub(2);
                                let context_lines: Vec<&str> = transcript.lines().skip(context_start).take(5).collect();
                                matches.push(format!(
                                    "[compaction #{}] line {}:\n{}",
                                    epoch + 1,
                                    line_num + 1,
                                    context_lines.join("\n")
                                ));
                            }
                        }
                    }
                    let r = if matches.is_empty() {
                        format!("No matches for '{}' in compaction history.", query)
                    } else {
                        let total = matches.len();
                        let shown: Vec<String> = matches.into_iter().take(20).collect();
                        format!("{} matches (showing up to 20):\n\n{}", total, shown.join("\n\n---\n\n"))
                    };
                    output.tool_result(&tool_name, &r);
                    r
                }
            } else if tool_name == "search_conversation" {
                let query = args.get("query").and_then(|v| v.as_str()).unwrap_or("");
                let query_lower = query.to_lowercase();
                let mut matches = Vec::new();

                for msg in &messages {
                    if let Some(content) = &msg.content {
                        for (line_num, line) in content.lines().enumerate() {
                            if line.to_lowercase().contains(&query_lower) {
                                let context_start = line_num.saturating_sub(1);
                                let context_lines: Vec<&str> = content.lines().skip(context_start).take(3).collect();
                                matches.push(format!("[{}]: {}", msg.role, context_lines.join("\n")));
                            }
                        }
                    }
                }

                for (epoch, transcript) in compaction_history.iter().enumerate() {
                    for (line_num, line) in transcript.lines().enumerate() {
                        if line.to_lowercase().contains(&query_lower) {
                            let context_start = line_num.saturating_sub(2);
                            let context_lines: Vec<&str> = transcript.lines().skip(context_start).take(5).collect();
                            matches.push(format!(
                                "[compaction #{}] line {}:\n{}",
                                epoch + 1,
                                line_num + 1,
                                context_lines.join("\n")
                            ));
                        }
                    }
                }

                let r = if matches.is_empty() {
                    format!("No matches for '{}' in conversation or compaction history.", query)
                } else {
                    let total = matches.len();
                    let shown: Vec<String> = matches.into_iter().take(30).collect();
                    format!("{} matches (showing up to 30):\n\n{}", total, shown.join("\n\n---\n\n"))
                };
                output.tool_result(&tool_name, &r);
                r
            } else if let Some(mcp) = mcp_client::find_client_for_tool(mcp_clients, &tool_name) {
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
            tc_idx += 1;
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
        assert_eq!(loaded.len(), MAX_RESUME_MESSAGES);
    }

    #[test]
    fn load_session_does_not_orphan_tool_messages_at_trim_boundary() {
        use crate::api::types::{FunctionCall, ToolCall};

        let dir = tempfile::tempdir().unwrap();
        let base = dir.path();
        let sid = "test-session-tool-boundary";

        let mut messages = vec![ChatMessage {
            reasoning_content: None,
            role: "system".to_string(),
            content: Some("sys".to_string()),
            tool_calls: None,
            tool_call_id: None,
        }];
        for i in 0..40 {
            messages.push(ChatMessage {
                reasoning_content: None,
                role: "user".to_string(),
                content: Some(format!("q{}", i)),
                tool_calls: None,
                tool_call_id: None,
            });
            messages.push(ChatMessage {
                reasoning_content: None,
                role: "assistant".to_string(),
                content: None,
                tool_calls: Some(vec![ToolCall {
                    id: format!("call_{}", i),
                    type_: "function".to_string(),
                    function: FunctionCall { name: "test".to_string(), arguments: "{}".to_string() },
                }]),
                tool_call_id: None,
            });
            messages.push(ChatMessage {
                reasoning_content: None,
                role: "tool".to_string(),
                content: Some(format!("r{}", i)),
                tool_calls: None,
                tool_call_id: Some(format!("call_{}", i)),
            });
        }

        save_session_messages_to(base, sid, &messages).unwrap();
        let loaded = load_session_messages_from(base, sid);

        for (i, m) in loaded.iter().enumerate() {
            if m.role == "tool" {
                let tc_id = m.tool_call_id.as_deref().unwrap();
                assert!(
                    loaded[..i].iter().any(|prev| {
                        prev.role == "assistant"
                            && prev.tool_calls.as_ref().is_some_and(|tcs| tcs.iter().any(|tc| tc.id == tc_id))
                    }),
                    "tool message '{}' at index {} has no matching assistant",
                    tc_id,
                    i
                );
            }
        }
    }

    #[test]
    fn detects_context_pressure_error_messages() {
        assert!(is_context_pressure_error_message("API returned 400 Bad Request: maximum context length exceeded"));
        assert!(is_context_pressure_error_message("prompt too long"));
        assert!(is_context_pressure_error_message("API returned 413 Request Entity Too Large"));
        assert!(!is_context_pressure_error_message("API returned 400 Bad Request: invalid tool name"));
    }

    #[test]
    fn recovery_truncation_is_conservative_but_effective() {
        let mut messages = vec![ChatMessage {
            reasoning_content: None,
            role: "system".to_string(),
            content: Some("You are helpful.".to_string()),
            tool_calls: None,
            tool_call_id: None,
        }];
        for i in 0..10 {
            messages.push(ChatMessage {
                reasoning_content: None,
                role: "user".to_string(),
                content: Some(format!("{} {}", i, "x ".repeat(400))),
                tool_calls: None,
                tool_call_id: None,
            });
        }

        let before = context::estimate_total_tokens(&messages);
        let changed =
            apply_context_pressure_truncation(&mut messages, 1024, 64, CONTEXT_PRESSURE_TRUNCATION_RESERVE_BONUS);
        let after = context::estimate_total_tokens(&messages);

        assert!(changed);
        assert!(after < before);
    }
}
