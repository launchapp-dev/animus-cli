use super::*;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(super) struct CoreState {
    pub(super) daemon_status: DaemonStatus,
    #[serde(alias = "daemon_max_agents")]
    pub(super) daemon_pool_size: Option<usize>,
    pub(super) runner_pid: Option<u32>,
    pub(super) logs: Vec<LogEntry>,
    pub(super) active_project_id: Option<String>,
    pub(super) projects: HashMap<String, OrchestratorProject>,
    #[serde(default, skip_serializing)]
    pub(super) tasks: HashMap<String, OrchestratorTask>,
    #[serde(skip)]
    pub(super) workflows: HashMap<String, OrchestratorWorkflow>,
    #[serde(default)]
    pub(super) vision: Option<VisionDocument>,
    #[serde(default, skip_serializing)]
    pub(super) requirements: HashMap<String, RequirementItem>,
    #[serde(default)]
    pub(super) architecture: ArchitectureGraph,
    #[serde(default)]
    pub(super) active_process_count: Option<usize>,
    #[serde(skip)]
    pub(super) dirty_tasks: HashSet<String>,
    #[serde(skip)]
    pub(super) dirty_requirements: HashSet<String>,
    #[serde(skip)]
    pub(super) all_tasks_dirty: bool,
    #[serde(skip)]
    pub(super) all_requirements_dirty: bool,
}

impl CoreState {
    pub(super) fn default_with_stopped() -> Self {
        Self { daemon_status: DaemonStatus::Stopped, ..Self::default() }
    }
}

fn normalize_legacy_task(task: &mut serde_json::Value) {
    let Some(task_obj) = task.as_object_mut() else {
        return;
    };

    if !task_obj.contains_key("assignee") || task_obj["assignee"].is_null() {
        task_obj.insert("assignee".to_string(), serde_json::json!({ "type": "unassigned" }));
    } else if let Some(user_id) = task_obj["assignee"].as_str() {
        task_obj.insert("assignee".to_string(), serde_json::json!({ "type": "human", "user_id": user_id }));
    }

    if let Some(status) = task_obj.get("status").and_then(|value| value.as_str()) {
        let normalized = match status {
            "todo" => Some("backlog"),
            "in_progress" => Some("in-progress"),
            "on_hold" => Some("on-hold"),
            _ => None,
        };
        if let Some(normalized) = normalized {
            task_obj.insert("status".to_string(), serde_json::Value::String(normalized.to_string()));
        }
    }

    if let Some(metadata) = task_obj.get_mut("metadata").and_then(|value| value.as_object_mut()) {
        if !metadata.contains_key("started_at") {
            metadata.insert("started_at".to_string(), serde_json::Value::Null);
        }
        if !metadata.contains_key("completed_at") {
            metadata.insert("completed_at".to_string(), serde_json::Value::Null);
        }
        if !metadata.contains_key("version") {
            metadata.insert("version".to_string(), serde_json::Value::from(1_u32));
        }
    }
}

fn default_legacy_project_config(project_type: &str, tech_stack: Vec<serde_json::Value>) -> serde_json::Value {
    let allowed_models: Vec<String> =
        protocol::default_model_specs().into_iter().map(|(model_id, _tool)| model_id).collect();
    let default_model = protocol::default_model_for_tool("claude").unwrap_or("sonnet").to_string();

    serde_json::json!({
        "project_type": project_type,
        "tech_stack": tech_stack,
        "auto_commit": true,
        "auto_push": false,
        "default_branch": "main",
        "model_preferences": {
            "allowed_models": allowed_models,
            "default_model": default_model,
            "phase_overrides": {}
        },
        "concurrency_limits": {
            "max_workflows": 3,
            "max_agents": 10
        },
        "mcp_port": 3101
    })
}

fn normalize_legacy_project(project: &mut serde_json::Value) {
    let Some(project_obj) = project.as_object_mut() else {
        return;
    };

    if !project_obj.contains_key("config") || project_obj["config"].is_null() {
        let project_type = project_obj.get("project_type").and_then(|value| value.as_str()).unwrap_or("other");
        let tech_stack = project_obj.get("tech_stack").and_then(|value| value.as_array()).cloned().unwrap_or_default();

        project_obj.insert("config".to_string(), default_legacy_project_config(project_type, tech_stack));
    }

    if !project_obj.contains_key("metadata") || project_obj["metadata"].is_null() {
        let description =
            project_obj.get("description").and_then(|value| value.as_str()).map(|value| value.to_string());

        project_obj.insert(
            "metadata".to_string(),
            serde_json::json!({
                "problem_statement": null,
                "target_users": [],
                "goals": [],
                "description": description,
                "custom": {}
            }),
        );
    }
}

fn deserialize_core_state(contents: &str) -> Result<CoreState> {
    if let Ok(state) = serde_json::from_str::<CoreState>(contents) {
        return Ok(state);
    }

    let mut raw: serde_json::Value = serde_json::from_str(contents).context("core-state JSON is invalid")?;
    if let Some(projects) = raw.get_mut("projects").and_then(|value| value.as_object_mut()) {
        for project in projects.values_mut() {
            normalize_legacy_project(project);
        }
    }
    if let Some(tasks) = raw.get_mut("tasks").and_then(|value| value.as_object_mut()) {
        for task in tasks.values_mut() {
            normalize_legacy_task(task);
        }
    }

    serde_json::from_value::<CoreState>(raw).context("core-state JSON does not match expected schema")
}

pub(super) fn load_core_state(state_file: &Path) -> CoreState {
    if !state_file.exists() {
        return CoreState::default_with_stopped();
    }

    let Ok(contents) = std::fs::read_to_string(state_file) else {
        return CoreState::default_with_stopped();
    };

    deserialize_core_state(&contents).unwrap_or_else(|_| CoreState::default_with_stopped())
}

pub(super) fn load_core_state_for_mutation(state_file: &Path) -> Result<CoreState> {
    if !state_file.exists() {
        return Ok(CoreState::default_with_stopped());
    }

    let contents = std::fs::read_to_string(state_file)
        .with_context(|| format!("failed to read core-state at {}", state_file.display()))?;
    deserialize_core_state(&contents).with_context(|| {
        format!("failed to parse core-state at {}; refusing mutation to avoid data loss", state_file.display())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mutation_loader_returns_default_when_state_file_is_missing() {
        let temp = tempfile::tempdir().expect("tempdir");
        let missing_path = temp.path().join("core-state.json");

        let loaded = load_core_state_for_mutation(&missing_path).expect("load default state");
        assert_eq!(loaded.daemon_status, DaemonStatus::Stopped);
        assert!(loaded.projects.is_empty());
        assert!(loaded.tasks.is_empty());
        assert!(loaded.requirements.is_empty());
    }

    #[test]
    fn mutation_loader_rejects_invalid_core_state_json() {
        let temp = tempfile::tempdir().expect("tempdir");
        let state_path = temp.path().join("core-state.json");
        std::fs::write(&state_path, "{not-valid-json").expect("write malformed state");

        let error = load_core_state_for_mutation(&state_path).expect_err("invalid core-state should fail closed");
        let message = format!("{error:#}");
        assert!(message.contains("refusing mutation to avoid data loss"));
    }
}
