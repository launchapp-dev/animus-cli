use anyhow::{bail, Result};
use futures_util::StreamExt;
use std::io::Write;
use std::time::Duration;

use super::types::*;

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
        let url = format!("{}/chat/completions", self.api_base);

        let mut last_err = None;
        for attempt in 0..3 {
            if attempt > 0 {
                let delay = Duration::from_millis(500 * 2u64.pow(attempt as u32));
                tokio::time::sleep(delay).await;
            }

            match self.do_stream(&url, request, on_text_chunk).await {
                Ok(result) => return Ok(result),
                Err(e) => {
                    let err_str = e.to_string();
                    // Retry on 429 (rate limit) or 5xx server errors
                    // Check for " 5" to match " 500", " 502", etc. in error messages
                    let should_retry = err_str.contains("429") || (err_str.contains(" 5") && attempt < 2);
                    if should_retry {
                        last_err = Some(e);
                        continue;
                    }
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
