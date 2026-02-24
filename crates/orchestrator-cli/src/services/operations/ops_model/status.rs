use protocol::{
    canonical_model_id, default_model_specs as protocol_default_model_specs, normalize_tool_id,
    required_api_keys_for_tool as protocol_required_api_keys_for_tool, tool_for_model_id,
};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::process::Command as ProcessCommand;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct ModelStatusDtoCli {
    pub(super) model_id: String,
    pub(super) cli_tool: String,
    pub(super) availability: String,
    #[serde(default)]
    pub(super) details: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct ModelAvailabilitySummaryCli {
    pub(super) statuses: Vec<ModelStatusDtoCli>,
    pub(super) all_available: bool,
    pub(super) summary: String,
}

pub(super) fn parse_model_specs(specs: &[String]) -> Vec<(String, String)> {
    if specs.is_empty() {
        return default_model_specs();
    }
    specs
        .iter()
        .map(|spec| {
            let mut parts = spec.splitn(2, ':');
            let model_id = canonical_model_id(parts.next().unwrap_or_default());
            let cli_tool = parts
                .next()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(normalize_tool_id)
                .unwrap_or_else(|| tool_for_model_id(&model_id).to_string());
            (model_id, cli_tool)
        })
        .filter(|(model_id, cli_tool)| !model_id.is_empty() && !cli_tool.is_empty())
        .collect()
}

pub(super) fn default_model_specs() -> Vec<(String, String)> {
    protocol_default_model_specs()
}

fn lookup_binary_in_path(binary_name: &str) -> Option<PathBuf> {
    #[cfg(unix)]
    {
        let output = ProcessCommand::new("which")
            .arg(binary_name)
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if path.is_empty() {
            None
        } else {
            Some(PathBuf::from(path))
        }
    }

    #[cfg(windows)]
    {
        let output = ProcessCommand::new("where")
            .arg(binary_name)
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let first = String::from_utf8_lossy(&output.stdout)
            .lines()
            .next()
            .map(str::trim)
            .unwrap_or_default()
            .to_string();
        if first.is_empty() {
            None
        } else {
            Some(PathBuf::from(first))
        }
    }

    #[cfg(not(any(unix, windows)))]
    {
        let _ = binary_name;
        None
    }
}

fn required_api_keys_for_tool(cli_tool: &str) -> Vec<&'static str> {
    protocol_required_api_keys_for_tool(cli_tool).to_vec()
}

pub(super) fn evaluate_model_status(model_id: &str, cli_tool: &str) -> ModelStatusDtoCli {
    if lookup_binary_in_path(cli_tool).is_none() {
        return ModelStatusDtoCli {
            model_id: model_id.to_string(),
            cli_tool: cli_tool.to_string(),
            availability: "missing_cli".to_string(),
            details: Some(format!("{cli_tool} binary not found in PATH")),
        };
    }

    let api_keys = required_api_keys_for_tool(cli_tool);
    if !api_keys.is_empty()
        && !api_keys.iter().any(|key| {
            std::env::var(key)
                .ok()
                .is_some_and(|value| !value.trim().is_empty())
        })
    {
        return ModelStatusDtoCli {
            model_id: model_id.to_string(),
            cli_tool: cli_tool.to_string(),
            availability: "missing_api_key".to_string(),
            details: Some(format!("missing one of: {}", api_keys.join(", "))),
        };
    }

    ModelStatusDtoCli {
        model_id: model_id.to_string(),
        cli_tool: cli_tool.to_string(),
        availability: "available".to_string(),
        details: None,
    }
}

pub(super) fn summarize_model_statuses(
    statuses: &[ModelStatusDtoCli],
) -> ModelAvailabilitySummaryCli {
    let all_available = statuses
        .iter()
        .all(|status| status.availability == "available");
    let available = statuses
        .iter()
        .filter(|status| status.availability == "available")
        .count();
    let summary = format!("{available}/{} models available", statuses.len());
    ModelAvailabilitySummaryCli {
        statuses: statuses.to_vec(),
        all_available,
        summary,
    }
}
