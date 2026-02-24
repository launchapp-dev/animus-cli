use super::*;

#[derive(Debug, Clone, Deserialize, Default)]
pub(super) struct WorkflowPhaseRuntimeSettings {
    #[serde(default)]
    pub(super) tool: Option<String>,
    #[serde(default)]
    pub(super) model: Option<String>,
    #[serde(default)]
    pub(super) fallback_models: Vec<String>,
    #[serde(default)]
    pub(super) reasoning_effort: Option<String>,
    #[serde(default)]
    pub(super) web_search: Option<bool>,
    #[serde(default)]
    pub(super) timeout_secs: Option<u64>,
    #[serde(default)]
    pub(super) max_attempts: Option<usize>,
}

#[cfg(test)]
#[derive(Debug, Clone, Deserialize, Default)]
pub(super) struct WorkflowPipelineRuntimeRecord {
    pub(super) id: String,
    #[serde(default)]
    pub(super) phase_settings: std::collections::HashMap<String, WorkflowPhaseRuntimeSettings>,
}

#[cfg(test)]
#[derive(Debug, Clone, Deserialize, Default)]
pub(super) struct WorkflowRuntimeConfigLite {
    #[serde(default)]
    pub(super) default_pipeline_id: String,
    #[serde(default)]
    pub(super) pipelines: Vec<WorkflowPipelineRuntimeRecord>,
}

#[cfg(test)]
fn workflow_runtime_config_paths(project_root: &str) -> [PathBuf; 2] {
    [
        Path::new(project_root)
            .join(".ao")
            .join("state")
            .join("workflow-config.json"),
        Path::new(project_root)
            .join(".ao")
            .join("workflow-config.json"),
    ]
}

#[cfg(test)]
pub(super) fn load_workflow_runtime_config(project_root: &str) -> WorkflowRuntimeConfigLite {
    for path in workflow_runtime_config_paths(project_root) {
        if !path.exists() {
            continue;
        }

        let Ok(content) = std::fs::read_to_string(path) else {
            continue;
        };

        if let Ok(parsed) = serde_json::from_str::<WorkflowRuntimeConfigLite>(&content) {
            return parsed;
        }
    }

    WorkflowRuntimeConfigLite::default()
}

#[cfg(test)]
pub(super) fn resolve_phase_runtime_settings(
    config: &WorkflowRuntimeConfigLite,
    pipeline_id: Option<&str>,
    phase_id: &str,
) -> Option<WorkflowPhaseRuntimeSettings> {
    let requested_pipeline = pipeline_id
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .or_else(|| {
            let value = config.default_pipeline_id.trim();
            if value.is_empty() {
                None
            } else {
                Some(value)
            }
        })?;

    let pipeline = config
        .pipelines
        .iter()
        .find(|pipeline| pipeline.id.eq_ignore_ascii_case(requested_pipeline))?;

    pipeline
        .phase_settings
        .iter()
        .find(|(phase_key, _)| phase_key.eq_ignore_ascii_case(phase_id))
        .map(|(_, settings)| settings.clone())
}

pub(super) fn phase_timeout_secs() -> Option<u64> {
    if std::env::var("AO_PHASE_WAIT_FOR_COMPLETION")
        .ok()
        .map(|value| value.trim().to_ascii_lowercase())
        .map(|value| matches!(value.as_str(), "1" | "true" | "yes" | "on"))
        .unwrap_or(false)
    {
        return None;
    }

    match std::env::var("AO_PHASE_TIMEOUT_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
    {
        Some(0) => None,
        Some(value) => Some(value),
        // Default to no hard timeout; let the phase complete unless explicitly capped.
        None => None,
    }
}

fn parse_env_usize(key: &str) -> Option<usize> {
    std::env::var(key)
        .ok()
        .and_then(|value| value.trim().parse::<usize>().ok())
}

pub(super) fn phase_runner_attempts() -> usize {
    parse_env_usize("AO_PHASE_RUN_ATTEMPTS")
        .unwrap_or(3)
        .clamp(1, 10)
}

pub(super) fn bootstrap_max_requirements() -> usize {
    match parse_env_usize("AO_BOOTSTRAP_MAX_REQUIREMENTS") {
        Some(0) => usize::MAX,
        Some(value) => value,
        None => usize::MAX,
    }
}

pub(super) fn requirement_needs_refinement(requirement: &RequirementItem) -> bool {
    if requirement.status == RequirementStatus::Draft {
        return true;
    }

    if requirement.acceptance_criteria.is_empty() {
        return true;
    }

    !requirement.acceptance_criteria.iter().any(|criterion| {
        criterion
            .to_ascii_lowercase()
            .contains("automated test coverage")
    })
}

fn codex_web_search_enabled(web_search_override: Option<bool>) -> bool {
    web_search_override.unwrap_or_else(|| {
        std::env::var("AO_CODEX_WEB_SEARCH")
            .ok()
            .map(|value| value.trim().to_ascii_lowercase())
            .map(|value| !matches!(value.as_str(), "0" | "false" | "no" | "off"))
            .unwrap_or(true)
    })
}

fn env_codex_reasoning_effort_override() -> Option<String> {
    std::env::var("AO_CODEX_REASONING_EFFORT")
        .ok()
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| !value.is_empty())
}

fn codex_reasoning_effort(reasoning_override: Option<&str>) -> Option<String> {
    reasoning_override
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.to_ascii_lowercase())
        .or_else(env_codex_reasoning_effort_override)
}

fn codex_exec_insert_index(args: &[Value]) -> usize {
    args.iter()
        .position(|item| item.as_str().is_some_and(|value| value == "exec"))
        .unwrap_or(0)
}

pub(super) fn inject_codex_search_launch_flag(
    runtime_contract: &mut Value,
    tool_id: &str,
    web_search_override: Option<bool>,
) {
    if !tool_id.eq_ignore_ascii_case("codex") || !codex_web_search_enabled(web_search_override) {
        return;
    }

    if let Some(args) = runtime_contract
        .pointer_mut("/cli/launch/args")
        .and_then(Value::as_array_mut)
    {
        let has_search_flag = args
            .iter()
            .any(|item| item.as_str().is_some_and(|value| value == "--search"));
        if !has_search_flag {
            let insert_at = codex_exec_insert_index(args);
            args.insert(insert_at, Value::String("--search".to_string()));
        }
    }

    if let Some(capabilities) = runtime_contract
        .pointer_mut("/cli/capabilities")
        .and_then(Value::as_object_mut)
    {
        capabilities.insert("supports_web_search".to_string(), Value::Bool(true));
    }
}

pub(super) fn inject_codex_reasoning_effort(
    runtime_contract: &mut Value,
    tool_id: &str,
    reasoning_override: Option<&str>,
) {
    if !tool_id.eq_ignore_ascii_case("codex") {
        return;
    }
    let Some(effort) = codex_reasoning_effort(reasoning_override) else {
        return;
    };

    if let Some(args) = runtime_contract
        .pointer_mut("/cli/launch/args")
        .and_then(Value::as_array_mut)
    {
        let mut has_override = false;
        for window in args.windows(2) {
            let Some(flag) = window[0].as_str() else {
                continue;
            };
            let Some(value) = window[1].as_str() else {
                continue;
            };
            if flag == "-c" && value.starts_with("model_reasoning_effort=") {
                has_override = true;
                break;
            }
        }
        if !has_override {
            let insert_at = codex_exec_insert_index(args);
            args.insert(insert_at, Value::String("-c".to_string()));
            args.insert(
                insert_at + 1,
                Value::String(format!("model_reasoning_effort=\"{effort}\"")),
            );
        }
    }
}
