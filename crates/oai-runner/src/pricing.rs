struct ModelPricing {
    input_per_1m: f64,
    output_per_1m: f64,
}

impl ModelPricing {
    fn cost(&self, input_tokens: u64, output_tokens: u64) -> f64 {
        (input_tokens as f64 * self.input_per_1m + output_tokens as f64 * self.output_per_1m) / 1_000_000.0
    }
}

pub fn compute_cost(api_base: &str, model_id: &str, input_tokens: u64, output_tokens: u64) -> f64 {
    pricing_for(api_base, model_id).cost(input_tokens, output_tokens)
}

fn pricing_for(api_base: &str, model_id: &str) -> ModelPricing {
    let model = model_id.to_ascii_lowercase();

    if api_base.contains("api.openai.com") {
        return openai_pricing(&model);
    }
    if api_base.contains("api.deepseek.com") {
        return deepseek_pricing(&model);
    }
    if api_base.contains("api.groq.com") {
        return groq_pricing(&model);
    }
    if api_base.contains("api.mistral.ai") {
        return mistral_pricing(&model);
    }
    if api_base.contains("api.together.xyz") {
        return ModelPricing { input_per_1m: 0.90, output_per_1m: 0.90 };
    }
    if api_base.contains("api.fireworks.ai") {
        return ModelPricing { input_per_1m: 0.90, output_per_1m: 0.90 };
    }
    if api_base.contains("api.minimax.io") {
        return ModelPricing { input_per_1m: 0.30, output_per_1m: 1.10 };
    }
    if api_base.contains("api.z.ai") {
        return ModelPricing { input_per_1m: 0.50, output_per_1m: 2.00 };
    }
    if api_base.contains("api.kimi.com") || api_base.contains("api.moonshot.ai") {
        return kimi_pricing(&model);
    }
    ModelPricing { input_per_1m: 0.0, output_per_1m: 0.0 }
}

fn openai_pricing(model: &str) -> ModelPricing {
    if model.contains("gpt-4o-mini") {
        return ModelPricing { input_per_1m: 0.15, output_per_1m: 0.60 };
    }
    if model.contains("gpt-4o") {
        return ModelPricing { input_per_1m: 2.50, output_per_1m: 10.00 };
    }
    if model.contains("gpt-4-turbo") {
        return ModelPricing { input_per_1m: 10.00, output_per_1m: 30.00 };
    }
    if model.contains("gpt-4") {
        return ModelPricing { input_per_1m: 30.00, output_per_1m: 60.00 };
    }
    if model.contains("gpt-3.5-turbo") {
        return ModelPricing { input_per_1m: 0.50, output_per_1m: 1.50 };
    }
    if model.contains("o1-mini") {
        return ModelPricing { input_per_1m: 3.00, output_per_1m: 12.00 };
    }
    if model.contains("o3-mini") {
        return ModelPricing { input_per_1m: 1.10, output_per_1m: 4.40 };
    }
    if model.starts_with("o1") {
        return ModelPricing { input_per_1m: 15.00, output_per_1m: 60.00 };
    }
    if model.starts_with("o3") {
        return ModelPricing { input_per_1m: 10.00, output_per_1m: 40.00 };
    }
    ModelPricing { input_per_1m: 2.50, output_per_1m: 10.00 }
}

fn deepseek_pricing(model: &str) -> ModelPricing {
    if model.contains("reasoner") {
        return ModelPricing { input_per_1m: 0.55, output_per_1m: 2.19 };
    }
    ModelPricing { input_per_1m: 0.27, output_per_1m: 1.10 }
}

fn groq_pricing(model: &str) -> ModelPricing {
    if model.contains("8b") {
        return ModelPricing { input_per_1m: 0.05, output_per_1m: 0.08 };
    }
    if model.contains("70b") || model.contains("versatile") {
        return ModelPricing { input_per_1m: 0.59, output_per_1m: 0.79 };
    }
    if model.contains("mixtral") {
        return ModelPricing { input_per_1m: 0.24, output_per_1m: 0.24 };
    }
    if model.contains("gemma") {
        return ModelPricing { input_per_1m: 0.20, output_per_1m: 0.20 };
    }
    ModelPricing { input_per_1m: 0.59, output_per_1m: 0.79 }
}

fn mistral_pricing(model: &str) -> ModelPricing {
    if model.contains("large") {
        return ModelPricing { input_per_1m: 2.00, output_per_1m: 6.00 };
    }
    if model.contains("codestral") {
        return ModelPricing { input_per_1m: 0.30, output_per_1m: 0.90 };
    }
    ModelPricing { input_per_1m: 0.10, output_per_1m: 0.30 }
}

fn kimi_pricing(model: &str) -> ModelPricing {
    if model.contains("k1.5") || model.contains("k2") {
        return ModelPricing { input_per_1m: 2.00, output_per_1m: 8.00 };
    }
    ModelPricing { input_per_1m: 1.00, output_per_1m: 3.00 }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx_eq(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn openai_gpt4o_cost() {
        let cost = compute_cost("https://api.openai.com/v1", "gpt-4o", 1_000_000, 500_000);
        assert!(approx_eq(cost, 2.50 + 5.00), "expected $7.50, got ${}", cost);
    }

    #[test]
    fn openai_gpt4o_mini_cost() {
        let cost = compute_cost("https://api.openai.com/v1", "gpt-4o-mini", 1_000_000, 1_000_000);
        assert!(approx_eq(cost, 0.15 + 0.60), "expected $0.75, got ${}", cost);
    }

    #[test]
    fn deepseek_chat_cost() {
        let cost = compute_cost("https://api.deepseek.com/v1", "deepseek-chat", 2_000_000, 1_000_000);
        assert!(approx_eq(cost, 0.54 + 1.10), "expected $1.64, got ${}", cost);
    }

    #[test]
    fn deepseek_reasoner_cost() {
        let cost = compute_cost("https://api.deepseek.com/v1", "deepseek-reasoner", 1_000_000, 1_000_000);
        assert!(approx_eq(cost, 0.55 + 2.19), "expected $2.74, got ${}", cost);
    }

    #[test]
    fn groq_llama_70b_cost() {
        let cost = compute_cost("https://api.groq.com/openai/v1", "llama-3.3-70b-versatile", 1_000_000, 1_000_000);
        assert!(approx_eq(cost, 0.59 + 0.79), "expected $1.38, got ${}", cost);
    }

    #[test]
    fn groq_llama_8b_cost() {
        let cost = compute_cost("https://api.groq.com/openai/v1", "llama-3.1-8b-instant", 1_000_000, 1_000_000);
        assert!(approx_eq(cost, 0.05 + 0.08), "expected $0.13, got ${}", cost);
    }

    #[test]
    fn mistral_large_cost() {
        let cost = compute_cost("https://api.mistral.ai/v1", "mistral-large-latest", 1_000_000, 1_000_000);
        assert!(approx_eq(cost, 2.00 + 6.00), "expected $8.00, got ${}", cost);
    }

    #[test]
    fn together_default_cost() {
        let cost = compute_cost("https://api.together.xyz/v1", "meta-llama/Llama-3-70b", 1_000_000, 1_000_000);
        assert!(approx_eq(cost, 0.90 + 0.90), "expected $1.80, got ${}", cost);
    }

    #[test]
    fn fireworks_default_cost() {
        let cost = compute_cost("https://api.fireworks.ai/inference/v1", "llama-v3p3-70b-instruct", 1_000_000, 1_000_000);
        assert!(approx_eq(cost, 0.90 + 0.90), "expected $1.80, got ${}", cost);
    }

    #[test]
    fn unknown_provider_zero_cost() {
        let cost = compute_cost("https://openrouter.ai/api/v1", "anthropic/claude-3-5-sonnet", 1_000_000, 1_000_000);
        assert!(approx_eq(cost, 0.0), "expected $0.0, got ${}", cost);
    }

    #[test]
    fn zero_tokens_zero_cost() {
        let cost = compute_cost("https://api.openai.com/v1", "gpt-4o", 0, 0);
        assert!(approx_eq(cost, 0.0));
    }
}
