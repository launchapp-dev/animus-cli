use super::*;

#[async_trait]
impl TaskServiceApi for InMemoryServiceHub {
    fn task_provider(&self) -> Arc<dyn TaskProvider> {
        Arc::new(crate::providers::BuiltinTaskProvider::new(Arc::new(self.clone())))
    }

    async fn list(&self) -> Result<Vec<OrchestratorTask>> {
        Ok(self.state.read().await.tasks.values().cloned().collect())
    }

    async fn list_filtered(&self, filter: TaskFilter) -> Result<Vec<OrchestratorTask>> {
        let tasks = TaskServiceApi::list(self).await?;
        Ok(tasks
            .into_iter()
            .filter(|task| task_matches_filter(task, &filter))
            .collect())
    }

    async fn list_prioritized(&self) -> Result<Vec<OrchestratorTask>> {
        let mut tasks = TaskServiceApi::list(self).await?;
        sort_tasks_by_priority(&mut tasks);
        Ok(tasks)
    }

    async fn next_task(&self) -> Result<Option<OrchestratorTask>> {
        Ok(self
            .list_prioritized()
            .await?
            .into_iter()
            .find(|task| matches!(task.status, TaskStatus::Ready | TaskStatus::Backlog)))
    }

    async fn statistics(&self) -> Result<TaskStatistics> {
        let tasks = TaskServiceApi::list(self).await?;
        Ok(build_task_statistics(&tasks))
    }

    async fn get(&self, id: &str) -> Result<OrchestratorTask> {
        self.state
            .read()
            .await
            .tasks
            .get(id)
            .cloned()
            .ok_or_else(|| anyhow!("task not found: {id}"))
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

    async fn assign_human(
        &self,
        id: &str,
        user_id: String,
        updated_by: String,
    ) -> Result<OrchestratorTask> {
        assign_human_in_state(&mut *self.state.write().await, id, user_id, updated_by)
    }

    async fn set_status(&self, id: &str, status: TaskStatus, validate: bool) -> Result<OrchestratorTask> {
        set_status_in_state(&mut *self.state.write().await, id, status, validate)
    }

    async fn add_checklist_item(
        &self,
        id: &str,
        description: String,
        updated_by: String,
    ) -> Result<OrchestratorTask> {
        add_checklist_item_in_state(&mut *self.state.write().await, id, description, updated_by)
    }

    async fn update_checklist_item(
        &self,
        id: &str,
        item_id: &str,
        completed: bool,
        updated_by: String,
    ) -> Result<OrchestratorTask> {
        update_checklist_item_in_state(
            &mut *self.state.write().await,
            id,
            item_id,
            completed,
            updated_by,
        )
    }

    async fn add_dependency(
        &self,
        id: &str,
        dependency_id: &str,
        dependency_type: DependencyType,
        updated_by: String,
    ) -> Result<OrchestratorTask> {
        add_dependency_in_state(
            &mut *self.state.write().await,
            id,
            dependency_id,
            dependency_type,
            updated_by,
        )
    }

    async fn remove_dependency(
        &self,
        id: &str,
        dependency_id: &str,
        updated_by: String,
    ) -> Result<OrchestratorTask> {
        remove_dependency_in_state(&mut *self.state.write().await, id, dependency_id, updated_by)
    }
}

#[async_trait]
impl TaskServiceApi for FileServiceHub {
    fn task_provider(&self) -> Arc<dyn TaskProvider> {
        Arc::new(crate::providers::BuiltinTaskProvider::new(Arc::new(self.clone())))
    }

    async fn list(&self) -> Result<Vec<OrchestratorTask>> {
        Ok(self.state.read().await.tasks.values().cloned().collect())
    }

    async fn list_filtered(&self, filter: TaskFilter) -> Result<Vec<OrchestratorTask>> {
        let tasks = TaskServiceApi::list(self).await?;
        Ok(tasks
            .into_iter()
            .filter(|task| task_matches_filter(task, &filter))
            .collect())
    }

    async fn list_prioritized(&self) -> Result<Vec<OrchestratorTask>> {
        let mut tasks = TaskServiceApi::list(self).await?;
        sort_tasks_by_priority(&mut tasks);
        Ok(tasks)
    }

    async fn next_task(&self) -> Result<Option<OrchestratorTask>> {
        Ok(self
            .list_prioritized()
            .await?
            .into_iter()
            .find(|task| matches!(task.status, TaskStatus::Ready | TaskStatus::Backlog)))
    }

    async fn statistics(&self) -> Result<TaskStatistics> {
        let tasks = TaskServiceApi::list(self).await?;
        Ok(build_task_statistics(&tasks))
    }

    async fn get(&self, id: &str) -> Result<OrchestratorTask> {
        self.state
            .read()
            .await
            .tasks
            .get(id)
            .cloned()
            .ok_or_else(|| anyhow!("task not found: {id}"))
    }

    async fn create(&self, input: TaskCreateInput) -> Result<OrchestratorTask> {
        let (task, _) = self
            .mutate_persistent_state(|state| create_task_in_state(state, input))
            .await?;
        Ok(task)
    }

    async fn update(&self, id: &str, input: TaskUpdateInput) -> Result<OrchestratorTask> {
        let (task, _) = self
            .mutate_persistent_state(|state| update_task_in_state(state, id, input))
            .await?;
        Ok(task)
    }

    async fn replace(&self, task: OrchestratorTask) -> Result<OrchestratorTask> {
        let (task, _) = self
            .mutate_persistent_state(|state| replace_task_in_state(state, task))
            .await?;
        Ok(task)
    }

    async fn delete(&self, id: &str) -> Result<()> {
        self.mutate_persistent_state(|state| delete_task_in_state(state, id))
            .await?;
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
        let (task, _) = self
            .mutate_persistent_state(|state| {
                assign_agent_in_state(state, id, role, model, updated_by)
            })
            .await?;
        Ok(task)
    }

    async fn assign_human(
        &self,
        id: &str,
        user_id: String,
        updated_by: String,
    ) -> Result<OrchestratorTask> {
        let (task, _) = self
            .mutate_persistent_state(|state| {
                assign_human_in_state(state, id, user_id, updated_by)
            })
            .await?;
        Ok(task)
    }

    async fn set_status(&self, id: &str, status: TaskStatus, validate: bool) -> Result<OrchestratorTask> {
        let (task, _) = self
            .mutate_persistent_state(|state| set_status_in_state(state, id, status, validate))
            .await?;
        Ok(task)
    }

    async fn add_checklist_item(
        &self,
        id: &str,
        description: String,
        updated_by: String,
    ) -> Result<OrchestratorTask> {
        let (task, _) = self
            .mutate_persistent_state(|state| {
                add_checklist_item_in_state(state, id, description, updated_by)
            })
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
            .mutate_persistent_state(|state| {
                update_checklist_item_in_state(state, id, item_id, completed, updated_by)
            })
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

    async fn remove_dependency(
        &self,
        id: &str,
        dependency_id: &str,
        updated_by: String,
    ) -> Result<OrchestratorTask> {
        let (task, _) = self
            .mutate_persistent_state(|state| {
                remove_dependency_in_state(state, id, dependency_id, updated_by)
            })
            .await?;
        Ok(task)
    }
}
