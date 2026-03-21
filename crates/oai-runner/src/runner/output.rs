use crate::pricing;
use serde_json::json;
use std::io::Write;

pub struct OutputFormatter {
    json_mode: bool,
    text_buffer: String,
    total_input_tokens: u64,
    total_output_tokens: u64,
    total_tokens: u64,
    request_count: u32,
    model: String,
    total_cost_usd: f64,
}

impl OutputFormatter {
    pub fn new(json_mode: bool, model: &str) -> Self {
        Self {
            json_mode,
            text_buffer: String::new(),
            total_input_tokens: 0,
            total_output_tokens: 0,
            total_tokens: 0,
            request_count: 0,
            model: model.to_string(),
            total_cost_usd: 0.0,
        }
    }

    pub fn text_chunk(&mut self, text: &str) {
        if self.json_mode {
            self.text_buffer.push_str(text);
        } else {
            print!("{}", text);
            std::io::stdout().flush().ok();
        }
    }

    pub fn flush_result(&mut self) {
        if self.json_mode && !self.text_buffer.is_empty() {
            let event = json!({
                "type": "result",
                "text": self.text_buffer
            });
            println!("{}", event);
            self.text_buffer.clear();
        }
    }

    pub fn tool_call(&self, tool_name: &str, arguments: &serde_json::Value) {
        if self.json_mode {
            let event = json!({
                "type": "tool_call",
                "tool_name": tool_name,
                "arguments": arguments
            });
            println!("{}", event);
        }
    }

    pub fn tool_result(&self, tool_name: &str, result: &str) {
        if self.json_mode {
            let event = json!({
                "type": "tool_result",
                "tool_name": tool_name,
                "output": result
            });
            println!("{}", event);
        } else {
            println!("\n[Tool Result: {}]", tool_name);
            println!("{}", result);
        }
    }

    pub fn tool_error(&self, tool_name: &str, error: &str) {
        if self.json_mode {
            let event = json!({
                "type": "tool_error",
                "tool_name": tool_name,
                "error": error
            });
            println!("{}", event);
        } else {
            eprintln!("\n[Tool Error: {}] {}", tool_name, error);
        }
    }

    /// Record token usage for a single API request and emit a metadata event.
    /// Computes cost for this request using the model's pricing and accumulates
    /// it into the session total.
    pub fn metadata(&mut self, usage: &crate::api::types::UsageInfo) {
        self.total_input_tokens += usage.prompt_tokens;
        self.total_output_tokens += usage.completion_tokens;
        self.request_count += 1;

        let req_total = usage.effective_total();
        self.total_tokens += req_total;

        let req_cost = pricing::lookup(&self.model)
            .map(|p| p.cost(usage.prompt_tokens, usage.completion_tokens));
        if let Some(cost) = req_cost {
            self.total_cost_usd += cost;
        }

        if self.json_mode {
            let mut event = json!({
                "type": "metadata",
                "tokens": {
                    "input": usage.prompt_tokens,
                    "output": usage.completion_tokens,
                    "total": req_total
                },
                "model": self.model,
                "request": self.request_count
            });
            if let Some(cost) = req_cost {
                event["cost_usd"] = json!(cost);
            }
            println!("{}", event);
        }
    }

    /// Emit a structured cost event for CI/cost audit pipelines.
    /// Called automatically by `emit_session_summary`, but can be called
    /// independently for intermediate cost snapshots.
    pub fn emit_cost_event(&self) {
        if self.total_tokens == 0 {
            return;
        }
        let mut event = json!({
            "type": "cost",
            "model": self.model,
            "tokens": {
                "total_input": self.total_input_tokens,
                "total_output": self.total_output_tokens,
                "total": self.total_tokens,
                "requests": self.request_count
            }
        });
        if let Some(pricing) = pricing::lookup(&self.model) {
            event["cost_usd"] = json!(self.total_cost_usd);
            event["pricing"] = json!({
                "input_per_million": pricing.input_per_million,
                "output_per_million": pricing.output_per_million
            });
        } else {
            event["cost_usd"] = json!(null);
            event["pricing"] = json!(null);
        }
        println!("{}", event);
    }

    pub fn emit_session_summary(&self) {
        if self.total_tokens == 0 {
            return;
        }

        // Always emit the structured cost event (both json and text mode)
        // so it appears in the JSONL log for audit pipelines.
        if self.json_mode {
            self.emit_cost_event();

            let mut event = json!({
                "type": "session_summary",
                "tokens": {
                    "total_input": self.total_input_tokens,
                    "total_output": self.total_output_tokens,
                    "total": self.total_tokens,
                    "requests": self.request_count
                },
                "model": self.model
            });
            if let Some(pricing) = pricing::lookup(&self.model) {
                event["cost_usd"] = json!(self.total_cost_usd);
                event["pricing"] = json!({
                    "input_per_million": pricing.input_per_million,
                    "output_per_million": pricing.output_per_million
                });
            }
            println!("{}", event);
        } else {
            // Emit cost event in text mode too (JSON line, parseable by CI)
            self.emit_cost_event();

            let pricing_info = match pricing::lookup(&self.model) {
                Some(p) => format!(
                    ", cost: ${:.6} (${:.2}/${:.2} per 1M tokens in/out)",
                    self.total_cost_usd, p.input_per_million, p.output_per_million
                ),
                None => String::new(),
            };
            eprintln!(
                "[oai-runner] Session: {} requests, {} input + {} output = {} total tokens (model: {}){}",
                self.request_count, self.total_input_tokens, self.total_output_tokens, self.total_tokens,
                self.model, pricing_info
            );
        }
    }

    pub fn newline(&self) {
        println!();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_formatter_json_mode_initializes_empty_buffer() {
        let formatter = OutputFormatter::new(true, "gpt-4o");
        assert!(formatter.text_buffer.is_empty());
        assert!(formatter.json_mode);
        assert_eq!(formatter.model, "gpt-4o");
    }

    #[test]
    fn output_formatter_text_mode_does_not_buffer() {
        let formatter = OutputFormatter::new(false, "gpt-4o");
        assert!(!formatter.json_mode);
        assert!(formatter.text_buffer.is_empty());
    }

    #[test]
    fn text_chunk_accumulates_in_buffer_for_json_mode() {
        let mut formatter = OutputFormatter::new(true, "gpt-4o");
        formatter.text_buffer.push_str("hello ");
        formatter.text_buffer.push_str("world");
        assert_eq!(formatter.text_buffer, "hello world");
    }

    #[test]
    fn flush_result_clears_buffer() {
        let mut formatter = OutputFormatter::new(true, "gpt-4o");
        formatter.text_buffer.push_str("accumulated text");
        assert!(!formatter.text_buffer.is_empty());
        formatter.text_buffer.clear();
        assert!(formatter.text_buffer.is_empty());
    }

    #[test]
    fn metadata_accumulates_tokens_and_cost() {
        let mut formatter = OutputFormatter::new(true, "gpt-4o");
        // gpt-4o: $2.50/1M input, $10.00/1M output
        let usage = crate::api::types::UsageInfo {
            prompt_tokens: 1000,
            completion_tokens: 500,
            total_tokens: 1500,
        };
        formatter.metadata(&usage);
        assert_eq!(formatter.total_input_tokens, 1000);
        assert_eq!(formatter.total_output_tokens, 500);
        assert_eq!(formatter.total_tokens, 1500);
        assert_eq!(formatter.request_count, 1);
        // Expected cost: (1000/1e6)*2.5 + (500/1e6)*10.0 = 0.0025 + 0.005 = 0.0075
        assert!((formatter.total_cost_usd - 0.0075).abs() < 1e-9);
    }

    #[test]
    fn metadata_accumulates_across_multiple_requests() {
        let mut formatter = OutputFormatter::new(true, "gpt-4o");
        formatter.metadata(&crate::api::types::UsageInfo {
            prompt_tokens: 1000,
            completion_tokens: 500,
            total_tokens: 0,
        });
        formatter.metadata(&crate::api::types::UsageInfo {
            prompt_tokens: 2000,
            completion_tokens: 1000,
            total_tokens: 0,
        });
        assert_eq!(formatter.total_input_tokens, 3000);
        assert_eq!(formatter.total_output_tokens, 1500);
        assert_eq!(formatter.total_tokens, 4500);
        assert_eq!(formatter.request_count, 2);
        // Cost: 0.0075 + (2000/1e6)*2.5 + (1000/1e6)*10.0 = 0.0075 + 0.005 + 0.01 = 0.0225
        assert!((formatter.total_cost_usd - 0.0225).abs() < 1e-9);
    }

    #[test]
    fn metadata_unknown_model_has_zero_cost() {
        let mut formatter = OutputFormatter::new(true, "unknown-model-xyz");
        formatter.metadata(&crate::api::types::UsageInfo {
            prompt_tokens: 1000,
            completion_tokens: 500,
            total_tokens: 0,
        });
        assert_eq!(formatter.total_input_tokens, 1000);
        assert!((formatter.total_cost_usd - 0.0).abs() < 1e-9);
    }

    #[test]
    fn metadata_claude_sonnet_pricing() {
        let mut formatter = OutputFormatter::new(true, "claude-sonnet-4-20250514");
        // claude-sonnet-4: $3.00/1M input, $15.00/1M output
        formatter.metadata(&crate::api::types::UsageInfo {
            prompt_tokens: 500_000,
            completion_tokens: 100_000,
            total_tokens: 600_000,
        });
        // Cost: (500000/1e6)*3.0 + (100000/1e6)*15.0 = 1.5 + 1.5 = 3.0
        assert!((formatter.total_cost_usd - 3.0).abs() < 1e-9);
    }

    #[test]
    fn metadata_deepseek_pricing() {
        let mut formatter = OutputFormatter::new(true, "deepseek-chat");
        // deepseek-chat: $0.14/1M input, $0.28/1M output
        formatter.metadata(&crate::api::types::UsageInfo {
            prompt_tokens: 1_000_000,
            completion_tokens: 1_000_000,
            total_tokens: 0,
        });
        // Cost: 0.14 + 0.28 = 0.42
        assert!((formatter.total_cost_usd - 0.42).abs() < 1e-9);
    }

    #[test]
    fn metadata_effective_total_prefers_provider_value() {
        let mut formatter = OutputFormatter::new(true, "gpt-4o");
        // Provider reports total_tokens=999 which differs from sum
        formatter.metadata(&crate::api::types::UsageInfo {
            prompt_tokens: 1000,
            completion_tokens: 500,
            total_tokens: 999,
        });
        assert_eq!(formatter.total_tokens, 999);
    }

    #[test]
    fn metadata_effective_total_falls_back_to_sum() {
        let mut formatter = OutputFormatter::new(true, "gpt-4o");
        // Provider doesn't report total_tokens (0)
        formatter.metadata(&crate::api::types::UsageInfo {
            prompt_tokens: 1000,
            completion_tokens: 500,
            total_tokens: 0,
        });
        assert_eq!(formatter.total_tokens, 1500);
    }

    #[test]
    fn emit_session_summary_skips_when_no_tokens() {
        let formatter = OutputFormatter::new(true, "gpt-4o");
        // Should not panic, should return early
        formatter.emit_session_summary();
        assert_eq!(formatter.total_tokens, 0);
    }
}
