use anyhow::Result;
use async_trait::async_trait;
use protocol::orchestrator::{
    DependencyType, OrchestratorTask, RequirementItem, RequirementsDraftInput, RequirementsDraftResult,
    RequirementsExecutionInput, RequirementsExecutionResult, RequirementsRefineInput, SubjectRef, TaskCreateInput,
    TaskFilter, TaskStatistics, TaskStatus, TaskUpdateInput,
};
use std::collections::HashMap;

#[async_trait]
pub trait TaskProvider: Send + Sync {
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
pub trait RequirementsProvider: Send + Sync {
    async fn list_requirements(&self) -> Result<Vec<RequirementItem>>;
    async fn get_requirement(&self, id: &str) -> Result<RequirementItem>;
    async fn upsert_requirement(&self, requirement: RequirementItem) -> Result<RequirementItem>;
    async fn delete_requirement(&self, id: &str) -> Result<()>;
}

// Planning verbs (draft/refine/execute) live on a separate service trait
// because they are LLM-driven orchestrators that need agent runtime, the
// model registry, and cross-cutting tool access — not a fit for the
// subject_backend dispatch surface that `RequirementsProvider` feeds.
#[async_trait]
pub trait RequirementsPlanningService: Send + Sync {
    async fn draft_requirements(&self, input: RequirementsDraftInput) -> Result<RequirementsDraftResult>;
    async fn refine_requirements(&self, input: RequirementsRefineInput) -> Result<Vec<RequirementItem>>;
    async fn execute_requirements(&self, input: RequirementsExecutionInput) -> Result<RequirementsExecutionResult>;
}

#[derive(Debug, Clone)]
pub struct SubjectContext {
    pub subject_kind: String,
    pub subject_id: String,
    pub subject_title: String,
    pub subject_description: String,
    pub attributes: HashMap<String, String>,
    pub task: Option<OrchestratorTask>,
}

#[async_trait]
pub trait SubjectResolver: Send + Sync {
    async fn resolve_subject_context(
        &self,
        subject: &SubjectRef,
        fallback_title: Option<&str>,
        fallback_description: Option<&str>,
    ) -> Result<SubjectContext>;
}

#[async_trait]
pub trait ProjectAdapter: Send + Sync {
    async fn ensure_execution_cwd(
        &self,
        project_root: &str,
        subject: &SubjectRef,
        subject_context: &SubjectContext,
    ) -> Result<String>;
}

#[async_trait]
pub trait TaskAdapterApi: Send + Sync {
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
pub trait PlanningAdapterApi: Send + Sync {
    async fn draft_requirements(&self, input: RequirementsDraftInput) -> Result<RequirementsDraftResult>;
    async fn list_requirements(&self) -> Result<Vec<RequirementItem>>;
    async fn get_requirement(&self, id: &str) -> Result<RequirementItem>;
    async fn refine_requirements(&self, input: RequirementsRefineInput) -> Result<Vec<RequirementItem>>;
    async fn upsert_requirement(&self, requirement: RequirementItem) -> Result<RequirementItem>;
    async fn delete_requirement(&self, id: &str) -> Result<()>;
    async fn execute_requirements(&self, input: RequirementsExecutionInput) -> Result<RequirementsExecutionResult>;
}

pub mod adapter;
pub mod builtin;
pub mod git;
pub mod plugins {
    pub use animus_plugin_protocol as protocol;
    pub use orchestrator_plugin_host::{
        discover_plugins, DiscoveredPlugin, DiscoverySource, DiscoveryWarning, PluginConfigEntry, PluginDiscovery,
        PluginHost, PluginRegistry, StdioTransport, SubjectRouter,
    };
}

pub use adapter::{
    builtin_subject_adapter_registry, BuiltinCustomSubjectAdapter, BuiltinProjectAdapter,
    BuiltinRequirementSubjectAdapter, BuiltinSubjectResolver, BuiltinTaskSubjectAdapter, SubjectAdapter,
    SubjectAdapterRegistry,
};
pub use builtin::{BuiltinRequirementsPlanningService, BuiltinRequirementsProvider, BuiltinTaskProvider};
pub use git::{BuiltinGitProvider, CreatePrInput, GitProvider, MergeResult, PullRequestInfo, WorktreeInfo};
