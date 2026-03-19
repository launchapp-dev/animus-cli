use anyhow::{bail, Result};
use eventsource_stream::Eventsource;
use futures_util::StreamExt;
use std::io::Write;
use std::time::Duration;

use super::circuit_breaker::{
    check_circuit, record_failure, record_success, CircuitBreakerConfig, CircuitCheck,
};
use super::types::*;

pub struct ApiClient {
    http: reqwest::Client,
    api_base: String,
    api_key: String,
    cb_config: CircuitBreakerConfig,
}

impl ApiClient {
    pub fn new(api_base: String, api_key: String, timeout_secs: u64) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(timeout_secs))
            .build()
            .expect("failed to build HTTP client");
        Self { http, api_base, api_key, cb_config: CircuitBreakerConfig::default() }
    }

    pub async fn stream_chat(
        &self,
        request: &ChatRequest,
        on_text_chunk: &mut dyn FnMut(&str),
    ) -> Result<(ChatMessage, Option<UsageInfo>)> {
        match check_circuit(&self.api_base, &self.cb_config) {
            CircuitCheck::Allow => {}
            CircuitCheck::AllowProbe => {
                eprintln!(
                    "[oai-runner] Circuit half-open for {}. Sending probe request.",
                    self.api_base
                );
            }
            CircuitCheck::Reject { open_until_secs } => {
                bail!(
                    "Circuit breaker OPEN for {} — too many consecutive failures. Retry after {} (unix secs).",
                    self.api_base,
                    open_until_secs
                );
            }
        }

        let url = format!("{}/chat/completions", self.api_base);

        let mut last_err = None;
        for attempt in 0..3 {
            if attempt > 0 {
                let delay = Duration::from_millis(500 * 2u64.pow(attempt as u32));
                tokio::time::sleep(delay).await;
            }

            match self.do_stream(&url, request, on_text_chunk).await {
                Ok(result) => {
                    record_success(&self.api_base);
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
                        record_failure(&self.api_base, &self.cb_config);
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
                    record_failure(&self.api_base, &self.cb_config);
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

        let mut stream = resp.bytes_stream().eventsource();

        while let Some(event_result) = stream.next().await {
            let event = match event_result {
                Ok(event) => event,
                Err(e) => {
                    eprintln!("[oai-runner] SSE parse error: {}", e);
                    continue;
                }
            };

            if event.data == "[DONE]" {
                std::io::stdout().flush().ok();
                let msg = ChatMessage {
                    role: "assistant".to_string(),
                    content: if content.is_empty() { None } else { Some(content) },
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
