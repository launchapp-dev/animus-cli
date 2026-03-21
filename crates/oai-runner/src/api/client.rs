use anyhow::{bail, Result};
use eventsource_stream::Eventsource;
use futures_util::StreamExt;
use std::collections::HashMap;
use std::io::Write;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use super::types::*;

static PROVIDER_STATES: RwLock<Option<HashMap<String, Arc<ProviderState>>>> = RwLock::new(None);

struct ProviderState {
    consecutive_failures: AtomicU32,
    circuit_open_until: AtomicU64,
}

const CIRCUIT_BREAKER_THRESHOLD: u32 = 5;
const CIRCUIT_BREAKER_COOLDOWN_SECS: u64 = 60;

fn get_provider_state(api_base: &str) -> Arc<ProviderState> {
    {
        let read = PROVIDER_STATES.read().unwrap();
        if let Some(map) = read.as_ref() {
            if let Some(state) = map.get(api_base) {
                return state.clone();
            }
        }
    }

    let mut write = PROVIDER_STATES.write().unwrap();
    let map = write.get_or_insert_with(HashMap::new);
    map.entry(api_base.to_string())
        .or_insert_with(|| {
            Arc::new(ProviderState { consecutive_failures: AtomicU32::new(0), circuit_open_until: AtomicU64::new(0) })
        })
        .clone()
}

fn circuit_is_open(state: &ProviderState) -> bool {
    let until = state.circuit_open_until.load(Ordering::Relaxed);
    if until == 0 {
        return false;
    }
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs();
    now < until
}

fn record_success(state: &ProviderState) {
    state.consecutive_failures.store(0, Ordering::Relaxed);
    state.circuit_open_until.store(0, Ordering::Relaxed);
}

fn record_failure(state: &ProviderState, api_base: &str) {
    let count = state.consecutive_failures.fetch_add(1, Ordering::Relaxed) + 1;
    if count >= CIRCUIT_BREAKER_THRESHOLD {
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs();
        state.circuit_open_until.store(now + CIRCUIT_BREAKER_COOLDOWN_SECS, Ordering::Relaxed);
        eprintln!(
            "[oai-runner] Circuit breaker OPEN for {} after {} consecutive failures. Cooling down for {}s.",
            api_base, count, CIRCUIT_BREAKER_COOLDOWN_SECS
        );
    }
}

pub struct ApiClient {
    http: reqwest::Client,
    api_base: String,
    api_key: String,
}

impl ApiClient {
    pub fn new(api_base: String, api_key: String, timeout_secs: u64) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(timeout_secs))
            .build()
            .expect("failed to build HTTP client");
        Self { http, api_base, api_key }
    }

    pub async fn stream_chat(
        &self,
        request: &ChatRequest,
        on_text_chunk: &mut dyn FnMut(&str),
    ) -> Result<(ChatMessage, Option<UsageInfo>)> {
        let state = get_provider_state(&self.api_base);
        if circuit_is_open(&state) {
            bail!(
                "Circuit breaker is open for {} — too many consecutive API failures. Waiting for cooldown.",
                self.api_base
            );
        }

        let url = format!("{}/chat/completions", self.api_base);

        let mut last_err = None;
        let mut emitted_any = false;

        for attempt in 0..3 {
            if attempt > 0 {
                if emitted_any {
                    // If we already sent chunks to the user, retrying the whole request
                    // will lead to duplicate output. Better to fail or implement resume.
                    // OpenAI-style APIs usually don't support resume mid-stream.
                    return Err(
                        last_err.unwrap_or_else(|| anyhow::anyhow!("Stream interrupted after emitting content"))
                    );
                }

                let delay = Duration::from_millis(500 * 2u64.pow(attempt as u32));
                tokio::time::sleep(delay).await;
            }

            let mut chunk_interceptor = |chunk: &str| {
                emitted_any = true;
                on_text_chunk(chunk);
            };

            match self.do_stream(&url, request, &mut chunk_interceptor).await {
                Ok(result) => {
                    record_success(&state);
                    return Ok(result);
                }
                Err(e) => {
                    let err_str = e.to_string();
                    let is_rate_limit = err_str.contains("429");
                    let is_server_error = err_str.contains(" 5") && attempt < 2;
                    let is_transient = err_str.contains("EOF")
                        || err_str.contains("connection closed")
                        || err_str.contains("broken pipe")
                        || err_str.contains("reset by peer");

                    if is_rate_limit || is_server_error || is_transient {
                        record_failure(&state, &self.api_base);
                        let reason = if is_rate_limit {
                            "rate limited (429)"
                        } else if is_transient {
                            "transient connection error"
                        } else {
                            "server error"
                        };
                        eprintln!("[oai-runner] Retry {}/3: {}", attempt + 1, reason);
                        last_err = Some(e);
                        continue;
                    }
                    record_failure(&state, &self.api_base);
                    return Err(e);
                }
            }
        }

        Err(last_err.unwrap_or_else(|| anyhow::anyhow!("stream_chat failed after retries")))
    }

    async fn do_stream(
        &self,
        url: &str,
        request: &ChatRequest,
        on_text_chunk: &mut dyn FnMut(&str),
    ) -> Result<(ChatMessage, Option<UsageInfo>)> {
        if std::env::var("AO_DEBUG_REQUESTS").is_ok() {
            eprintln!("[oai-runner] Request body: {}", serde_json::to_string(request).unwrap_or_default());
        }
        let resp = self
            .http
            .post(url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .header("User-Agent", "claude-code/2.1.80")
            .json(request)
            .send()
            .await?;

        let status = resp.status();
        if !status.is_success() {
            if status.as_u16() == 429 {
                if let Some(retry_after) = resp.headers().get("retry-after") {
                    if let Ok(secs) = retry_after.to_str().unwrap_or("0").parse::<u64>() {
                        let wait = secs.min(120);
                        eprintln!("[oai-runner] Rate limited. Retry-After: {}s", wait);
                        tokio::time::sleep(Duration::from_secs(wait)).await;
                    }
                }
            }
            let body = resp.text().await.unwrap_or_default();
            bail!("API returned {} {}: {}", status.as_u16(), status.as_str(), body);
        }

        let mut content = String::new();
        let mut reasoning_content = String::new();
        let mut tool_calls: Vec<ToolCall> = Vec::new();
        let mut usage: Option<UsageInfo> = None;

        let mut stream = resp.bytes_stream().eventsource();

        while let Some(event_result) = stream.next().await {
            let event = match event_result {
                Ok(event) => event,
                Err(e) => {
                    // If we fail mid-stream, we should probably return an error so stream_chat can decide to retry
                    bail!("SSE stream error: {}", e);
                }
            };

            if event.data == "[DONE]" {
                std::io::stdout().flush().ok();
                let msg = ChatMessage {
                    role: "assistant".to_string(),
                    content: Some(content),
                    reasoning_content: Some(reasoning_content),
                    tool_calls: if tool_calls.is_empty() { None } else { Some(tool_calls) },
                    tool_call_id: None,
                };
                return Ok((msg, usage));
            }

            let parsed: StreamChunk = match serde_json::from_str(&event.data) {
                Ok(c) => c,
                Err(_) => continue,
            };

            if let Some(u) = parsed.usage {
                usage = Some(u);
            }

            // Only care about the first choice for agent loop
            if let Some(choice) = parsed.choices.first() {
                if let Some(text) = &choice.delta.content {
                    content.push_str(text);
                    on_text_chunk(text);
                }
                if let Some(text) = &choice.delta.reasoning_content {
                    reasoning_content.push_str(text);
                }

                if let Some(tc_deltas) = &choice.delta.tool_calls {
                    for tc_delta in tc_deltas {
                        let idx = tc_delta.index;

                        while tool_calls.len() <= idx {
                            tool_calls.push(ToolCall {
                                id: String::new(),
                                type_: "function".to_string(),
                                function: FunctionCall { name: String::new(), arguments: String::new() },
                            });
                        }

                        if let Some(id) = &tc_delta.id {
                            tool_calls[idx].id = id.clone();
                        }
                        if let Some(fc) = &tc_delta.function {
                            if let Some(name) = &fc.name {
                                tool_calls[idx].function.name = name.clone();
                            }
                            if let Some(args) = &fc.arguments {
                                tool_calls[idx].function.arguments.push_str(args);
                            }
                        }
                    }
                }
            }
        }

        std::io::stdout().flush().ok();
        let msg = ChatMessage {
            role: "assistant".to_string(),
            content: if content.is_empty() { None } else { Some(content) },
            reasoning_content: Some(reasoning_content),
            tool_calls: if tool_calls.is_empty() { None } else { Some(tool_calls) },
            tool_call_id: None,
        };
        Ok((msg, usage))
    }
}
