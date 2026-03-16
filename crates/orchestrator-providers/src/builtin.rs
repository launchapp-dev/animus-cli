use std::sync::Arc;

use anyhow::Result;
use protocol::orchestrator::{
    DependencyType, OrchestratorTask, RequirementItem, RequirementsDraftInput, RequirementsDraftResult,
    RequirementsExecutionInput, RequirementsExecutionResult, RequirementsRefineInput, TaskCreateInput, TaskFilter,
    TaskStatistics, TaskStatus, TaskUpdateInput,
};

use crate::{PlanningServiceApi, RequirementsProvider, TaskProvider, TaskServiceApi};

#[derive(Clone)]
pub struct BuiltinTaskProvider<T> {
    hub: Arc<T>,
}

impl<T> BuiltinTaskProvider<T>
where
    T: TaskServiceApi,
{
    #[must_use]
    pub fn new(hub: Arc<T>) -> Self {
        Self { hub }
    }
}

#[async_trait::async_trait]
impl<T> TaskProvider for BuiltinTaskProvider<T>
where
    T: TaskServiceApi,
{
    async fn list(&self) -> Result<Vec<OrchestratorTask>> {
        self.hub.list().await
    }

    async fn list_filtered(&self, filter: TaskFilter) -> Result<Vec<OrchestratorTask>> {
        self.hub.list_filtered(filter).await
    }

    async fn list_prioritized(&self) -> Result<Vec<OrchestratorTask>> {
        self.hub.list_prioritized().await
    }

    async fn next_task(&self) -> Result<Option<OrchestratorTask>> {
        self.hub.next_task().await
    }

    async fn statistics(&self) -> Result<TaskStatistics> {
        self.hub.statistics().await
    }

    async fn get(&self, id: &str) -> Result<OrchestratorTask> {
        self.hub.get(id).await
    }

    async fn create(&self, input: TaskCreateInput) -> Result<OrchestratorTask> {
        self.hub.create(input).await
    }

    async fn update(&self, id: &str, input: TaskUpdateInput) -> Result<OrchestratorTask> {
        self.hub.update(id, input).await
    }

    async fn replace(&self, task: OrchestratorTask) -> Result<OrchestratorTask> {
        self.hub.replace(task).await
    }

    async fn delete(&self, id: &str) -> Result<()> {
        self.hub.delete(id).await
    }

    async fn assign(&self, id: &str, assignee: String) -> Result<OrchestratorTask> {
        self.hub.assign(id, assignee).await
    }

    async fn set_status(&self, id: &str, status: TaskStatus, validate: bool) -> Result<OrchestratorTask> {
        self.hub.set_status(id, status, validate).await
    }

    async fn add_checklist_item(&self, id: &str, description: String, updated_by: String) -> Result<OrchestratorTask> {
        self.hub.add_checklist_item(id, description, updated_by).await
    }

    async fn update_checklist_item(
        &self,
        id: &str,
        item_id: &str,
        completed: bool,
        updated_by: String,
    ) -> Result<OrchestratorTask> {
        self.hub.update_checklist_item(id, item_id, completed, updated_by).await
    }

    async fn add_dependency(
        &self,
        id: &str,
        dependency_id: &str,
        dependency_type: DependencyType,
        updated_by: String,
    ) -> Result<OrchestratorTask> {
        self.hub.add_dependency(id, dependency_id, dependency_type, updated_by).await
    }

    async fn remove_dependency(&self, id: &str, dependency_id: &str, updated_by: String) -> Result<OrchestratorTask> {
        self.hub.remove_dependency(id, dependency_id, updated_by).await
    }
}

#[derive(Clone)]
pub struct BuiltinRequirementsProvider<T> {
    hub: Arc<T>,
}

impl<T> BuiltinRequirementsProvider<T>
where
    T: PlanningServiceApi,
{
    #[must_use]
    pub fn new(hub: Arc<T>) -> Self {
        Self { hub }
    }
}

#[async_trait::async_trait]
impl<T> RequirementsProvider for BuiltinRequirementsProvider<T>
where
    T: PlanningServiceApi,
{
    async fn draft_requirements(&self, input: RequirementsDraftInput) -> Result<RequirementsDraftResult> {
        self.hub.draft_requirements(input).await
    }

    async fn list_requirements(&self) -> Result<Vec<RequirementItem>> {
        self.hub.list_requirements().await
    }

    async fn get_requirement(&self, id: &str) -> Result<RequirementItem> {
        self.hub.get_requirement(id).await
    }

    async fn refine_requirements(&self, input: RequirementsRefineInput) -> Result<Vec<RequirementItem>> {
        self.hub.refine_requirements(input).await
    }

    async fn upsert_requirement(&self, requirement: RequirementItem) -> Result<RequirementItem> {
        self.hub.upsert_requirement(requirement).await
    }

    async fn delete_requirement(&self, id: &str) -> Result<()> {
        self.hub.delete_requirement(id).await
    }

    async fn execute_requirements(&self, input: RequirementsExecutionInput) -> Result<RequirementsExecutionResult> {
        self.hub.execute_requirements(input).await
    }
}
