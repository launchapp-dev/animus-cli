//! Hardcoded per-model USD rates used when an `AgentRunEvent::Metadata`
//! event does not include a `cost` field. The published rates table is
//! intentionally small and provider-agnostic — the aggregator prefers
//! the cost the provider reports.
//!
//! Source / refresh policy: update by hand against vendor pricing pages;
//! the values here are documented in `docs/reference/configuration.md`
//! under "Cost model rates".

/// USD per 1 million input + output + reasoning tokens. The combined
/// flat rate is intentionally simple — finer-grained input/output split
/// can be added when a customer asks.
#[derive(Debug, Clone, Copy)]
pub struct ModelRate {
    pub model_id_prefix: &'static str,
    pub usd_per_million_tokens: f64,
}

const RATES: &[ModelRate] = &[
    ModelRate { model_id_prefix: "claude-opus", usd_per_million_tokens: 30.0 },
    ModelRate { model_id_prefix: "claude-sonnet", usd_per_million_tokens: 6.0 },
    ModelRate { model_id_prefix: "claude-haiku", usd_per_million_tokens: 1.25 },
    ModelRate { model_id_prefix: "codex", usd_per_million_tokens: 5.0 },
    ModelRate { model_id_prefix: "o4", usd_per_million_tokens: 5.0 },
    ModelRate { model_id_prefix: "gpt-5", usd_per_million_tokens: 5.0 },
    ModelRate { model_id_prefix: "gemini-3", usd_per_million_tokens: 1.25 },
    ModelRate { model_id_prefix: "gemini-2", usd_per_million_tokens: 0.75 },
    ModelRate { model_id_prefix: "kimi", usd_per_million_tokens: 1.0 },
    ModelRate { model_id_prefix: "minimax", usd_per_million_tokens: 0.7 },
    ModelRate { model_id_prefix: "opencode", usd_per_million_tokens: 2.5 },
];

/// Estimate cost for a model id + total token count. Returns `None`
/// when no prefix matches (no estimate is more honest than a bad one).
pub fn estimate_cost_usd(model_id: &str, total_tokens: u64) -> Option<f64> {
    if total_tokens == 0 {
        return Some(0.0);
    }
    let lower = model_id.to_ascii_lowercase();
    for rate in RATES {
        if lower.starts_with(rate.model_id_prefix) {
            let tokens = total_tokens as f64;
            return Some((tokens / 1_000_000.0) * rate.usd_per_million_tokens);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_model_estimates_use_published_rate() {
        let cost = estimate_cost_usd("claude-sonnet-4-6", 500_000).expect("known prefix");
        assert!((cost - 3.0).abs() < 1e-9, "expected 3.0 USD for 500k sonnet tokens, got {cost}");
    }

    #[test]
    fn unknown_model_returns_none() {
        assert!(estimate_cost_usd("brand-new-vendor-x1", 1_000).is_none());
    }

    #[test]
    fn zero_tokens_is_zero_cost() {
        assert_eq!(estimate_cost_usd("claude-haiku-4", 0), Some(0.0));
    }

    #[test]
    fn case_insensitive_match() {
        assert!(estimate_cost_usd("CLAUDE-OPUS-4", 1_000_000).is_some());
    }
}
