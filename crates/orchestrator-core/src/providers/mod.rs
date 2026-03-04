use anyhow::Result;
use async_trait::async_trait;

use crate::types::{
    DependencyType, OrchestratorTask, RequirementItem, RequirementsDraftInput, RequirementsDraftResult,
    RequirementsExecutionInput, RequirementsExecutionResult, RequirementsRefineInput, TaskCreateInput,
    TaskFilter, TaskStatistics, TaskStatus, TaskUpdateInput,
};

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
pub trait RequirementsProvider: Send + Sync {
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
    async fn upsert_requirement(
        &self,
        requirement: RequirementItem,
    ) -> Result<RequirementItem>;
    async fn delete_requirement(&self, id: &str) -> Result<()>;
    async fn execute_requirements(
        &self,
        input: RequirementsExecutionInput,
    ) -> Result<RequirementsExecutionResult>;
}

pub mod builtin;
#[cfg(feature = "jira")]
pub mod jira;
#[cfg(feature = "gitlab")]
pub mod gitlab;
pub mod git;
#[cfg(feature = "linear")]
pub mod linear;

pub use builtin::{BuiltinRequirementsProvider, BuiltinTaskProvider};
#[cfg(feature = "jira")]
pub use jira::JiraTaskProvider;
pub use git::{
    BuiltinGitProvider, CreatePrInput, GitHubProvider, GitProvider, MergeResult,
    PullRequestInfo, WorktreeInfo,
};
#[cfg(feature = "linear")]
pub use linear::LinearTaskProvider;
#[cfg(feature = "gitlab")]
pub use gitlab::{GitLabConfig, GitLabGitProvider};
