/// Per-model token pricing for cost estimation.
///
/// Prices are in USD per million tokens. The pricing table covers major
/// providers (OpenAI, Anthropic, Google, DeepSeek, etc.) and is used by
/// `OutputFormatter` to compute per-request and session-level costs.
///
/// Unknown models return `None` from [`lookup`], and callers can choose to
/// skip cost reporting rather than invent a bogus number.

use std::sync::LazyLock;

/// Per-million-token pricing for a single model.
#[derive(Debug, Clone, Copy)]
pub struct ModelPricing {
    /// USD per 1M input (prompt) tokens.
    pub input_per_million: f64,
    /// USD per 1M output (completion) tokens.
    pub output_per_million: f64,
}

impl ModelPricing {
    pub const fn new(input_per_million: f64, output_per_million: f64) -> Self {
        Self { input_per_million, output_per_million }
    }

    /// Compute the USD cost for the given token counts.
    #[inline]
    pub fn cost(&self, prompt_tokens: u64, completion_tokens: u64) -> f64 {
        let input_cost = (prompt_tokens as f64 / 1_000_000.0) * self.input_per_million;
        let output_cost = (completion_tokens as f64 / 1_000_000.0) * self.output_per_million;
        input_cost + output_cost
    }
}

/// A pricing entry with one or more model name prefixes that should match.
struct PricingEntry {
    /// Lowercased model-name substrings / prefixes that trigger this entry.
    /// First match wins.
    prefixes: &'static [&'static str],
    pricing: ModelPricing,
}

/// Ordered list of pricing entries. First match wins, so more specific
/// entries must come before broader ones.
static PRICING_TABLE: LazyLock<Vec<PricingEntry>> = LazyLock::new(|| {
    vec![
        // ── OpenAI ────────────────────────────────────────────────
        PricingEntry {
            prefixes: &["o4-mini"],
            pricing: ModelPricing::new(1.10, 4.40),
        },
        PricingEntry {
            prefixes: &["o3-pro"],
            pricing: ModelPricing::new(10.00, 40.00),
        },
        PricingEntry {
            prefixes: &["o3"],
            pricing: ModelPricing::new(2.00, 8.00),
        },
        PricingEntry {
            prefixes: &["o1-pro"],
            pricing: ModelPricing::new(150.00, 600.00),
        },
        PricingEntry {
            prefixes: &["o1-preview"],
            pricing: ModelPricing::new(15.00, 60.00),
        },
        PricingEntry {
            prefixes: &["o1-mini"],
            pricing: ModelPricing::new(3.00, 12.00),
        },
        PricingEntry {
            prefixes: &["o1"],
            pricing: ModelPricing::new(15.00, 60.00),
        },
        PricingEntry {
            prefixes: &["gpt-4.1-mini"],
            pricing: ModelPricing::new(0.40, 1.60),
        },
        PricingEntry {
            prefixes: &["gpt-4.1-nano"],
            pricing: ModelPricing::new(0.10, 0.40),
        },
        PricingEntry {
            prefixes: &["gpt-4.1"],
            pricing: ModelPricing::new(2.00, 8.00),
        },
        PricingEntry {
            prefixes: &["gpt-4.5-preview"],
            pricing: ModelPricing::new(75.00, 150.00),
        },
        PricingEntry {
            prefixes: &["gpt-4.5"],
            pricing: ModelPricing::new(75.00, 150.00),
        },
        PricingEntry {
            prefixes: &["gpt-4o-mini"],
            pricing: ModelPricing::new(0.15, 0.60),
        },
        PricingEntry {
            prefixes: &["gpt-4o"],
            pricing: ModelPricing::new(2.50, 10.00),
        },
        PricingEntry {
            prefixes: &["gpt-4-turbo"],
            pricing: ModelPricing::new(10.00, 30.00),
        },
        PricingEntry {
            prefixes: &["gpt-4"],
            pricing: ModelPricing::new(30.00, 60.00),
        },

        // ── Anthropic ─────────────────────────────────────────────
        PricingEntry {
            prefixes: &["claude-opus-4"],
            pricing: ModelPricing::new(15.00, 75.00),
        },
        PricingEntry {
            prefixes: &["claude-sonnet-4"],
            pricing: ModelPricing::new(3.00, 15.00),
        },
        PricingEntry {
            prefixes: &["claude-3.7-sonnet", "claude-3-7-sonnet"],
            pricing: ModelPricing::new(3.00, 15.00),
        },
        PricingEntry {
            prefixes: &["claude-3.5-haiku", "claude-3-5-haiku"],
            pricing: ModelPricing::new(0.80, 4.00),
        },
        PricingEntry {
            prefixes: &["claude-3.5-sonnet", "claude-3-5-sonnet"],
            pricing: ModelPricing::new(3.00, 15.00),
        },
        PricingEntry {
            prefixes: &["claude-3-opus"],
            pricing: ModelPricing::new(15.00, 75.00),
        },
        PricingEntry {
            prefixes: &["claude-3-sonnet"],
            pricing: ModelPricing::new(3.00, 15.00),
        },
        PricingEntry {
            prefixes: &["claude-3-haiku"],
            pricing: ModelPricing::new(0.25, 1.25),
        },
        PricingEntry {
            prefixes: &["claude"],
            pricing: ModelPricing::new(3.00, 15.00),
        },

        // ── Google / Gemini ───────────────────────────────────────
        PricingEntry {
            prefixes: &["gemini-2.5-pro"],
            pricing: ModelPricing::new(1.25, 10.00),
        },
        PricingEntry {
            prefixes: &["gemini-2.5-flash"],
            pricing: ModelPricing::new(0.15, 0.60),
        },
        PricingEntry {
            prefixes: &["gemini-2.0-flash"],
            pricing: ModelPricing::new(0.10, 0.40),
        },
        PricingEntry {
            prefixes: &["gemini-1.5-pro"],
            pricing: ModelPricing::new(1.25, 5.00),
        },
        PricingEntry {
            prefixes: &["gemini-1.5-flash"],
            pricing: ModelPricing::new(0.075, 0.30),
        },
        PricingEntry {
            prefixes: &["gemini"],
            pricing: ModelPricing::new(0.10, 0.40),
        },

        // ── DeepSeek ──────────────────────────────────────────────
        PricingEntry {
            prefixes: &["deepseek-r1"],
            pricing: ModelPricing::new(0.55, 2.19),
        },
        PricingEntry {
            prefixes: &["deepseek-chat", "deepseek-v3"],
            pricing: ModelPricing::new(0.14, 0.28),
        },
        PricingEntry {
            prefixes: &["deepseek"],
            pricing: ModelPricing::new(0.14, 0.28),
        },

        // ── Meta Llama (via various providers) ────────────────────
        PricingEntry {
            prefixes: &["llama-4-maverick"],
            pricing: ModelPricing::new(0.20, 0.60),
        },
        PricingEntry {
            prefixes: &["llama-4-scout"],
            pricing: ModelPricing::new(0.10, 0.30),
        },
        PricingEntry {
            prefixes: &["llama-3.3-70b"],
            pricing: ModelPricing::new(0.59, 0.79),
        },
        PricingEntry {
            prefixes: &["llama-3.1-405b"],
            pricing: ModelPricing::new(2.00, 8.00),
        },
        PricingEntry {
            prefixes: &["llama-3.1-70b"],
            pricing: ModelPricing::new(0.59, 0.79),
        },
        PricingEntry {
            prefixes: &["llama-3.1-8b"],
            pricing: ModelPricing::new(0.06, 0.06),
        },
        PricingEntry {
            prefixes: &["llama-3-70b"],
            pricing: ModelPricing::new(0.59, 0.79),
        },
        PricingEntry {
            prefixes: &["llama-3-8b"],
            pricing: ModelPricing::new(0.05, 0.05),
        },

        // ── Mistral ───────────────────────────────────────────────
        PricingEntry {
            prefixes: &["mistral-large"],
            pricing: ModelPricing::new(2.00, 6.00),
        },
        PricingEntry {
            prefixes: &["mistral-medium"],
            pricing: ModelPricing::new(2.70, 8.10),
        },
        PricingEntry {
            prefixes: &["mistral-small"],
            pricing: ModelPricing::new(0.50, 1.50),
        },
        PricingEntry {
            prefixes: &["codestral"],
            pricing: ModelPricing::new(0.30, 0.90),
        },
        PricingEntry {
            prefixes: &["mistral"],
            pricing: ModelPricing::new(2.00, 6.00),
        },

        // ── Qwen ──────────────────────────────────────────────────
        PricingEntry {
            prefixes: &["qwen3-235b-a22b"],
            pricing: ModelPricing::new(0.14, 0.42),
        },
        PricingEntry {
            prefixes: &["qwen3-32b"],
            pricing: ModelPricing::new(0.02, 0.06),
        },
        PricingEntry {
            prefixes: &["qwen3-coder"],
            pricing: ModelPricing::new(0.14, 0.42),
        },
        PricingEntry {
            prefixes: &["qwen2.5-coder-32b", "qwen-2.5-coder-32b"],
            pricing: ModelPricing::new(0.14, 0.42),
        },
        PricingEntry {
            prefixes: &["qwen"],
            pricing: ModelPricing::new(0.14, 0.42),
        },

        // ── MiniMax ───────────────────────────────────────────────
        PricingEntry {
            prefixes: &["minimax"],
            pricing: ModelPricing::new(0.11, 0.11),
        },

        // ── Kimi / Moonshot ───────────────────────────────────────
        PricingEntry {
            prefixes: &["moonshot", "kimi"],
            pricing: ModelPricing::new(1.00, 2.00),
        },

        // ── GLM / ZAI ─────────────────────────────────────────────
        PricingEntry {
            prefixes: &["glm"],
            pricing: ModelPricing::new(0.50, 0.50),
        },
    ]
});

/// Look up pricing for the given model string.
///
/// The model string is matched case-insensitively against known model
/// prefixes. Returns `None` for unrecognized models.
pub fn lookup(model: &str) -> Option<ModelPricing> {
    let lower = model.to_ascii_lowercase();
    for entry in PRICING_TABLE.iter() {
        for prefix in entry.prefixes {
            if lower.contains(prefix) {
                return Some(entry.pricing);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_pricing_cost_calculation() {
        let pricing = ModelPricing::new(2.50, 10.00);
        // 1M input tokens → $2.50
        assert!((pricing.cost(1_000_000, 0) - 2.50).abs() < 1e-9);
        // 1M output tokens → $10.00
        assert!((pricing.cost(0, 1_000_000) - 10.00).abs() < 1e-9);
        // 100k input + 50k output
        let cost = pricing.cost(100_000, 50_000);
        assert!((cost - 0.75).abs() < 1e-9);
    }

    #[test]
    fn model_pricing_zero_tokens() {
        let pricing = ModelPricing::new(2.50, 10.00);
        assert!((pricing.cost(0, 0) - 0.0).abs() < 1e-9);
    }

    #[test]
    fn lookup_openai_gpt4o() {
        let pricing = lookup("gpt-4o").unwrap();
        assert!((pricing.input_per_million - 2.50).abs() < 1e-9);
        assert!((pricing.output_per_million - 10.00).abs() < 1e-9);
    }

    #[test]
    fn lookup_openai_gpt4o_mini() {
        let pricing = lookup("gpt-4o-mini").unwrap();
        assert!((pricing.input_per_million - 0.15).abs() < 1e-9);
    }

    #[test]
    fn lookup_anthropic_claude_sonnet_4() {
        let pricing = lookup("claude-sonnet-4-20250514").unwrap();
        assert!((pricing.input_per_million - 3.00).abs() < 1e-9);
        assert!((pricing.output_per_million - 15.00).abs() < 1e-9);
    }

    #[test]
    fn lookup_anthropic_claude_35_sonnet() {
        let pricing = lookup("claude-3-5-sonnet-20241022").unwrap();
        assert!((pricing.input_per_million - 3.00).abs() < 1e-9);
    }

    #[test]
    fn lookup_google_gemini_25_pro() {
        let pricing = lookup("gemini-2.5-pro-preview-05-06").unwrap();
        assert!((pricing.input_per_million - 1.25).abs() < 1e-9);
        assert!((pricing.output_per_million - 10.00).abs() < 1e-9);
    }

    #[test]
    fn lookup_deepseek_chat() {
        let pricing = lookup("deepseek-chat").unwrap();
        assert!((pricing.input_per_million - 0.14).abs() < 1e-9);
        assert!((pricing.output_per_million - 0.28).abs() < 1e-9);
    }

    #[test]
    fn lookup_with_provider_prefix() {
        // Provider prefixes (openrouter/, etc.) should be stripped implicitly by contains()
        let pricing = lookup("openrouter/anthropic/claude-sonnet-4").unwrap();
        assert!((pricing.input_per_million - 3.00).abs() < 1e-9);
    }

    #[test]
    fn lookup_case_insensitive() {
        let pricing = lookup("GPT-4O").unwrap();
        assert!((pricing.input_per_million - 2.50).abs() < 1e-9);
    }

    #[test]
    fn lookup_unknown_model_returns_none() {
        assert!(lookup("totally-unknown-model-xyz").is_none());
    }

    #[test]
    fn lookup_empty_string_returns_none() {
        assert!(lookup("").is_none());
    }

    #[test]
    fn lookup_mistral_large() {
        let pricing = lookup("mistral-large-latest").unwrap();
        assert!((pricing.input_per_million - 2.00).abs() < 1e-9);
        assert!((pricing.output_per_million - 6.00).abs() < 1e-9);
    }

    #[test]
    fn lookup_llama_3_3_70b() {
        let pricing = lookup("llama-3.3-70b-versatile").unwrap();
        assert!((pricing.input_per_million - 0.59).abs() < 1e-9);
    }

    #[test]
    fn lookup_qwen_coder() {
        let pricing = lookup("qwen2.5-coder-32b-instruct").unwrap();
        assert!((pricing.input_per_million - 0.14).abs() < 1e-9);
    }
}
