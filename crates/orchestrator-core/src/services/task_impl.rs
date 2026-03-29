use super::*;
use orchestrator_providers::TaskServiceApi as ProviderTaskServiceApi;

fn task_filter_can_use_db_predicates(filter: &TaskFilter) -> bool {
    filter.risk.is_none()
        && filter.assignee_type.is_none()
        && filter.tags.is_none()
        && filter.linked_requirement.is_none()
        && filter.linked_architecture_entity.is_none()
        && filter.search_text.is_none()
}

fn task_query_can_use_db_page(query: &TaskQuery) -> bool {
    task_filter_can_use_db_predicates(&query.filter) && query.sort != TaskQuerySort::CreatedAt
}

#[async_trait]
impl TaskServiceApi for InMemoryServiceHub {
    fn task_provider(&self) -> Arc<dyn TaskProvider> {
        Arc::new(crate::providers::BuiltinTaskProvider::new(Arc::new(self.clone())))
    }

    async fn list(&self) -> Result<Vec<OrchestratorTask>> {
        Ok(self.state.read().await.tasks.values().cloned().collect())
    }

    async fn query(&self, query: TaskQuery) -> Result<ListPage<OrchestratorTask>> {
        let tasks = TaskServiceApi::list(self).await?;
        Ok(query_tasks(tasks, &query))
    }

    async fn list_filtered(&self, filter: TaskFilter) -> Result<Vec<OrchestratorTask>> {
        let tasks = TaskServiceApi::list(self).await?;
        Ok(tasks.into_iter().filter(|task| task_matches_filter(task, &filter)).collect())
    }

    async fn list_prioritized(&self) -> Result<Vec<OrchestratorTask>> {
        let mut tasks = TaskServiceApi::list(self).await?;
        sort_tasks_by_priority(&mut tasks);
        Ok(tasks)
    }

    async fn next_task(&self) -> Result<Option<OrchestratorTask>> {
        Ok(TaskServiceApi::list_prioritized(self)
            .await?
            .into_iter()
            .find(|task| matches!(task.status, TaskStatus::Ready | TaskStatus::Backlog)))
    }

    async fn statistics(&self) -> Result<TaskStatistics> {
        let tasks = TaskServiceApi::list(self).await?;
        Ok(build_task_statistics(&tasks))
    }

    async fn get(&self, id: &str) -> Result<OrchestratorTask> {
        self.state.read().await.tasks.get(id).cloned().ok_or_else(|| not_found(format!("task not found: {id}")))
    }

    async fn create(&self, input: TaskCreateInput) -> Result<OrchestratorTask> {
        create_task_in_state(&mut *self.state.write().await, input)
    }

    async fn update(&self, id: &str, input: TaskUpdateInput) -> Result<OrchestratorTask> {
        update_task_in_state(&mut *self.state.write().await, id, input)
    }

    async fn replace(&self, task: OrchestratorTask) -> Result<OrchestratorTask> {
        replace_task_in_state(&mut *self.state.write().await, task)
    }

    async fn delete(&self, id: &str) -> Result<()> {
        delete_task_in_state(&mut *self.state.write().await, id)
    }

    async fn assign(&self, id: &str, assignee: String) -> Result<OrchestratorTask> {
        self.assign_human(id, assignee.clone(), assignee).await
    }

    async fn assign_agent(
        &self,
        id: &str,
        role: String,
        model: Option<String>,
        updated_by: String,
    ) -> Result<OrchestratorTask> {
        assign_agent_in_state(&mut *self.state.write().await, id, role, model, updated_by)
    }

    async fn assign_human(&self, id: &str, user_id: String, updated_by: String) -> Result<OrchestratorTask> {
        assign_human_in_state(&mut *self.state.write().await, id, user_id, updated_by)
    }

    async fn set_status(&self, id: &str, status: TaskStatus, validate: bool) -> Result<OrchestratorTask> {
        set_status_in_state(&mut *self.state.write().await, id, status, validate)
    }

    async fn add_checklist_item(&self, id: &str, description: String, updated_by: String) -> Result<OrchestratorTask> {
        add_checklist_item_in_state(&mut *self.state.write().await, id, description, updated_by)
    }

    async fn update_checklist_item(
        &self,
        id: &str,
        item_id: &str,
        completed: bool,
        updated_by: String,
    ) -> Result<OrchestratorTask> {
        update_checklist_item_in_state(&mut *self.state.write().await, id, item_id, completed, updated_by)
    }

    async fn add_dependency(
        &self,
        id: &str,
        dependency_id: &str,
        dependency_type: DependencyType,
        updated_by: String,
    ) -> Result<OrchestratorTask> {
        add_dependency_in_state(&mut *self.state.write().await, id, dependency_id, dependency_type, updated_by)
    }

    async fn remove_dependency(&self, id: &str, dependency_id: &str, updated_by: String) -> Result<OrchestratorTask> {
        remove_dependency_in_state(&mut *self.state.write().await, id, dependency_id, updated_by)
    }
}

#[async_trait]
impl TaskServiceApi for FileServiceHub {
    fn task_provider(&self) -> Arc<dyn TaskProvider> {
        Arc::new(crate::providers::BuiltinTaskProvider::new(Arc::new(self.clone())))
    }

    async fn list(&self) -> Result<Vec<OrchestratorTask>> {
        Ok(crate::workflow::load_all_tasks(&self.project_root)?.into_values().collect())
    }

    async fn query(&self, query: TaskQuery) -> Result<ListPage<OrchestratorTask>> {
        if task_query_can_use_db_page(&query) {
            let (ids, total) = crate::workflow::query_task_ids(&self.project_root, &query)?;
            let items = crate::workflow::load_tasks_by_ids(&self.project_root, &ids)?;
            return Ok(ListPage::new(items, total, query.page));
        }

        let tasks = TaskServiceApi::list(self).await?;
        Ok(query_tasks(tasks, &query))
    }

    async fn list_filtered(&self, filter: TaskFilter) -> Result<Vec<OrchestratorTask>> {
        if task_filter_can_use_db_predicates(&filter) {
            let query = TaskQuery { filter, page: crate::ListPageRequest::unbounded(), sort: TaskQuerySort::Id };
            let (ids, _) = crate::workflow::query_task_ids(&self.project_root, &query)?;
            return crate::workflow::load_tasks_by_ids(&self.project_root, &ids);
        }

        let tasks = TaskServiceApi::list(self).await?;
        Ok(tasks.into_iter().filter(|task| task_matches_filter(task, &filter)).collect())
    }

    async fn list_prioritized(&self) -> Result<Vec<OrchestratorTask>> {
        let query = TaskQuery {
            filter: TaskFilter::default(),
            page: crate::ListPageRequest::unbounded(),
            sort: TaskQuerySort::Priority,
        };
        let (ids, _) = crate::workflow::query_task_ids(&self.project_root, &query)?;
        crate::workflow::load_tasks_by_ids(&self.project_root, &ids)
    }

    async fn next_task(&self) -> Result<Option<OrchestratorTask>> {
        crate::workflow::load_next_task_by_priority(&self.project_root)
    }

    async fn statistics(&self) -> Result<TaskStatistics> {
        crate::workflow::load_task_statistics(&self.project_root)
    }

    async fn get(&self, id: &str) -> Result<OrchestratorTask> {
        crate::workflow::load_task(&self.project_root, id)
    }

    async fn create(&self, input: TaskCreateInput) -> Result<OrchestratorTask> {
        let (task, _) = self.mutate_persistent_state(|state| create_task_in_state(state, input)).await?;
        orchestrator_logging::Logger::for_project(&self.project_root)
            .info("task.create", format!("{}: {}", task.id, task.title.chars().take(60).collect::<String>()))
            .task(&task.id)
            .status(task.status.to_string())
            .emit();
        Ok(task)
    }

    async fn update(&self, id: &str, input: TaskUpdateInput) -> Result<OrchestratorTask> {
        let (task, _) = self.mutate_persistent_state(|state| update_task_in_state(state, id, input)).await?;
        Ok(task)
    }

    async fn replace(&self, task: OrchestratorTask) -> Result<OrchestratorTask> {
        let (task, _) = self.mutate_persistent_state(|state| replace_task_in_state(state, task)).await?;
        Ok(task)
    }

    async fn delete(&self, id: &str) -> Result<()> {
        self.mutate_persistent_state(|state| delete_task_in_state(state, id)).await?;
        Ok(())
    }

    async fn assign(&self, id: &str, assignee: String) -> Result<OrchestratorTask> {
        self.assign_human(id, assignee.clone(), assignee).await
    }

    async fn assign_agent(
        &self,
        id: &str,
        role: String,
        model: Option<String>,
        updated_by: String,
    ) -> Result<OrchestratorTask> {
        let (task, _) =
            self.mutate_persistent_state(|state| assign_agent_in_state(state, id, role, model, updated_by)).await?;
        Ok(task)
    }

    async fn assign_human(&self, id: &str, user_id: String, updated_by: String) -> Result<OrchestratorTask> {
        let (task, _) =
            self.mutate_persistent_state(|state| assign_human_in_state(state, id, user_id, updated_by)).await?;
        Ok(task)
    }

    async fn set_status(&self, id: &str, status: TaskStatus, validate: bool) -> Result<OrchestratorTask> {
        let (task, _) = self.mutate_persistent_state(|state| set_status_in_state(state, id, status, validate)).await?;
        orchestrator_logging::Logger::for_project(&self.project_root)
            .info("task.status", format!("{} → {}", id, task.status))
            .task(id)
            .status(task.status.to_string())
            .emit();
        Ok(task)
    }

    async fn add_checklist_item(&self, id: &str, description: String, updated_by: String) -> Result<OrchestratorTask> {
        let (task, _) = self
            .mutate_persistent_state(|state| add_checklist_item_in_state(state, id, description, updated_by))
            .await?;
        Ok(task)
    }

    async fn update_checklist_item(
        &self,
        id: &str,
        item_id: &str,
        completed: bool,
        updated_by: String,
    ) -> Result<OrchestratorTask> {
        let (task, _) = self
            .mutate_persistent_state(|state| update_checklist_item_in_state(state, id, item_id, completed, updated_by))
            .await?;
        Ok(task)
    }

    async fn add_dependency(
        &self,
        id: &str,
        dependency_id: &str,
        dependency_type: DependencyType,
        updated_by: String,
    ) -> Result<OrchestratorTask> {
        let (task, _) = self
            .mutate_persistent_state(|state| {
                add_dependency_in_state(state, id, dependency_id, dependency_type, updated_by)
            })
            .await?;
        Ok(task)
    }

    async fn remove_dependency(&self, id: &str, dependency_id: &str, updated_by: String) -> Result<OrchestratorTask> {
        let (task, _) = self
            .mutate_persistent_state(|state| remove_dependency_in_state(state, id, dependency_id, updated_by))
            .await?;
        Ok(task)
    }
}

#[async_trait]
impl ProviderTaskServiceApi for InMemoryServiceHub {
    async fn list(&self) -> Result<Vec<OrchestratorTask>> {
        TaskServiceApi::list(self).await
    }

    async fn list_filtered(&self, filter: TaskFilter) -> Result<Vec<OrchestratorTask>> {
        TaskServiceApi::list_filtered(self, filter).await
    }

    async fn list_prioritized(&self) -> Result<Vec<OrchestratorTask>> {
        TaskServiceApi::list_prioritized(self).await
    }

    async fn next_task(&self) -> Result<Option<OrchestratorTask>> {
        TaskServiceApi::next_task(self).await
    }

    async fn statistics(&self) -> Result<TaskStatistics> {
        TaskServiceApi::statistics(self).await
    }

    async fn get(&self, id: &str) -> Result<OrchestratorTask> {
        TaskServiceApi::get(self, id).await
    }

    async fn create(&self, input: TaskCreateInput) -> Result<OrchestratorTask> {
        TaskServiceApi::create(self, input).await
    }

    async fn update(&self, id: &str, input: TaskUpdateInput) -> Result<OrchestratorTask> {
        TaskServiceApi::update(self, id, input).await
    }

    async fn replace(&self, task: OrchestratorTask) -> Result<OrchestratorTask> {
        TaskServiceApi::replace(self, task).await
    }

    async fn delete(&self, id: &str) -> Result<()> {
        TaskServiceApi::delete(self, id).await
    }

    async fn assign(&self, id: &str, assignee: String) -> Result<OrchestratorTask> {
        TaskServiceApi::assign(self, id, assignee).await
    }

    async fn set_status(&self, id: &str, status: TaskStatus, validate: bool) -> Result<OrchestratorTask> {
        TaskServiceApi::set_status(self, id, status, validate).await
    }

    async fn add_checklist_item(&self, id: &str, description: String, updated_by: String) -> Result<OrchestratorTask> {
        TaskServiceApi::add_checklist_item(self, id, description, updated_by).await
    }

    async fn update_checklist_item(
        &self,
        id: &str,
        item_id: &str,
        completed: bool,
        updated_by: String,
    ) -> Result<OrchestratorTask> {
        TaskServiceApi::update_checklist_item(self, id, item_id, completed, updated_by).await
    }

    async fn add_dependency(
        &self,
        id: &str,
        dependency_id: &str,
        dependency_type: DependencyType,
        updated_by: String,
    ) -> Result<OrchestratorTask> {
        TaskServiceApi::add_dependency(self, id, dependency_id, dependency_type, updated_by).await
    }

    async fn remove_dependency(&self, id: &str, dependency_id: &str, updated_by: String) -> Result<OrchestratorTask> {
        TaskServiceApi::remove_dependency(self, id, dependency_id, updated_by).await
    }
}

#[async_trait]
impl ProviderTaskServiceApi for FileServiceHub {
    async fn list(&self) -> Result<Vec<OrchestratorTask>> {
        TaskServiceApi::list(self).await
    }

    async fn list_filtered(&self, filter: TaskFilter) -> Result<Vec<OrchestratorTask>> {
        TaskServiceApi::list_filtered(self, filter).await
    }

    async fn list_prioritized(&self) -> Result<Vec<OrchestratorTask>> {
        TaskServiceApi::list_prioritized(self).await
    }

    async fn next_task(&self) -> Result<Option<OrchestratorTask>> {
        TaskServiceApi::next_task(self).await
    }

    async fn statistics(&self) -> Result<TaskStatistics> {
        TaskServiceApi::statistics(self).await
    }

    async fn get(&self, id: &str) -> Result<OrchestratorTask> {
        TaskServiceApi::get(self, id).await
    }

    async fn create(&self, input: TaskCreateInput) -> Result<OrchestratorTask> {
        TaskServiceApi::create(self, input).await
    }

    async fn update(&self, id: &str, input: TaskUpdateInput) -> Result<OrchestratorTask> {
        TaskServiceApi::update(self, id, input).await
    }

    async fn replace(&self, task: OrchestratorTask) -> Result<OrchestratorTask> {
        TaskServiceApi::replace(self, task).await
    }

    async fn delete(&self, id: &str) -> Result<()> {
        TaskServiceApi::delete(self, id).await
    }

    async fn assign(&self, id: &str, assignee: String) -> Result<OrchestratorTask> {
        TaskServiceApi::assign(self, id, assignee).await
    }

    async fn set_status(&self, id: &str, status: TaskStatus, validate: bool) -> Result<OrchestratorTask> {
        TaskServiceApi::set_status(self, id, status, validate).await
    }

    async fn add_checklist_item(&self, id: &str, description: String, updated_by: String) -> Result<OrchestratorTask> {
        TaskServiceApi::add_checklist_item(self, id, description, updated_by).await
    }

    async fn update_checklist_item(
        &self,
        id: &str,
        item_id: &str,
        completed: bool,
        updated_by: String,
    ) -> Result<OrchestratorTask> {
        TaskServiceApi::update_checklist_item(self, id, item_id, completed, updated_by).await
    }

    async fn add_dependency(
        &self,
        id: &str,
        dependency_id: &str,
        dependency_type: DependencyType,
        updated_by: String,
    ) -> Result<OrchestratorTask> {
        TaskServiceApi::add_dependency(self, id, dependency_id, dependency_type, updated_by).await
    }

    async fn remove_dependency(&self, id: &str, dependency_id: &str, updated_by: String) -> Result<OrchestratorTask> {
        TaskServiceApi::remove_dependency(self, id, dependency_id, updated_by).await
    }
}
