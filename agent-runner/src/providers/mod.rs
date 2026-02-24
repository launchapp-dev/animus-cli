use protocol::{
    canonical_model_id, default_model_specs, required_api_keys_for_tool, tool_for_model_id,
    ModelAvailability, ModelId, ModelStatus, ModelStatusRequest, ModelStatusResponse,
};
use std::env;
use std::process::Command;

pub async fn check_model_status(req: ModelStatusRequest) -> ModelStatusResponse {
    let models_to_check = if req.models.is_empty() {
        default_model_specs()
            .into_iter()
            .map(|(model_id, _tool)| ModelId(model_id))
            .collect()
    } else {
        req.models
    };

    let statuses = models_to_check
        .into_iter()
        .map(check_single_model)
        .collect();

    ModelStatusResponse { statuses }
}

fn check_single_model(model: ModelId) -> ModelStatus {
    let canonical = canonical_model_id(&model.0);
    if canonical.is_empty() {
        return ModelStatus {
            model,
            availability: ModelAvailability::Error,
            details: Some("Unknown model".into()),
        };
    }

    let cli_name = tool_for_model_id(&canonical);
    let api_key_envs = required_api_keys_for_tool(cli_name);

    if !is_cli_available(cli_name) {
        return ModelStatus {
            model,
            availability: ModelAvailability::MissingCli,
            details: Some(format!("CLI '{}' not found on PATH", cli_name)),
        };
    }

    let has_any_key = api_key_envs.iter().any(|key| {
        env::var(key)
            .ok()
            .is_some_and(|value| !value.trim().is_empty())
    });

    if !has_any_key {
        return ModelStatus {
            model,
            availability: ModelAvailability::MissingApiKey,
            details: Some(format!(
                "Environment variable not set (expected one of: {})",
                api_key_envs.join(", ")
            )),
        };
    }

    ModelStatus {
        model,
        availability: ModelAvailability::Available,
        details: None,
    }
}

fn is_cli_available(cli_name: &str) -> bool {
    Command::new("which")
        .arg(cli_name)
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}
