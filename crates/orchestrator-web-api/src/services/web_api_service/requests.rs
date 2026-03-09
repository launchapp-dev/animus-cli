use orchestrator_core::ProjectMetadata;
use serde::Deserialize;
use serde_json::Value;

use super::parsing::default_true_flag;

#[derive(Debug, Deserialize)]
pub(super) struct ProjectCreateRequest {
    pub(super) name: String,
    pub(super) path: String,
    #[serde(default)]
    pub(super) project_type: Option<String>,
    #[serde(default)]
    pub(super) description: Option<String>,
    #[serde(default)]
    pub(super) tech_stack: Vec<String>,
    #[serde(default)]
    pub(super) metadata: Option<ProjectMetadata>,
}

#[derive(Debug, Deserialize)]
pub(super) struct ProjectPatchRequest {
    #[serde(default)]
    pub(super) name: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct VisionRefineRequest {
    #[serde(default)]
    pub(super) focus: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct RequirementCreateRequest {
    #[serde(default)]
    pub(super) id: Option<String>,
    pub(super) title: String,
    #[serde(default)]
    pub(super) description: Option<String>,
    #[serde(default)]
    pub(super) body: Option<String>,
    #[serde(default)]
    pub(super) category: Option<String>,
    #[serde(default)]
    pub(super) requirement_type: Option<String>,
    #[serde(default)]
    pub(super) acceptance_criteria: Vec<String>,
    #[serde(default)]
    pub(super) priority: Option<String>,
    #[serde(default)]
    pub(super) status: Option<String>,
    #[serde(default)]
    pub(super) source: Option<String>,
    #[serde(default)]
    pub(super) tags: Vec<String>,
    #[serde(default)]
    pub(super) labels: Vec<String>,
    #[serde(default)]
    pub(super) area: Option<String>,
    #[serde(default)]
    pub(super) external_ref: Option<String>,
    #[serde(default)]
    pub(super) linked_epic_ids: Vec<String>,
    #[serde(default)]
    pub(super) linked_task_ids: Vec<String>,
    #[serde(default)]
    pub(super) relative_path: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
pub(super) struct RequirementPatchRequest {
    #[serde(default)]
    pub(super) title: Option<String>,
    #[serde(default)]
    pub(super) description: Option<String>,
    #[serde(default)]
    pub(super) body: Option<String>,
    #[serde(default)]
    pub(super) category: Option<String>,
    #[serde(default)]
    pub(super) requirement_type: Option<String>,
    #[serde(default)]
    pub(super) acceptance_criteria: Option<Vec<String>>,
    #[serde(default)]
    pub(super) priority: Option<String>,
    #[serde(default)]
    pub(super) status: Option<String>,
    #[serde(default)]
    pub(super) source: Option<String>,
    #[serde(default)]
    pub(super) tags: Option<Vec<String>>,
    #[serde(default)]
    pub(super) labels: Option<Vec<String>>,
    #[serde(default)]
    pub(super) area: Option<String>,
    #[serde(default)]
    pub(super) external_ref: Option<String>,
    #[serde(default)]
    pub(super) linked_epic_ids: Option<Vec<String>>,
    #[serde(default)]
    pub(super) linked_task_ids: Option<Vec<String>>,
    #[serde(default)]
    pub(super) relative_path: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct RequirementsDraftRequest {
    #[serde(default = "default_true_flag")]
    pub(super) include_codebase_scan: bool,
    #[serde(default = "default_true_flag")]
    pub(super) append_only: bool,
    #[serde(default)]
    pub(super) max_requirements: Option<usize>,
}

impl Default for RequirementsDraftRequest {
    fn default() -> Self {
        Self {
            include_codebase_scan: true,
            append_only: true,
            max_requirements: None,
        }
    }
}

#[derive(Debug, Deserialize, Default)]
pub(super) struct RequirementsRefineRequest {
    #[serde(default)]
    pub(super) requirement_ids: Vec<String>,
    #[serde(default)]
    pub(super) focus: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct TaskCreateRequest {
    pub(super) title: String,
    #[serde(default)]
    pub(super) description: String,
    #[serde(default)]
    pub(super) task_type: Option<String>,
    #[serde(default)]
    pub(super) priority: Option<String>,
    #[serde(default)]
    pub(super) created_by: Option<String>,
    #[serde(default)]
    pub(super) tags: Vec<String>,
    #[serde(default)]
    pub(super) labels: Vec<String>,
    #[serde(default)]
    pub(super) area: Option<String>,
    #[serde(default)]
    pub(super) external_ref: Option<String>,
    #[serde(default)]
    pub(super) epic_id: Option<String>,
    #[serde(default)]
    pub(super) parent_task_id: Option<String>,
    #[serde(default)]
    pub(super) linked_requirements: Vec<String>,
    #[serde(default)]
    pub(super) linked_architecture_entities: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct TaskPatchRequest {
    #[serde(default)]
    pub(super) title: Option<String>,
    #[serde(default)]
    pub(super) description: Option<String>,
    #[serde(default)]
    pub(super) priority: Option<String>,
    #[serde(default)]
    pub(super) status: Option<String>,
    #[serde(default)]
    pub(super) assignee: Option<String>,
    #[serde(default)]
    pub(super) tags: Option<Vec<String>>,
    #[serde(default)]
    pub(super) labels: Option<Vec<String>>,
    #[serde(default)]
    pub(super) area: Option<String>,
    #[serde(default)]
    pub(super) external_ref: Option<String>,
    #[serde(default)]
    pub(super) epic_id: Option<String>,
    #[serde(default)]
    pub(super) parent_task_id: Option<String>,
    #[serde(default)]
    pub(super) linked_requirements: Option<Vec<String>>,
    #[serde(default)]
    pub(super) updated_by: Option<String>,
    #[serde(default)]
    pub(super) deadline: Option<String>,
    #[serde(default)]
    pub(super) linked_architecture_entities: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
pub(super) struct TaskStatusRequest {
    pub(super) status: String,
}

#[derive(Debug, Deserialize)]
pub(super) struct TaskAssignAgentRequest {
    pub(super) role: String,
    #[serde(default)]
    pub(super) model: Option<String>,
    #[serde(default)]
    pub(super) updated_by: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct TaskAssignHumanRequest {
    pub(super) user_id: String,
    #[serde(default)]
    pub(super) updated_by: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct TaskChecklistAddRequest {
    pub(super) description: String,
    #[serde(default)]
    pub(super) updated_by: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct TaskChecklistUpdateRequest {
    pub(super) completed: bool,
    #[serde(default)]
    pub(super) updated_by: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct TaskDependencyAddRequest {
    pub(super) dependency_id: String,
    pub(super) dependency_type: String,
    #[serde(default)]
    pub(super) updated_by: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct TaskDependencyRemoveRequest {
    #[serde(default)]
    pub(super) updated_by: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct WorkflowRunRequest {
    #[serde(default)]
    pub(super) task_id: Option<String>,
    #[serde(default)]
    pub(super) requirement_id: Option<String>,
    #[serde(default)]
    pub(super) title: Option<String>,
    #[serde(default)]
    pub(super) description: Option<String>,
    #[serde(default)]
    pub(super) workflow_ref: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct ReviewHandoffRequest {
    #[serde(default)]
    pub(super) handoff_id: Option<String>,
    pub(super) run_id: String,
    pub(super) target_role: String,
    pub(super) question: String,
    #[serde(default)]
    pub(super) context: Value,
}

#[derive(Debug, Deserialize)]
pub(super) struct QueueReorderRequest {
    #[serde(default)]
    #[serde(alias = "task_ids")]
    pub(super) subject_ids: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct QueueHoldRequest {}

#[derive(Debug, Deserialize)]
pub(super) struct QueueReleaseRequest {
    #[serde(default)]
    pub(super) reason: Option<String>,
}
