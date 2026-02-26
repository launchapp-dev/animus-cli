use super::*;

#[async_trait]
impl TaskServiceApi for InMemoryServiceHub {
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
        let now = Utc::now();
        let mut lock = self.state.write().await;
        let id = next_task_id(&lock.tasks);
        let created_by = input.created_by.unwrap_or_else(|| "ao-cli".to_string());
        validate_linked_architecture_entities(
            &lock.architecture,
            &input.linked_architecture_entities,
        )?;
        let task = OrchestratorTask {
            id: id.clone(),
            title: input.title,
            description: input.description,
            task_type: input.task_type.unwrap_or(TaskType::Feature),
            status: TaskStatus::Backlog,
            blocked_reason: None,
            blocked_at: None,
            blocked_phase: None,
            blocked_by: None,
            priority: input.priority.unwrap_or(Priority::Medium),
            risk: RiskLevel::Medium,
            scope: Scope::Medium,
            complexity: Complexity::Medium,
            impact_area: Vec::new(),
            assignee: Assignee::Unassigned,
            estimated_effort: None,
            linked_requirements: input.linked_requirements,
            linked_architecture_entities: input.linked_architecture_entities,
            dependencies: Vec::new(),
            checklist: Vec::new(),
            tags: input.tags,
            workflow_metadata: WorkflowMetadata::default(),
            worktree_path: None,
            branch_name: None,
            metadata: TaskMetadata {
                created_at: now,
                updated_at: now,
                created_by: created_by.clone(),
                updated_by: created_by,
                started_at: None,
                completed_at: None,
                version: 1,
            },
            deadline: None,
            paused: false,
            cancelled: false,
            resource_requirements: Default::default(),
        };

        lock.tasks.insert(id, task.clone());
        Ok(task)
    }

    async fn update(&self, id: &str, input: TaskUpdateInput) -> Result<OrchestratorTask> {
        let mut lock = self.state.write().await;
        if let Some(entity_ids) = input.linked_architecture_entities.as_ref() {
            validate_linked_architecture_entities(&lock.architecture, entity_ids)?;
        }
        let task = lock
            .tasks
            .get_mut(id)
            .ok_or_else(|| anyhow!("task not found: {id}"))?;
        apply_task_update(task, input);
        Ok(task.clone())
    }

    async fn replace(&self, task: OrchestratorTask) -> Result<OrchestratorTask> {
        let mut task = task;
        task.metadata.updated_at = Utc::now();
        task.metadata.version = task.metadata.version.saturating_add(1);
        self.state
            .write()
            .await
            .tasks
            .insert(task.id.clone(), task.clone());
        Ok(task)
    }

    async fn delete(&self, id: &str) -> Result<()> {
        self.state
            .write()
            .await
            .tasks
            .remove(id)
            .ok_or_else(|| anyhow!("task not found: {id}"))?;
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
        let mut lock = self.state.write().await;
        let task = lock
            .tasks
            .get_mut(id)
            .ok_or_else(|| anyhow!("task not found: {id}"))?;
        task.assignee = Assignee::Agent { role, model };
        task.metadata.updated_at = Utc::now();
        task.metadata.updated_by = updated_by;
        task.metadata.version = task.metadata.version.saturating_add(1);
        Ok(task.clone())
    }

    async fn assign_human(
        &self,
        id: &str,
        user_id: String,
        updated_by: String,
    ) -> Result<OrchestratorTask> {
        let mut lock = self.state.write().await;
        let task = lock
            .tasks
            .get_mut(id)
            .ok_or_else(|| anyhow!("task not found: {id}"))?;
        task.assignee = Assignee::Human { user_id };
        task.metadata.updated_at = Utc::now();
        task.metadata.updated_by = updated_by;
        task.metadata.version = task.metadata.version.saturating_add(1);
        Ok(task.clone())
    }

    async fn set_status(&self, id: &str, status: TaskStatus) -> Result<OrchestratorTask> {
        let mut lock = self.state.write().await;
        let task = lock
            .tasks
            .get_mut(id)
            .ok_or_else(|| anyhow!("task not found: {id}"))?;
        apply_task_status(task, status);
        task.metadata.updated_at = Utc::now();
        task.metadata.version = task.metadata.version.saturating_add(1);
        Ok(task.clone())
    }

    async fn add_checklist_item(
        &self,
        id: &str,
        description: String,
        updated_by: String,
    ) -> Result<OrchestratorTask> {
        let mut lock = self.state.write().await;
        let task = lock
            .tasks
            .get_mut(id)
            .ok_or_else(|| anyhow!("task not found: {id}"))?;
        task.checklist.push(ChecklistItem {
            id: Uuid::new_v4().to_string(),
            description,
            completed: false,
            created_at: Utc::now(),
            completed_at: None,
        });
        task.metadata.updated_at = Utc::now();
        task.metadata.updated_by = updated_by;
        task.metadata.version = task.metadata.version.saturating_add(1);
        Ok(task.clone())
    }

    async fn update_checklist_item(
        &self,
        id: &str,
        item_id: &str,
        completed: bool,
        updated_by: String,
    ) -> Result<OrchestratorTask> {
        let mut lock = self.state.write().await;
        let task = lock
            .tasks
            .get_mut(id)
            .ok_or_else(|| anyhow!("task not found: {id}"))?;
        let item = task
            .checklist
            .iter_mut()
            .find(|item| item.id == item_id)
            .ok_or_else(|| anyhow!("checklist item not found: {item_id}"))?;
        item.completed = completed;
        item.completed_at = if completed { Some(Utc::now()) } else { None };
        task.metadata.updated_at = Utc::now();
        task.metadata.updated_by = updated_by;
        task.metadata.version = task.metadata.version.saturating_add(1);
        Ok(task.clone())
    }

    async fn add_dependency(
        &self,
        id: &str,
        dependency_id: &str,
        dependency_type: DependencyType,
        updated_by: String,
    ) -> Result<OrchestratorTask> {
        let mut lock = self.state.write().await;
        if !lock.tasks.contains_key(dependency_id) {
            return Err(anyhow!("dependency task not found: {dependency_id}"));
        }

        let task = lock
            .tasks
            .get_mut(id)
            .ok_or_else(|| anyhow!("task not found: {id}"))?;

        if !task.dependencies.iter().any(|existing| {
            existing.task_id == dependency_id && existing.dependency_type == dependency_type
        }) {
            task.dependencies.push(TaskDependency {
                task_id: dependency_id.to_string(),
                dependency_type,
            });
        }

        task.metadata.updated_at = Utc::now();
        task.metadata.updated_by = updated_by;
        task.metadata.version = task.metadata.version.saturating_add(1);
        Ok(task.clone())
    }

    async fn remove_dependency(
        &self,
        id: &str,
        dependency_id: &str,
        updated_by: String,
    ) -> Result<OrchestratorTask> {
        let mut lock = self.state.write().await;
        let task = lock
            .tasks
            .get_mut(id)
            .ok_or_else(|| anyhow!("task not found: {id}"))?;

        task.dependencies
            .retain(|dependency| dependency.task_id != dependency_id);
        task.metadata.updated_at = Utc::now();
        task.metadata.updated_by = updated_by;
        task.metadata.version = task.metadata.version.saturating_add(1);
        Ok(task.clone())
    }
}

#[async_trait]
impl TaskServiceApi for FileServiceHub {
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
        let TaskCreateInput {
            title,
            description,
            task_type,
            priority,
            created_by,
            tags,
            linked_requirements,
            linked_architecture_entities,
        } = input;
        let now = Utc::now();
        let created_by = created_by.unwrap_or_else(|| "ao-cli".to_string());
        let (task, _) = self
            .mutate_persistent_state(|state| {
                let id = next_task_id(&state.tasks);
                validate_linked_architecture_entities(
                    &state.architecture,
                    &linked_architecture_entities,
                )?;
                let task = OrchestratorTask {
                    id: id.clone(),
                    title,
                    description,
                    task_type: task_type.unwrap_or(TaskType::Feature),
                    status: TaskStatus::Backlog,
                    blocked_reason: None,
                    blocked_at: None,
                    blocked_phase: None,
                    blocked_by: None,
                    priority: priority.unwrap_or(Priority::Medium),
                    risk: RiskLevel::Medium,
                    scope: Scope::Medium,
                    complexity: Complexity::Medium,
                    impact_area: Vec::new(),
                    assignee: Assignee::Unassigned,
                    estimated_effort: None,
                    linked_requirements,
                    linked_architecture_entities,
                    dependencies: Vec::new(),
                    checklist: Vec::new(),
                    tags,
                    workflow_metadata: WorkflowMetadata::default(),
                    worktree_path: None,
                    branch_name: None,
                    metadata: TaskMetadata {
                        created_at: now,
                        updated_at: now,
                        created_by: created_by.clone(),
                        updated_by: created_by.clone(),
                        started_at: None,
                        completed_at: None,
                        version: 1,
                    },
                    deadline: None,
                    paused: false,
                    cancelled: false,
                    resource_requirements: Default::default(),
                };
                state.tasks.insert(id, task.clone());
                Ok(task)
            })
            .await?;
        Ok(task)
    }

    async fn update(&self, id: &str, input: TaskUpdateInput) -> Result<OrchestratorTask> {
        let (task, _) = self
            .mutate_persistent_state(|state| {
                if let Some(entity_ids) = input.linked_architecture_entities.as_ref() {
                    validate_linked_architecture_entities(&state.architecture, entity_ids)?;
                }
                let task = state
                    .tasks
                    .get_mut(id)
                    .ok_or_else(|| anyhow!("task not found: {id}"))?;
                apply_task_update(task, input);
                Ok(task.clone())
            })
            .await?;
        Ok(task)
    }

    async fn replace(&self, task: OrchestratorTask) -> Result<OrchestratorTask> {
        let (task, _) = self
            .mutate_persistent_state(|state| {
                let mut task = task;
                task.metadata.updated_at = Utc::now();
                task.metadata.version = task.metadata.version.saturating_add(1);
                state.tasks.insert(task.id.clone(), task.clone());
                Ok(task)
            })
            .await?;
        Ok(task)
    }

    async fn delete(&self, id: &str) -> Result<()> {
        self.mutate_persistent_state(|state| {
            state
                .tasks
                .remove(id)
                .ok_or_else(|| anyhow!("task not found: {id}"))?;
            Ok(())
        })
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
                let task = state
                    .tasks
                    .get_mut(id)
                    .ok_or_else(|| anyhow!("task not found: {id}"))?;
                task.assignee = Assignee::Agent { role, model };
                task.metadata.updated_at = Utc::now();
                task.metadata.updated_by = updated_by;
                task.metadata.version = task.metadata.version.saturating_add(1);
                Ok(task.clone())
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
                let task = state
                    .tasks
                    .get_mut(id)
                    .ok_or_else(|| anyhow!("task not found: {id}"))?;
                task.assignee = Assignee::Human { user_id };
                task.metadata.updated_at = Utc::now();
                task.metadata.updated_by = updated_by;
                task.metadata.version = task.metadata.version.saturating_add(1);
                Ok(task.clone())
            })
            .await?;
        Ok(task)
    }

    async fn set_status(&self, id: &str, status: TaskStatus) -> Result<OrchestratorTask> {
        let (task, _) = self
            .mutate_persistent_state(|state| {
                let task = state
                    .tasks
                    .get_mut(id)
                    .ok_or_else(|| anyhow!("task not found: {id}"))?;
                apply_task_status(task, status);
                task.metadata.updated_at = Utc::now();
                task.metadata.version = task.metadata.version.saturating_add(1);
                Ok(task.clone())
            })
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
                let task = state
                    .tasks
                    .get_mut(id)
                    .ok_or_else(|| anyhow!("task not found: {id}"))?;
                task.checklist.push(ChecklistItem {
                    id: Uuid::new_v4().to_string(),
                    description,
                    completed: false,
                    created_at: Utc::now(),
                    completed_at: None,
                });
                task.metadata.updated_at = Utc::now();
                task.metadata.updated_by = updated_by;
                task.metadata.version = task.metadata.version.saturating_add(1);
                Ok(task.clone())
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
                let task = state
                    .tasks
                    .get_mut(id)
                    .ok_or_else(|| anyhow!("task not found: {id}"))?;
                let item = task
                    .checklist
                    .iter_mut()
                    .find(|item| item.id == item_id)
                    .ok_or_else(|| anyhow!("checklist item not found: {item_id}"))?;
                item.completed = completed;
                item.completed_at = if completed { Some(Utc::now()) } else { None };
                task.metadata.updated_at = Utc::now();
                task.metadata.updated_by = updated_by;
                task.metadata.version = task.metadata.version.saturating_add(1);
                Ok(task.clone())
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
                if !state.tasks.contains_key(dependency_id) {
                    return Err(anyhow!("dependency task not found: {dependency_id}"));
                }
                let task = state
                    .tasks
                    .get_mut(id)
                    .ok_or_else(|| anyhow!("task not found: {id}"))?;
                if !task.dependencies.iter().any(|existing| {
                    existing.task_id == dependency_id && existing.dependency_type == dependency_type
                }) {
                    task.dependencies.push(TaskDependency {
                        task_id: dependency_id.to_string(),
                        dependency_type,
                    });
                }
                task.metadata.updated_at = Utc::now();
                task.metadata.updated_by = updated_by;
                task.metadata.version = task.metadata.version.saturating_add(1);
                Ok(task.clone())
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
                let task = state
                    .tasks
                    .get_mut(id)
                    .ok_or_else(|| anyhow!("task not found: {id}"))?;
                task.dependencies
                    .retain(|dependency| dependency.task_id != dependency_id);
                task.metadata.updated_at = Utc::now();
                task.metadata.updated_by = updated_by;
                task.metadata.version = task.metadata.version.saturating_add(1);
                Ok(task.clone())
            })
            .await?;
        Ok(task)
    }
}
