use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use crate::types::not_found;
use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use chrono::Utc;
use fs2::FileExt;
use orchestrator_store::{write_json_if_missing, write_json_pretty};
use protocol::{RunnerStatusRequest, RunnerStatusResponse};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::RwLock;
use tokio::time::sleep;
use uuid::Uuid;

use crate::providers::{BuiltinGitProvider, GitProvider};
use crate::providers::{
    BuiltinProjectAdapter, BuiltinRequirementsProvider, BuiltinSubjectResolver, BuiltinTaskProvider, ProjectAdapter,
    RequirementsProvider, SubjectResolver, TaskProvider,
};
use crate::types::{
    AgentHandoffRequestInput, AgentHandoffResult, ArchitectureGraph, Assignee, ChecklistItem, CheckpointReason,
    CodebaseInsight, Complexity, ComplexityAssessment, DaemonHealth, DaemonStatus, DependencyType, ListPage, LogEntry,
    LogLevel, OrchestratorProject, OrchestratorTask, OrchestratorWorkflow, PhaseDecision, Priority, ProjectConfig,
    ProjectCreateInput, ProjectType, RequirementFilter, RequirementItem, RequirementPriority, RequirementPriorityExt,
    RequirementQuery, RequirementQuerySort, RequirementStatus, RequirementsDraftInput, RequirementsDraftResult,
    RequirementsExecutionInput, RequirementsExecutionResult, RequirementsRefineInput, RiskLevel, Scope,
    TaskCreateInput, TaskDensity, TaskDependency, TaskFilter, TaskMetadata, TaskPriorityDistribution,
    TaskPriorityPolicyReport, TaskPriorityRebalanceChange, TaskPriorityRebalanceOptions, TaskPriorityRebalancePlan,
    TaskQuery, TaskQuerySort, TaskStatistics, TaskStatus, TaskType, TaskUpdateInput, VisionDocument, VisionDraftInput,
    WorkflowFilter, WorkflowMetadata, WorkflowQuery, WorkflowQuerySort, WorkflowRunInput, WorkflowStatus,
};
use crate::workflow::{ResumeConfig, WorkflowLifecycleExecutor, WorkflowStateManager};

mod daemon_impl;
mod phase_execution;
mod planning_impl;
mod planning_shared;
mod planning_utils;
mod project_impl;
mod project_shared;
mod query_support;
mod review_impl;
mod runner_helpers;
mod schedule_state;
mod state_store;
mod task_impl;
mod task_shared;
mod workflow_impl;

pub use phase_execution::{PhaseExecutionRequest, PhaseExecutionResult, PhaseExecutor, PhaseVerdict};
use planning_utils::*;
pub use runner_helpers::stop_agent_runner_process;
use runner_helpers::*;
pub use schedule_state::{load_schedule_state, save_schedule_state, ScheduleRunState, ScheduleState};
use state_store::{load_core_state, load_core_state_for_mutation, CoreState};
pub use task_shared::task_matches_filter;
use task_shared::*;

pub fn evaluate_task_priority_policy(
    tasks: &[OrchestratorTask],
    high_budget_percent: u8,
) -> Result<TaskPriorityPolicyReport> {
    evaluate_task_priority_policy_report(tasks, high_budget_percent)
}

pub fn plan_task_priority_rebalance(
    tasks: &[OrchestratorTask],
    options: TaskPriorityRebalanceOptions,
) -> Result<TaskPriorityRebalancePlan> {
    plan_task_priority_rebalance_from_tasks(tasks, options)
}

#[derive(Debug, Clone, Default)]
pub struct DaemonStartConfig {
    pub pool_size: Option<usize>,
    pub skip_runner: bool,
    pub runner_scope: Option<String>,
}

#[async_trait]
pub trait DaemonServiceApi: Send + Sync {
    async fn start(&self, config: DaemonStartConfig) -> Result<()>;
    async fn stop(&self) -> Result<()>;
    async fn pause(&self) -> Result<()>;
    async fn resume(&self) -> Result<()>;
    async fn status(&self) -> Result<DaemonStatus>;
    async fn health(&self) -> Result<DaemonHealth>;
    async fn logs(&self, limit: Option<usize>) -> Result<Vec<LogEntry>>;
    async fn clear_logs(&self) -> Result<()>;
    async fn active_agents(&self) -> Result<usize>;
    async fn set_active_process_count(&self, count: usize) -> Result<()>;
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
    fn task_provider(&self) -> Arc<dyn TaskProvider>;
    async fn list(&self) -> Result<Vec<OrchestratorTask>>;
    async fn query(&self, query: TaskQuery) -> Result<ListPage<OrchestratorTask>>;
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
    async fn assign_human(&self, id: &str, user_id: String, updated_by: String) -> Result<OrchestratorTask>;
    async fn set_status(&self, id: &str, status: TaskStatus, validate: bool) -> Result<OrchestratorTask>;
    async fn add_checklist_item(&self, id: &str, description: String, updated_by: String) -> Result<OrchestratorTask>;
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
    async fn remove_dependency(&self, id: &str, dependency_id: &str, updated_by: String) -> Result<OrchestratorTask>;
}

#[async_trait]
pub trait WorkflowServiceApi: Send + Sync {
    async fn list(&self) -> Result<Vec<OrchestratorWorkflow>>;
    async fn query(&self, query: WorkflowQuery) -> Result<ListPage<OrchestratorWorkflow>>;
    async fn get(&self, id: &str) -> Result<OrchestratorWorkflow>;
    async fn decisions(&self, id: &str) -> Result<Vec<crate::types::WorkflowDecisionRecord>>;
    async fn list_checkpoints(&self, id: &str) -> Result<Vec<usize>>;
    async fn get_checkpoint(&self, id: &str, checkpoint_number: usize) -> Result<OrchestratorWorkflow>;
    async fn run(&self, input: WorkflowRunInput) -> Result<OrchestratorWorkflow>;
    async fn resume(&self, id: &str) -> Result<OrchestratorWorkflow>;
    async fn pause(&self, id: &str) -> Result<OrchestratorWorkflow>;
    async fn cancel(&self, id: &str) -> Result<OrchestratorWorkflow>;
    async fn complete_current_phase(&self, id: &str) -> Result<OrchestratorWorkflow>;
    async fn complete_current_phase_with_decision(
        &self,
        id: &str,
        decision: Option<PhaseDecision>,
    ) -> Result<OrchestratorWorkflow>;
    async fn fail_current_phase(&self, id: &str, error: String) -> Result<OrchestratorWorkflow>;
    async fn mark_completed_failed(&self, id: &str, error: String) -> Result<OrchestratorWorkflow>;
    async fn mark_merge_conflict(&self, id: &str, error: String) -> Result<OrchestratorWorkflow>;
    async fn resolve_merge_conflict(&self, id: &str) -> Result<OrchestratorWorkflow>;
    async fn record_feedback(&self, id: &str, feedback: String) -> Result<()>;
}

#[async_trait]
pub trait PlanningServiceApi: Send + Sync {
    fn requirements_provider(&self) -> Arc<dyn RequirementsProvider>;
    async fn draft_vision(&self, input: VisionDraftInput) -> Result<VisionDocument>;
    async fn get_vision(&self) -> Result<Option<VisionDocument>>;
    async fn draft_requirements(&self, input: RequirementsDraftInput) -> Result<RequirementsDraftResult>;
    async fn query(&self, query: RequirementQuery) -> Result<ListPage<RequirementItem>>;
    async fn list_requirements(&self) -> Result<Vec<RequirementItem>>;
    async fn get_requirement(&self, id: &str) -> Result<RequirementItem>;
    async fn refine_requirements(&self, input: RequirementsRefineInput) -> Result<Vec<RequirementItem>>;
    async fn upsert_requirement(&self, requirement: RequirementItem) -> Result<RequirementItem>;
    async fn delete_requirement(&self, id: &str) -> Result<()>;
    async fn execute_requirements(&self, input: RequirementsExecutionInput) -> Result<RequirementsExecutionResult>;
}

#[async_trait]
pub trait ReviewServiceApi: Send + Sync {
    async fn request_handoff(&self, input: AgentHandoffRequestInput) -> Result<AgentHandoffResult>;
}

pub trait ServiceHub: Send + Sync {
    fn daemon(&self) -> Arc<dyn DaemonServiceApi>;
    fn projects(&self) -> Arc<dyn ProjectServiceApi>;
    fn tasks(&self) -> Arc<dyn TaskServiceApi>;
    fn task_provider(&self) -> Arc<dyn TaskProvider>;
    fn subject_resolver(&self) -> Arc<dyn SubjectResolver>;
    fn workflows(&self) -> Arc<dyn WorkflowServiceApi>;
    fn planning(&self) -> Arc<dyn PlanningServiceApi>;
    fn requirements_provider(&self) -> Arc<dyn RequirementsProvider>;
    fn project_adapter(&self) -> Arc<dyn ProjectAdapter>;
    fn review(&self) -> Arc<dyn ReviewServiceApi>;
}

#[derive(Clone)]
pub struct InMemoryServiceHub {
    state: Arc<RwLock<CoreState>>,
}

impl Default for InMemoryServiceHub {
    fn default() -> Self {
        Self { state: Arc::new(RwLock::new(CoreState::default_with_stopped())) }
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
            lock.logs.push(LogEntry { timestamp: Utc::now(), level, message });
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
    pub fn git_provider(&self) -> Arc<dyn GitProvider> {
        Arc::new(BuiltinGitProvider::new(self.project_root.clone()))
    }

    pub fn new(project_root: impl AsRef<Path>) -> Result<Self> {
        let project_root = project_root.as_ref().to_path_buf();
        Self::bootstrap_project_base_configs(&project_root)?;
        let scoped_root = protocol::scoped_state_root(&project_root).unwrap_or_else(|| project_root.join(".ao"));
        let state_file = scoped_root.join("core-state.json");

        Self::migrate_workflows_from_core_state(&state_file, &project_root);

        let mut state = load_core_state(&state_file);

        crate::workflow::migrate_tasks_and_requirements_from_core_state(
            &project_root,
            &state.tasks,
            &state.requirements,
        );

        if let Ok(tasks) = crate::workflow::load_all_tasks(&project_root) {
            if !tasks.is_empty() {
                state.tasks = tasks;
            }
        }

        if let Ok(reqs) = crate::workflow::load_all_requirements(&project_root) {
            if !reqs.is_empty() {
                state.requirements = reqs;
            }
        }

        let workflow_manager = WorkflowStateManager::new(&project_root);
        if let Ok(workflows) = workflow_manager.list() {
            state.workflows = workflows.into_iter().map(|workflow| (workflow.id.clone(), workflow)).collect();
        }

        let hub = Self { state: Arc::new(RwLock::new(state)), state_file, project_root };
        Ok(hub)
    }

    fn docs_dir_for_state_file(path: &Path) -> Option<PathBuf> {
        path.parent().map(|ao_dir| ao_dir.join("docs"))
    }

    fn ao_dir_for_state_file(path: &Path) -> Option<PathBuf> {
        path.parent().map(Path::to_path_buf)
    }

    fn state_lock_file_for_state_file(path: &Path) -> PathBuf {
        path.with_extension("lock")
    }

    fn lock_state_file(path: &Path) -> Result<std::fs::File> {
        let lock_path = Self::state_lock_file_for_state_file(path);
        if let Some(parent) = lock_path.parent() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("failed to create parent directory for core state lock at {}", lock_path.display())
            })?;
        }

        let lock_file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&lock_path)
            .with_context(|| format!("failed to open core state lock file at {}", lock_path.display()))?;
        lock_file
            .lock_exclusive()
            .with_context(|| format!("failed to acquire exclusive core state lock at {}", lock_path.display()))?;
        Ok(lock_file)
    }

    async fn mutate_persistent_state<T>(
        &self,
        mutator: impl FnOnce(&mut CoreState) -> Result<T>,
    ) -> Result<(T, CoreState)> {
        let _file_lock = Self::lock_state_file(&self.state_file)?;

        let mut state = self.state.write().await;
        *state = load_core_state_for_mutation(&self.state_file)?;
        if let Ok(tasks) = crate::workflow::load_all_tasks(&self.project_root) {
            if !tasks.is_empty() {
                state.tasks = tasks;
            }
        }
        if let Ok(reqs) = crate::workflow::load_all_requirements(&self.project_root) {
            if !reqs.is_empty() {
                state.requirements = reqs;
            }
        }
        let output = mutator(&mut state)?;
        let dirty_task_ids: Vec<String> = if state.all_tasks_dirty {
            state.tasks.keys().cloned().collect()
        } else {
            state.dirty_tasks.iter().cloned().collect()
        };
        let dirty_req_ids: Vec<String> = if state.all_requirements_dirty {
            state.requirements.keys().cloned().collect()
        } else {
            state.dirty_requirements.iter().cloned().collect()
        };
        Self::persist_dirty_to_sqlite(&self.project_root, &state, &dirty_task_ids, &dirty_req_ids);
        state.dirty_tasks.clear();
        state.dirty_requirements.clear();
        state.all_tasks_dirty = false;
        state.all_requirements_dirty = false;
        Self::persist_and_clear_dirty(&self.state_file, &mut state)?;
        Ok((output, state.clone()))
    }

    fn persist_dirty_to_sqlite(project_root: &Path, state: &CoreState, task_ids: &[String], req_ids: &[String]) {
        for id in task_ids {
            if let Some(task) = state.tasks.get(id) {
                let _ = crate::workflow::save_task(project_root, task);
            } else {
                let _ = crate::workflow::delete_task(project_root, id);
            }
        }
        for id in req_ids {
            if let Some(req) = state.requirements.get(id) {
                let _ = crate::workflow::save_requirement(project_root, req);
            } else {
                let _ = crate::workflow::delete_requirement(project_root, id);
            }
        }
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

    fn index_root_for_state_file(path: &Path) -> Option<PathBuf> {
        path.parent().map(|scoped_root| scoped_root.join("index"))
    }

    fn legacy_requirement_status(status: RequirementStatus) -> &'static str {
        match status {
            RequirementStatus::Draft | RequirementStatus::Refined | RequirementStatus::Planned => "draft",
            RequirementStatus::InProgress => "em-review",
            RequirementStatus::Done | RequirementStatus::Implemented => "implemented",
            RequirementStatus::PoReview => "po-review",
            RequirementStatus::EmReview => "em-review",
            RequirementStatus::NeedsRework => "needs-rework",
            RequirementStatus::Approved => "approved",
            RequirementStatus::Deprecated => "deprecated",
        }
    }

    fn legacy_requirement_payload(requirement: &RequirementItem) -> serde_json::Value {
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
            "status": Self::legacy_requirement_status(requirement.status),
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

    fn write_requirement_files(path: &Path, snapshot: &CoreState, only_ids: Option<&HashSet<String>>) -> Result<()> {
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
            let relative_path = Self::sanitize_relative_json_path(requirement.relative_path.as_deref(), &fallback_file);
            let full_path = requirements_dir.join(&relative_path);
            if let Some(parent) = full_path.parent() {
                std::fs::create_dir_all(parent)?;
            }

            let payload = Self::legacy_requirement_payload(&requirement);
            if only_ids.is_none() || only_ids.is_some_and(|ids| ids.contains(&requirement.id)) {
                std::fs::write(&full_path, serde_json::to_string_pretty(&payload)?)?;
            }

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
                "status": Self::legacy_requirement_status(requirement.status),
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
        let index_root =
            Self::index_root_for_state_file(path).ok_or_else(|| anyhow!("failed to resolve AO index directory"))?;
        std::fs::create_dir_all(index_root.join("requirements"))?;
        let requirements_index = index_root.join("requirements").join("index.json");
        std::fs::write(requirements_index, serde_json::to_string_pretty(&index_payload)?)?;

        Ok(())
    }

    fn write_task_files(path: &Path, snapshot: &CoreState, only_ids: Option<&HashSet<String>>) -> Result<()> {
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
            if let Some(seq) = task.id.strip_prefix("TASK-").and_then(|value| value.parse::<u32>().ok()) {
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
                "linked_architecture_entities": task.linked_architecture_entities,
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
            if only_ids.is_none() || only_ids.is_some_and(|ids| ids.contains(&task.id)) {
                std::fs::write(tasks_dir.join(format!("{}.json", task.id)), serde_json::to_string_pretty(&payload)?)?;
            }

            index_entries.push(serde_json::json!({
                "id": task.id,
                "title": task.title,
                "status": task.status,
                "priority": task.priority,
                "linked_architecture_entities_count": task.linked_architecture_entities.len(),
                "updated_at": task.metadata.updated_at,
            }));
        }

        let index_payload = serde_json::json!({
            "last_updated": Utc::now(),
            "last_sequence": last_sequence,
            "tasks": index_entries,
        });
        let index_dir = Self::index_root_for_state_file(path)
            .ok_or_else(|| anyhow!("failed to resolve AO index directory"))?
            .join("tasks");
        std::fs::create_dir_all(&index_dir)?;
        std::fs::write(index_dir.join("index.json"), serde_json::to_string_pretty(&index_payload)?)?;

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

        let architecture_json_path = docs_dir.join("architecture.json");
        std::fs::write(&architecture_json_path, serde_json::to_string_pretty(&snapshot.architecture)?)?;

        Ok(())
    }

    fn persist_snapshot(path: &Path, snapshot: &CoreState) -> Result<()> {
        write_json_pretty(path, snapshot)?;
        Self::persist_structured_artifacts(path, snapshot)?;
        Ok(())
    }

    fn persist_and_clear_dirty(path: &Path, state: &mut CoreState) -> Result<()> {
        write_json_pretty(path, &*state)?;

        if let Some(docs_dir) = Self::docs_dir_for_state_file(path) {
            std::fs::create_dir_all(&docs_dir)?;

            let vision_json_path = docs_dir.join("vision.json");
            if let Some(vision) = &state.vision {
                std::fs::write(&vision_json_path, serde_json::to_string_pretty(vision)?)?;
            } else if vision_json_path.exists() {
                std::fs::remove_file(&vision_json_path)?;
            }

            let architecture_json_path = docs_dir.join("architecture.json");
            std::fs::write(&architecture_json_path, serde_json::to_string_pretty(&state.architecture)?)?;
        }
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
            .with_context(|| format!("failed to run git command in {}: git {}", project_root.display(), args.join(" ")))
    }

    fn ensure_project_git_repository(project_root: &Path) -> Result<()> {
        let is_repo = Self::git_command_status(project_root, &["rev-parse", "--is-inside-work-tree"])?.success();
        if !is_repo {
            let init_status = Self::git_command_status(project_root, &["init"])?;
            if !init_status.success() {
                anyhow::bail!("failed to initialize git repository at {}", project_root.display());
            }
        }

        let has_head = Self::git_command_status(project_root, &["rev-parse", "--verify", "HEAD"])?.success();
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
                .with_context(|| format!("failed to create initial git commit in {}", project_root.display()))?;
            if !seed_status.success() {
                anyhow::bail!("failed to create initial git commit in {}", project_root.display());
            }
        }

        Ok(())
    }

    pub fn bootstrap_project_git_repository(project_root: &Path) -> Result<()> {
        std::fs::create_dir_all(project_root)?;
        Self::ensure_project_git_repository(project_root)
    }

    fn maybe_migrate_state_to_scoped_root(project_root: &Path) -> Result<()> {
        let Some(scoped_root) = protocol::scoped_state_root(project_root) else {
            return Ok(());
        };
        if scoped_root.join("core-state.json").exists() {
            return Ok(());
        }
        let legacy_ao = project_root.join(".ao");
        if !legacy_ao.join("core-state.json").exists() {
            return Ok(());
        }
        std::fs::create_dir_all(&scoped_root)?;
        let copy = |name: &str| -> Result<()> {
            let src = legacy_ao.join(name);
            if src.exists() {
                let dst = scoped_root.join(name);
                if src.is_dir() {
                    Self::copy_dir_recursive(&src, &dst)?;
                } else {
                    if let Some(parent) = dst.parent() {
                        std::fs::create_dir_all(parent)?;
                    }
                    std::fs::copy(&src, &dst)?;
                }
            }
            Ok(())
        };
        copy("core-state.json")?;
        copy("resume-config.json")?;
        copy("state")?;
        copy("tasks")?;
        copy("requirements")?;
        copy("docs")?;
        copy("workflow-state")?;

        let legacy_daemon_config = legacy_ao.join(crate::daemon_config::DAEMON_PROJECT_CONFIG_FILE_NAME);
        if legacy_daemon_config.exists() {
            let daemon_dir = scoped_root.join("daemon");
            std::fs::create_dir_all(&daemon_dir)?;
            let scoped_daemon_config = daemon_dir.join(crate::daemon_config::DAEMON_PROJECT_CONFIG_FILE_NAME);
            if !scoped_daemon_config.exists() {
                std::fs::copy(&legacy_daemon_config, &scoped_daemon_config)?;
            }
        }

        let scoped_state_file = scoped_root.join("core-state.json");
        let tasks_dir = scoped_root.join("tasks");
        let requirements_dir = scoped_root.join("requirements");
        if !tasks_dir.exists() || !requirements_dir.exists() {
            let state = state_store::load_core_state(&scoped_state_file);
            if !state.tasks.is_empty() || !state.requirements.is_empty() {
                Self::persist_structured_artifacts(&scoped_state_file, &state)?;
            }
        }

        std::fs::write(scoped_root.join(".migrated-from-repo"), project_root.display().to_string())?;
        eprintln!(
            "{}",
            serde_json::json!({
                "event": "state_migration",
                "from": legacy_ao.display().to_string(),
                "to": scoped_root.display().to_string(),
                "migrated_at": chrono::Utc::now().to_rfc3339(),
            })
        );
        Ok(())
    }

    fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<()> {
        std::fs::create_dir_all(dst)?;
        for entry in std::fs::read_dir(src)? {
            let entry = entry?;
            let src_path = entry.path();
            let dst_path = dst.join(entry.file_name());
            if src_path.is_dir() {
                Self::copy_dir_recursive(&src_path, &dst_path)?;
            } else {
                std::fs::copy(&src_path, &dst_path)?;
            }
        }
        Ok(())
    }

    fn bootstrap_project_base_configs(project_root: &Path) -> Result<()> {
        std::fs::create_dir_all(project_root)?;

        let ao_dir = project_root.join(".ao");
        std::fs::create_dir_all(&ao_dir)?;

        Self::maybe_migrate_state_to_scoped_root(project_root)?;

        let scoped_root =
            protocol::scoped_state_root(project_root).expect("scoped_state_root requires a home directory");
        let state_dir = scoped_root.join("state");
        std::fs::create_dir_all(&state_dir)?;

        let core_state_path = scoped_root.join("core-state.json");
        let is_new_project = !core_state_path.exists();
        if !core_state_path.exists() {
            let _file_lock = Self::lock_state_file(&core_state_path)?;
            if !core_state_path.exists() {
                Self::persist_snapshot(&core_state_path, &CoreState::default_with_stopped())?;
            }
        }

        write_json_if_missing(&scoped_root.join("resume-config.json"), &ResumeConfig::default())?;
        crate::state_machines::ensure_state_machines_file(project_root)?;
        if is_new_project {
            crate::workflow_config::ensure_workflow_yaml_scaffold(project_root)?;
        }
        crate::workflow_config::ensure_workflow_config_compiled(project_root)?;

        protocol::Config::load_or_default(project_root.to_string_lossy().as_ref())?;
        Ok(())
    }

    fn workflow_manager(&self) -> WorkflowStateManager {
        WorkflowStateManager::new(&self.project_root)
    }

    fn migrate_workflows_from_core_state(state_file: &Path, project_root: &Path) {
        if !state_file.exists() {
            return;
        }
        let Ok(contents) = std::fs::read_to_string(state_file) else {
            return;
        };
        let Ok(raw) = serde_json::from_str::<serde_json::Value>(&contents) else {
            return;
        };
        let Some(workflows_obj) = raw.get("workflows").and_then(|v| v.as_object()) else {
            return;
        };
        if workflows_obj.is_empty() {
            return;
        }
        let manager = WorkflowStateManager::new(project_root);
        for (id, workflow_value) in workflows_obj {
            if manager.load(id).is_ok() {
                continue;
            }
            let Ok(workflow) = serde_json::from_value::<crate::types::OrchestratorWorkflow>(workflow_value.clone())
            else {
                continue;
            };
            let _ = manager.save(&workflow);
        }
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

    fn task_provider(&self) -> Arc<dyn TaskProvider> {
        Arc::new(BuiltinTaskProvider::new(Arc::new(self.clone())))
    }

    fn subject_resolver(&self) -> Arc<dyn SubjectResolver> {
        Arc::new(BuiltinSubjectResolver::new(Arc::new(self.clone())))
    }

    fn workflows(&self) -> Arc<dyn WorkflowServiceApi> {
        Arc::new(self.clone())
    }

    fn planning(&self) -> Arc<dyn PlanningServiceApi> {
        Arc::new(self.clone())
    }

    fn requirements_provider(&self) -> Arc<dyn RequirementsProvider> {
        Arc::new(BuiltinRequirementsProvider::new(Arc::new(self.clone())))
    }

    fn project_adapter(&self) -> Arc<dyn ProjectAdapter> {
        Arc::new(BuiltinProjectAdapter::new(Arc::new(self.clone())))
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

    fn task_provider(&self) -> Arc<dyn TaskProvider> {
        Arc::new(BuiltinTaskProvider::new(Arc::new(self.clone())))
    }

    fn subject_resolver(&self) -> Arc<dyn SubjectResolver> {
        Arc::new(BuiltinSubjectResolver::new(Arc::new(self.clone())))
    }

    fn workflows(&self) -> Arc<dyn WorkflowServiceApi> {
        Arc::new(self.clone())
    }

    fn planning(&self) -> Arc<dyn PlanningServiceApi> {
        Arc::new(self.clone())
    }

    fn requirements_provider(&self) -> Arc<dyn RequirementsProvider> {
        Arc::new(BuiltinRequirementsProvider::new(Arc::new(self.clone())))
    }

    fn project_adapter(&self) -> Arc<dyn ProjectAdapter> {
        Arc::new(BuiltinProjectAdapter::new(Arc::new(self.clone())))
    }

    fn review(&self) -> Arc<dyn ReviewServiceApi> {
        Arc::new(self.clone())
    }
}

#[cfg(test)]
mod tests;
