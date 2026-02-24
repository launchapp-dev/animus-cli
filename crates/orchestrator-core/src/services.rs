use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use chrono::Utc;
use protocol::{RunnerStatusRequest, RunnerStatusResponse};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::RwLock;
use tokio::time::sleep;
use uuid::Uuid;

use crate::types::{
    AgentHandoffRequestInput, AgentHandoffResult, Assignee, ChecklistItem, CheckpointReason,
    CodebaseInsight, Complexity, ComplexityAssessment, ComplexityTier, DaemonHealth, DaemonStatus,
    DependencyType, LogEntry, LogLevel, OrchestratorProject, OrchestratorTask,
    OrchestratorWorkflow, Priority, ProjectConfig, ProjectCreateInput, ProjectType,
    RequirementItem, RequirementStatus, RequirementsDraftInput, RequirementsDraftResult,
    RequirementsExecutionInput, RequirementsExecutionResult, RequirementsRefineInput, RiskLevel,
    Scope, TaskCreateInput, TaskDensity, TaskDependency, TaskFilter, TaskMetadata, TaskStatistics,
    TaskStatus, TaskType, TaskUpdateInput, VisionDocument, VisionDraftInput, WorkflowMetadata,
    WorkflowRunInput,
};
use crate::workflow::{
    phase_plan_for_pipeline_id, ResumeConfig, WorkflowLifecycleExecutor, WorkflowStateManager,
    STANDARD_PIPELINE_ID, UI_UX_PIPELINE_ID,
};

mod daemon_impl;
mod planning_impl;
mod planning_shared;
mod planning_utils;
mod project_impl;
mod project_shared;
mod review_impl;
mod runner_helpers;
mod state_store;
mod task_impl;
mod task_shared;
mod workflow_impl;

use planning_utils::*;
use runner_helpers::*;
use state_store::{load_core_state, CoreState};
use task_shared::*;

#[async_trait]
pub trait DaemonServiceApi: Send + Sync {
    async fn start(&self) -> Result<()>;
    async fn stop(&self) -> Result<()>;
    async fn pause(&self) -> Result<()>;
    async fn resume(&self) -> Result<()>;
    async fn status(&self) -> Result<DaemonStatus>;
    async fn health(&self) -> Result<DaemonHealth>;
    async fn logs(&self, limit: Option<usize>) -> Result<Vec<LogEntry>>;
    async fn clear_logs(&self) -> Result<()>;
    async fn active_agents(&self) -> Result<usize>;
}

#[async_trait]
pub trait ProjectServiceApi: Send + Sync {
    async fn list(&self) -> Result<Vec<OrchestratorProject>>;
    async fn get(&self, id: &str) -> Result<OrchestratorProject>;
    async fn active(&self) -> Result<Option<OrchestratorProject>>;
    async fn create(&self, input: ProjectCreateInput) -> Result<OrchestratorProject>;
    async fn upsert(&self, project: OrchestratorProject) -> Result<OrchestratorProject>;
    async fn load(&self, id: &str) -> Result<OrchestratorProject>;
    async fn rename(&self, id: &str, new_name: &str) -> Result<OrchestratorProject>;
    async fn archive(&self, id: &str) -> Result<OrchestratorProject>;
    async fn remove(&self, id: &str) -> Result<()>;
}

#[async_trait]
pub trait TaskServiceApi: Send + Sync {
    async fn list(&self) -> Result<Vec<OrchestratorTask>>;
    async fn list_filtered(&self, filter: TaskFilter) -> Result<Vec<OrchestratorTask>>;
    async fn list_prioritized(&self) -> Result<Vec<OrchestratorTask>>;
    async fn next_task(&self) -> Result<Option<OrchestratorTask>>;
    async fn statistics(&self) -> Result<TaskStatistics>;
    async fn get(&self, id: &str) -> Result<OrchestratorTask>;
    async fn create(&self, input: TaskCreateInput) -> Result<OrchestratorTask>;
    async fn update(&self, id: &str, input: TaskUpdateInput) -> Result<OrchestratorTask>;
    async fn replace(&self, task: OrchestratorTask) -> Result<OrchestratorTask>;
    async fn delete(&self, id: &str) -> Result<()>;
    async fn assign(&self, id: &str, assignee: String) -> Result<OrchestratorTask>;
    async fn assign_agent(
        &self,
        id: &str,
        role: String,
        model: Option<String>,
        updated_by: String,
    ) -> Result<OrchestratorTask>;
    async fn assign_human(
        &self,
        id: &str,
        user_id: String,
        updated_by: String,
    ) -> Result<OrchestratorTask>;
    async fn set_status(&self, id: &str, status: TaskStatus) -> Result<OrchestratorTask>;
    async fn add_checklist_item(
        &self,
        id: &str,
        description: String,
        updated_by: String,
    ) -> Result<OrchestratorTask>;
    async fn update_checklist_item(
        &self,
        id: &str,
        item_id: &str,
        completed: bool,
        updated_by: String,
    ) -> Result<OrchestratorTask>;
    async fn add_dependency(
        &self,
        id: &str,
        dependency_id: &str,
        dependency_type: DependencyType,
        updated_by: String,
    ) -> Result<OrchestratorTask>;
    async fn remove_dependency(
        &self,
        id: &str,
        dependency_id: &str,
        updated_by: String,
    ) -> Result<OrchestratorTask>;
}

#[async_trait]
pub trait WorkflowServiceApi: Send + Sync {
    async fn list(&self) -> Result<Vec<OrchestratorWorkflow>>;
    async fn get(&self, id: &str) -> Result<OrchestratorWorkflow>;
    async fn decisions(&self, id: &str) -> Result<Vec<crate::types::WorkflowDecisionRecord>>;
    async fn list_checkpoints(&self, id: &str) -> Result<Vec<usize>>;
    async fn get_checkpoint(
        &self,
        id: &str,
        checkpoint_number: usize,
    ) -> Result<OrchestratorWorkflow>;
    async fn run(&self, input: WorkflowRunInput) -> Result<OrchestratorWorkflow>;
    async fn resume(&self, id: &str) -> Result<OrchestratorWorkflow>;
    async fn pause(&self, id: &str) -> Result<OrchestratorWorkflow>;
    async fn cancel(&self, id: &str) -> Result<OrchestratorWorkflow>;
    async fn request_research(&self, id: &str, reason: String) -> Result<OrchestratorWorkflow>;
    async fn complete_current_phase(&self, id: &str) -> Result<OrchestratorWorkflow>;
    async fn fail_current_phase(&self, id: &str, error: String) -> Result<OrchestratorWorkflow>;
}

#[async_trait]
pub trait PlanningServiceApi: Send + Sync {
    async fn draft_vision(&self, input: VisionDraftInput) -> Result<VisionDocument>;
    async fn get_vision(&self) -> Result<Option<VisionDocument>>;
    async fn draft_requirements(
        &self,
        input: RequirementsDraftInput,
    ) -> Result<RequirementsDraftResult>;
    async fn list_requirements(&self) -> Result<Vec<RequirementItem>>;
    async fn get_requirement(&self, id: &str) -> Result<RequirementItem>;
    async fn refine_requirements(
        &self,
        input: RequirementsRefineInput,
    ) -> Result<Vec<RequirementItem>>;
    async fn upsert_requirement(&self, requirement: RequirementItem) -> Result<RequirementItem>;
    async fn delete_requirement(&self, id: &str) -> Result<()>;
    async fn execute_requirements(
        &self,
        input: RequirementsExecutionInput,
    ) -> Result<RequirementsExecutionResult>;
}

#[async_trait]
pub trait ReviewServiceApi: Send + Sync {
    async fn request_handoff(&self, input: AgentHandoffRequestInput) -> Result<AgentHandoffResult>;
}

pub trait ServiceHub: Send + Sync {
    fn daemon(&self) -> Arc<dyn DaemonServiceApi>;
    fn projects(&self) -> Arc<dyn ProjectServiceApi>;
    fn tasks(&self) -> Arc<dyn TaskServiceApi>;
    fn workflows(&self) -> Arc<dyn WorkflowServiceApi>;
    fn planning(&self) -> Arc<dyn PlanningServiceApi>;
    fn review(&self) -> Arc<dyn ReviewServiceApi>;
}

#[derive(Clone)]
pub struct InMemoryServiceHub {
    state: Arc<RwLock<CoreState>>,
}

impl Default for InMemoryServiceHub {
    fn default() -> Self {
        Self {
            state: Arc::new(RwLock::new(CoreState::default_with_stopped())),
        }
    }
}

impl InMemoryServiceHub {
    pub fn new() -> Self {
        Self::default()
    }

    fn log(&self, level: LogLevel, message: String) {
        let state = self.state.clone();
        tokio::spawn(async move {
            let mut lock = state.write().await;
            lock.logs.push(LogEntry {
                timestamp: Utc::now(),
                level,
                message,
            });
        });
    }
}

#[derive(Clone)]
pub struct FileServiceHub {
    state: Arc<RwLock<CoreState>>,
    state_file: PathBuf,
    project_root: PathBuf,
}

impl FileServiceHub {
    pub fn new(project_root: impl AsRef<Path>) -> Result<Self> {
        let project_root = project_root.as_ref().to_path_buf();
        Self::bootstrap_project_base_configs(&project_root)?;
        let state_file = project_root.join(".ao").join("core-state.json");

        let mut state = load_core_state(&state_file);

        let workflow_manager = WorkflowStateManager::new(&project_root);
        if let Ok(workflows) = workflow_manager.list() {
            state.workflows = workflows
                .into_iter()
                .map(|workflow| (workflow.id.clone(), workflow))
                .collect();
        }

        Self::persist_snapshot(&state_file, &state)?;

        let hub = Self {
            state: Arc::new(RwLock::new(state)),
            state_file,
            project_root,
        };
        Ok(hub)
    }

    fn docs_dir_for_state_file(path: &Path) -> Option<PathBuf> {
        path.parent().map(|ao_dir| ao_dir.join("docs"))
    }

    fn ao_dir_for_state_file(path: &Path) -> Option<PathBuf> {
        path.parent().map(Path::to_path_buf)
    }

    fn sanitize_relative_json_path(raw: Option<&str>, fallback_file_name: &str) -> PathBuf {
        let fallback = PathBuf::from(fallback_file_name);
        let Some(raw) = raw.map(str::trim).filter(|value| !value.is_empty()) else {
            return fallback;
        };

        let candidate = PathBuf::from(raw);
        if candidate.is_absolute() {
            return fallback;
        }

        let mut safe = PathBuf::new();
        for component in candidate.components() {
            match component {
                Component::Normal(segment) => safe.push(segment),
                Component::CurDir => continue,
                Component::RootDir | Component::ParentDir | Component::Prefix(_) => {
                    return fallback;
                }
            }
        }

        if safe.as_os_str().is_empty() {
            return fallback;
        }

        if safe.extension().and_then(|ext| ext.to_str()) != Some("json") {
            safe.set_extension("json");
        }

        safe
    }

    fn tauri_requirement_status(status: RequirementStatus) -> &'static str {
        match status {
            RequirementStatus::Draft | RequirementStatus::Refined | RequirementStatus::Planned => {
                "draft"
            }
            RequirementStatus::InProgress => "em-review",
            RequirementStatus::Done | RequirementStatus::Implemented => "implemented",
            RequirementStatus::PoReview => "po-review",
            RequirementStatus::EmReview => "em-review",
            RequirementStatus::NeedsRework => "needs-rework",
            RequirementStatus::Approved => "approved",
            RequirementStatus::Deprecated => "deprecated",
        }
    }

    fn tauri_compatible_requirement_payload(requirement: &RequirementItem) -> serde_json::Value {
        let mut tasks = requirement.links.tasks.clone();
        tasks.extend(requirement.linked_task_ids.clone());
        tasks.sort();
        tasks.dedup();

        serde_json::json!({
            "id": requirement.id,
            "title": requirement.title,
            "description": if requirement.description.trim().is_empty() { serde_json::Value::Null } else { serde_json::Value::String(requirement.description.clone()) },
            "legacy_id": requirement.legacy_id,
            "category": requirement.category,
            "type": requirement.requirement_type,
            "priority": requirement.priority,
            "status": Self::tauri_requirement_status(requirement.status),
            "acceptance_criteria": requirement.acceptance_criteria,
            "tags": requirement.tags,
            "links": {
                "tasks": tasks,
                "workflows": requirement.links.workflows,
                "tests": requirement.links.tests,
                "mockups": requirement.links.mockups,
                "flows": requirement.links.flows,
                "related_requirements": requirement.links.related_requirements,
            },
            "comments": requirement.comments,
            "created_at": requirement.created_at,
            "updated_at": requirement.updated_at,
        })
    }

    fn write_requirement_files(path: &Path, snapshot: &CoreState) -> Result<()> {
        let Some(ao_dir) = Self::ao_dir_for_state_file(path) else {
            return Ok(());
        };
        let requirements_dir = ao_dir.join("requirements");
        std::fs::create_dir_all(&requirements_dir)?;

        let mut requirements: Vec<_> = snapshot.requirements.values().cloned().collect();
        requirements.sort_by(|a, b| a.id.cmp(&b.id));

        let mut index_entries = Vec::new();
        let mut traceability = HashMap::new();

        for requirement in requirements {
            let fallback_file = format!("generated/{}.json", requirement.id);
            let relative_path = Self::sanitize_relative_json_path(
                requirement.relative_path.as_deref(),
                &fallback_file,
            );
            let full_path = requirements_dir.join(&relative_path);
            if let Some(parent) = full_path.parent() {
                std::fs::create_dir_all(parent)?;
            }

            let payload = Self::tauri_compatible_requirement_payload(&requirement);
            std::fs::write(&full_path, serde_json::to_string_pretty(&payload)?)?;

            let mut linked_tasks = requirement.links.tasks.clone();
            linked_tasks.extend(requirement.linked_task_ids.clone());
            linked_tasks.sort();
            linked_tasks.dedup();

            let relative_str = relative_path.to_string_lossy().replace('\\', "/");
            index_entries.push(serde_json::json!({
                "id": requirement.id,
                "title": requirement.title,
                "category": requirement.category,
                "type": requirement.requirement_type,
                "priority": requirement.priority,
                "status": Self::tauri_requirement_status(requirement.status),
                "relative_path": relative_str,
                "tags": requirement.tags,
                "acceptance_criteria_count": requirement.acceptance_criteria.len(),
                "linked_tasks": linked_tasks,
                "linked_workflows": requirement.links.workflows,
                "linked_tests": requirement.links.tests,
                "linked_mockups": requirement.links.mockups,
                "linked_flows": requirement.links.flows,
                "linked_related_requirements": requirement.links.related_requirements,
                "created_at": requirement.created_at,
                "updated_at": requirement.updated_at,
            }));

            traceability.insert(requirement.id, linked_tasks);
        }

        let index_payload = serde_json::json!({
            "requirements": index_entries,
            "traceability": traceability,
        });
        std::fs::write(
            requirements_dir.join("index.json"),
            serde_json::to_string_pretty(&index_payload)?,
        )?;

        Ok(())
    }

    fn write_task_files(path: &Path, snapshot: &CoreState) -> Result<()> {
        let Some(ao_dir) = Self::ao_dir_for_state_file(path) else {
            return Ok(());
        };
        let tasks_dir = ao_dir.join("tasks");
        std::fs::create_dir_all(&tasks_dir)?;

        let mut tasks: Vec<_> = snapshot.tasks.values().cloned().collect();
        tasks.sort_by(|a, b| a.id.cmp(&b.id));

        let mut index_entries = Vec::new();
        let mut last_sequence = 0u32;

        for task in tasks {
            if let Some(seq) = task
                .id
                .strip_prefix("TASK-")
                .and_then(|value| value.parse::<u32>().ok())
            {
                last_sequence = last_sequence.max(seq);
            }

            let deadline = task
                .deadline
                .as_ref()
                .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
                .map(|value| value.with_timezone(&Utc).to_rfc3339());

            let payload = serde_json::json!({
                "id": task.id,
                "title": task.title,
                "description": task.description,
                "type": task.task_type,
                "status": task.status,
                "blocked_reason": task.blocked_reason,
                "blocked_at": task.blocked_at,
                "blocked_phase": task.blocked_phase,
                "blocked_by": task.blocked_by,
                "priority": task.priority,
                "risk": task.risk,
                "scope": task.scope,
                "complexity": task.complexity,
                "impact_area": task.impact_area,
                "assignee": task.assignee,
                "estimated_effort": task.estimated_effort,
                "linked_requirements": task.linked_requirements,
                "dependencies": task.dependencies,
                "checklist": task.checklist,
                "tags": task.tags,
                "workflow_metadata": task.workflow_metadata,
                "worktree_path": task.worktree_path,
                "branch_name": task.branch_name,
                "metadata": task.metadata,
                "deadline": deadline,
                "paused": task.paused,
                "cancelled": task.cancelled,
                "resource_requirements": task.resource_requirements,
            });
            std::fs::write(
                tasks_dir.join(format!("{}.json", task.id)),
                serde_json::to_string_pretty(&payload)?,
            )?;

            index_entries.push(serde_json::json!({
                "id": task.id,
                "title": task.title,
                "status": task.status,
                "priority": task.priority,
                "updated_at": task.metadata.updated_at,
            }));
        }

        let index_payload = serde_json::json!({
            "last_updated": Utc::now(),
            "last_sequence": last_sequence,
            "tasks": index_entries,
        });
        std::fs::write(
            tasks_dir.join("index.json"),
            serde_json::to_string_pretty(&index_payload)?,
        )?;

        Ok(())
    }

    fn persist_structured_artifacts(path: &Path, snapshot: &CoreState) -> Result<()> {
        let Some(docs_dir) = Self::docs_dir_for_state_file(path) else {
            return Ok(());
        };
        std::fs::create_dir_all(&docs_dir)?;

        let vision_json_path = docs_dir.join("vision.json");
        if let Some(vision) = &snapshot.vision {
            std::fs::write(&vision_json_path, serde_json::to_string_pretty(vision)?)?;
        } else if vision_json_path.exists() {
            std::fs::remove_file(&vision_json_path)?;
        }

        let mut requirements: Vec<_> = snapshot.requirements.values().cloned().collect();
        requirements.sort_by(|a, b| a.id.cmp(&b.id));
        std::fs::write(
            docs_dir.join("requirements.json"),
            serde_json::to_string_pretty(&requirements)?,
        )?;

        let mut tasks: Vec<_> = snapshot.tasks.values().cloned().collect();
        tasks.sort_by(|a, b| a.id.cmp(&b.id));
        std::fs::write(
            docs_dir.join("tasks.json"),
            serde_json::to_string_pretty(&tasks)?,
        )?;

        Self::write_requirement_files(path, snapshot)?;
        Self::write_task_files(path, snapshot)?;

        Ok(())
    }

    fn persist_snapshot(path: &Path, snapshot: &CoreState) -> Result<()> {
        std::fs::write(path, serde_json::to_string_pretty(snapshot)?)?;
        Self::persist_structured_artifacts(path, snapshot)?;
        Ok(())
    }

    fn write_json_if_missing<T: serde::Serialize>(path: &Path, value: &T) -> Result<()> {
        if path.exists() {
            return Ok(());
        }

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        std::fs::write(path, serde_json::to_string_pretty(value)?)?;
        Ok(())
    }

    fn git_command_status(project_root: &Path, args: &[&str]) -> Result<std::process::ExitStatus> {
        Command::new("git")
            .arg("-C")
            .arg(project_root)
            .args(args)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .with_context(|| {
                format!(
                    "failed to run git command in {}: git {}",
                    project_root.display(),
                    args.join(" ")
                )
            })
    }

    fn ensure_project_git_repository(project_root: &Path) -> Result<()> {
        let is_repo =
            Self::git_command_status(project_root, &["rev-parse", "--is-inside-work-tree"])?
                .success();
        if !is_repo {
            let init_status = Self::git_command_status(project_root, &["init"])?;
            if !init_status.success() {
                anyhow::bail!(
                    "failed to initialize git repository at {}",
                    project_root.display()
                );
            }
        }

        let has_head =
            Self::git_command_status(project_root, &["rev-parse", "--verify", "HEAD"])?.success();
        if !has_head {
            let seed_status = Command::new("git")
                .arg("-C")
                .arg(project_root)
                .args([
                    "-c",
                    "user.name=AO Bootstrap",
                    "-c",
                    "user.email=ao-bootstrap@local",
                    "commit",
                    "--allow-empty",
                    "-m",
                    "chore: initialize repository",
                ])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .with_context(|| {
                    format!(
                        "failed to create initial git commit in {}",
                        project_root.display()
                    )
                })?;
            if !seed_status.success() {
                anyhow::bail!(
                    "failed to create initial git commit in {}",
                    project_root.display()
                );
            }
        }

        Ok(())
    }

    fn bootstrap_project_base_configs(project_root: &Path) -> Result<()> {
        std::fs::create_dir_all(project_root)?;
        Self::ensure_project_git_repository(project_root)?;

        let ao_dir = project_root.join(".ao");
        let state_dir = ao_dir.join("state");
        std::fs::create_dir_all(&state_dir)?;

        let core_state_path = ao_dir.join("core-state.json");
        let is_new_project = !core_state_path.exists();
        if !core_state_path.exists() {
            Self::persist_snapshot(&core_state_path, &CoreState::default_with_stopped())?;
        }

        Self::write_json_if_missing(&ao_dir.join("resume-config.json"), &ResumeConfig::default())?;
        crate::state_machines::ensure_state_machines_file(project_root)?;
        if is_new_project {
            crate::workflow_config::ensure_workflow_config_file(project_root)?;
            crate::agent_runtime_config::ensure_agent_runtime_config_file(project_root)?;
        }

        protocol::Config::load_or_default(project_root.to_string_lossy().as_ref())?;
        Ok(())
    }

    fn workflow_manager(&self) -> WorkflowStateManager {
        WorkflowStateManager::new(&self.project_root)
    }
}

impl ServiceHub for InMemoryServiceHub {
    fn daemon(&self) -> Arc<dyn DaemonServiceApi> {
        Arc::new(self.clone())
    }

    fn projects(&self) -> Arc<dyn ProjectServiceApi> {
        Arc::new(self.clone())
    }

    fn tasks(&self) -> Arc<dyn TaskServiceApi> {
        Arc::new(self.clone())
    }

    fn workflows(&self) -> Arc<dyn WorkflowServiceApi> {
        Arc::new(self.clone())
    }

    fn planning(&self) -> Arc<dyn PlanningServiceApi> {
        Arc::new(self.clone())
    }

    fn review(&self) -> Arc<dyn ReviewServiceApi> {
        Arc::new(self.clone())
    }
}

impl ServiceHub for FileServiceHub {
    fn daemon(&self) -> Arc<dyn DaemonServiceApi> {
        Arc::new(self.clone())
    }

    fn projects(&self) -> Arc<dyn ProjectServiceApi> {
        Arc::new(self.clone())
    }

    fn tasks(&self) -> Arc<dyn TaskServiceApi> {
        Arc::new(self.clone())
    }

    fn workflows(&self) -> Arc<dyn WorkflowServiceApi> {
        Arc::new(self.clone())
    }

    fn planning(&self) -> Arc<dyn PlanningServiceApi> {
        Arc::new(self.clone())
    }

    fn review(&self) -> Arc<dyn ReviewServiceApi> {
        Arc::new(self.clone())
    }
}

#[cfg(test)]
mod tests;
