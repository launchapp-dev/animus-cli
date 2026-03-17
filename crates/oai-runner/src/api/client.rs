use anyhow::{bail, Result};
use futures_util::StreamExt;
use std::io::Write;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::Duration;

use super::types::*;

static CONSECUTIVE_FAILURES: AtomicU32 = AtomicU32::new(0);
static CIRCUIT_OPEN_UNTIL: AtomicU64 = AtomicU64::new(0);

const CIRCUIT_BREAKER_THRESHOLD: u32 = 5;
const CIRCUIT_BREAKER_COOLDOWN_SECS: u64 = 60;

fn circuit_is_open() -> bool {
    let until = CIRCUIT_OPEN_UNTIL.load(Ordering::Relaxed);
    if until == 0 {
        return false;
    }
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs();
    now < until
}

fn record_success() {
    CONSECUTIVE_FAILURES.store(0, Ordering::Relaxed);
    CIRCUIT_OPEN_UNTIL.store(0, Ordering::Relaxed);
}

fn record_failure() {
    let count = CONSECUTIVE_FAILURES.fetch_add(1, Ordering::Relaxed) + 1;
    if count >= CIRCUIT_BREAKER_THRESHOLD {
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs();
        CIRCUIT_OPEN_UNTIL.store(now + CIRCUIT_BREAKER_COOLDOWN_SECS, Ordering::Relaxed);
        eprintln!(
            "[oai-runner] Circuit breaker OPEN after {} consecutive failures. Cooling down for {}s.",
            count, CIRCUIT_BREAKER_COOLDOWN_SECS
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
        if circuit_is_open() {
            bail!("Circuit breaker is open — too many consecutive API failures. Waiting for cooldown.");
        }

        let url = format!("{}/chat/completions", self.api_base);

        let mut last_err = None;
        for attempt in 0..3 {
            if attempt > 0 {
                let delay = Duration::from_millis(500 * 2u64.pow(attempt as u32));
                tokio::time::sleep(delay).await;
            }

            match self.do_stream_with_retry_after(&url, request, on_text_chunk).await {
                Ok(result) => {
                    record_success();
                    return Ok(result);
                }
                Err(e) => {
                    let err_str = e.to_string();
                    let is_rate_limit = err_str.contains("429");
                    let is_server_error = err_str.contains(" 5") && attempt < 2;
                    if is_rate_limit || is_server_error {
                        record_failure();
                        eprintln!(
                            "[oai-runner] Retry {}/3: {}",
                            attempt + 1,
                            if is_rate_limit { "rate limited (429)" } else { "server error" }
                        );
                        last_err = Some(e);
                        continue;
                    }
                    record_failure();
                    return Err(e);
                }
            }
        }

        Err(last_err.unwrap_or_else(|| anyhow::anyhow!("stream_chat failed after retries")))
    }

    async fn do_stream_with_retry_after(
        &self,
        url: &str,
        request: &ChatRequest,
        on_text_chunk: &mut dyn FnMut(&str),
    ) -> Result<(ChatMessage, Option<UsageInfo>)> {
        let resp = self
            .http
            .post(url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
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
        let mut tool_calls: Vec<ToolCall> = Vec::new();
        let mut usage: Option<UsageInfo> = None;

        let mut stream = resp.bytes_stream();
        let mut buffer = String::new();

        while let Some(chunk_result) = stream.next().await {
            let chunk = chunk_result?;
            buffer.push_str(&String::from_utf8_lossy(&chunk));

            while let Some(line_end) = buffer.find('\n') {
                let line = buffer[..line_end].trim().to_string();
                buffer = buffer[line_end + 1..].to_string();

                if line.is_empty() || line.starts_with(':') {
                    continue;
                }

                if !line.starts_with("data: ") {
                    continue;
                }

                let data = &line[6..];

                if data == "[DONE]" {
                    std::io::stdout().flush().ok();
                    let msg = ChatMessage {
                        role: "assistant".to_string(),
                        content: if content.is_empty() { None } else { Some(content) },
                        tool_calls: if tool_calls.is_empty() { None } else { Some(tool_calls) },
                        tool_call_id: None,
                    };
                    return Ok((msg, usage));
                }

                let parsed: StreamChunk = match serde_json::from_str(data) {
                    Ok(c) => c,
                    Err(_) => continue,
                };

                if let Some(u) = parsed.usage {
                    usage = Some(u);
                }

                for choice in &parsed.choices {
                    if let Some(text) = &choice.delta.content {
                        content.push_str(text);
                        on_text_chunk(text);
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
        }

        std::io::stdout().flush().ok();
        let msg = ChatMessage {
            role: "assistant".to_string(),
            content: if content.is_empty() { None } else { Some(content) },
            tool_calls: if tool_calls.is_empty() { None } else { Some(tool_calls) },
            tool_call_id: None,
        };
        Ok((msg, usage))
    }
}
