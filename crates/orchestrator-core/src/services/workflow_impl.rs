use super::*;

fn effective_pipeline_id(
    requested: Option<&str>,
    task: Option<&crate::types::OrchestratorTask>,
) -> String {
    if let Some(pipeline_id) = requested
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
    {
        return pipeline_id;
    }

    if task.map(|task| task.is_frontend_related()).unwrap_or(false) {
        return crate::workflow::UI_UX_PIPELINE_ID.to_string();
    }

    crate::workflow::STANDARD_PIPELINE_ID.to_string()
}

fn load_compiled_state_machines(
    project_root: &std::path::Path,
) -> Result<crate::state_machines::CompiledStateMachines> {
    let loaded = crate::state_machines::load_state_machines_for_project(project_root)?;
    for warning in &loaded.warnings {
        tracing::warn!(
            target: "orchestrator_core::state_machines",
            code = %warning.code,
            message = %warning.message,
            source = %loaded.compiled.metadata.source.as_str(),
            hash = %loaded.compiled.metadata.hash,
            version = loaded.compiled.metadata.version,
            path = %loaded.path.display(),
            "state machine fallback"
        );
    }
    Ok(loaded.compiled)
}

#[async_trait]
impl WorkflowServiceApi for InMemoryServiceHub {
    async fn list(&self) -> Result<Vec<OrchestratorWorkflow>> {
        Ok(self
            .state
            .read()
            .await
            .workflows
            .values()
            .cloned()
            .collect())
    }

    async fn get(&self, id: &str) -> Result<OrchestratorWorkflow> {
        self.state
            .read()
            .await
            .workflows
            .get(id)
            .cloned()
            .ok_or_else(|| anyhow!("workflow not found: {id}"))
    }

    async fn decisions(&self, id: &str) -> Result<Vec<crate::types::WorkflowDecisionRecord>> {
        Ok(WorkflowServiceApi::get(self, id).await?.decision_history)
    }

    async fn list_checkpoints(&self, id: &str) -> Result<Vec<usize>> {
        let workflow = WorkflowServiceApi::get(self, id).await?;
        Ok(workflow
            .checkpoint_metadata
            .checkpoints
            .iter()
            .map(|checkpoint| checkpoint.number)
            .collect())
    }

    async fn get_checkpoint(
        &self,
        id: &str,
        checkpoint_number: usize,
    ) -> Result<OrchestratorWorkflow> {
        let workflow = WorkflowServiceApi::get(self, id).await?;
        if workflow
            .checkpoint_metadata
            .checkpoints
            .iter()
            .any(|checkpoint| checkpoint.number == checkpoint_number)
        {
            Ok(workflow)
        } else {
            Err(anyhow!("checkpoint not found: {id} #{checkpoint_number}"))
        }
    }

    async fn run(&self, input: WorkflowRunInput) -> Result<OrchestratorWorkflow> {
        let id = Uuid::new_v4().to_string();
        let workflow = {
            let mut lock = self.state.write().await;
            let task = lock.tasks.get(&input.task_id).cloned();
            let pipeline_id = effective_pipeline_id(input.pipeline_id.as_deref(), task.as_ref());
            let executor = WorkflowLifecycleExecutor::new(crate::resolve_phase_plan_for_pipeline(
                None,
                Some(pipeline_id.as_str()),
            )?);
            let workflow = executor.bootstrap(
                id.clone(),
                WorkflowRunInput {
                    task_id: input.task_id,
                    pipeline_id: Some(pipeline_id),
                },
            );
            lock.workflows.insert(id.clone(), workflow.clone());
            workflow
        };
        Ok(workflow)
    }

    async fn resume(&self, id: &str) -> Result<OrchestratorWorkflow> {
        let mut lock = self.state.write().await;
        let workflow = lock
            .workflows
            .get_mut(id)
            .ok_or_else(|| anyhow!("workflow not found: {id}"))?;
        let executor = WorkflowLifecycleExecutor::default();
        executor.resume(workflow);
        Ok(workflow.clone())
    }

    async fn pause(&self, id: &str) -> Result<OrchestratorWorkflow> {
        let mut lock = self.state.write().await;
        let workflow = lock
            .workflows
            .get_mut(id)
            .ok_or_else(|| anyhow!("workflow not found: {id}"))?;
        WorkflowLifecycleExecutor::default().pause(workflow);
        Ok(workflow.clone())
    }

    async fn cancel(&self, id: &str) -> Result<OrchestratorWorkflow> {
        let mut lock = self.state.write().await;
        let workflow = lock
            .workflows
            .get_mut(id)
            .ok_or_else(|| anyhow!("workflow not found: {id}"))?;
        WorkflowLifecycleExecutor::default().cancel(workflow);
        Ok(workflow.clone())
    }

    async fn request_research(&self, id: &str, reason: String) -> Result<OrchestratorWorkflow> {
        let mut lock = self.state.write().await;
        let workflow = lock
            .workflows
            .get_mut(id)
            .ok_or_else(|| anyhow!("workflow not found: {id}"))?;
        WorkflowLifecycleExecutor::default().request_research_phase(workflow, reason);
        Ok(workflow.clone())
    }

    async fn complete_current_phase(&self, id: &str) -> Result<OrchestratorWorkflow> {
        let mut lock = self.state.write().await;
        let workflow = lock
            .workflows
            .get_mut(id)
            .ok_or_else(|| anyhow!("workflow not found: {id}"))?;
        WorkflowLifecycleExecutor::default().mark_current_phase_success(workflow);
        Ok(workflow.clone())
    }

    async fn fail_current_phase(&self, id: &str, error: String) -> Result<OrchestratorWorkflow> {
        let mut lock = self.state.write().await;
        let workflow = lock
            .workflows
            .get_mut(id)
            .ok_or_else(|| anyhow!("workflow not found: {id}"))?;
        WorkflowLifecycleExecutor::default().mark_current_phase_failed(workflow, error);
        Ok(workflow.clone())
    }

    async fn mark_merge_conflict(&self, id: &str, error: String) -> Result<OrchestratorWorkflow> {
        let mut lock = self.state.write().await;
        let workflow = lock
            .workflows
            .get_mut(id)
            .ok_or_else(|| anyhow!("workflow not found: {id}"))?;
        WorkflowLifecycleExecutor::default().mark_merge_conflict(workflow, error);
        Ok(workflow.clone())
    }

    async fn resolve_merge_conflict(&self, id: &str) -> Result<OrchestratorWorkflow> {
        let mut lock = self.state.write().await;
        let workflow = lock
            .workflows
            .get_mut(id)
            .ok_or_else(|| anyhow!("workflow not found: {id}"))?;
        WorkflowLifecycleExecutor::default().resolve_merge_conflict(workflow);
        Ok(workflow.clone())
    }
}

#[async_trait]
impl WorkflowServiceApi for FileServiceHub {
    async fn list(&self) -> Result<Vec<OrchestratorWorkflow>> {
        let workflows = self.workflow_manager().list()?;

        self.mutate_persistent_state(|state| {
            state.workflows = workflows
                .iter()
                .cloned()
                .map(|workflow| (workflow.id.clone(), workflow))
                .collect();
            Ok(())
        })
        .await?;

        Ok(workflows)
    }

    async fn get(&self, id: &str) -> Result<OrchestratorWorkflow> {
        if let Ok(workflow) = self.workflow_manager().load(id) {
            return Ok(workflow);
        }

        self.state
            .read()
            .await
            .workflows
            .get(id)
            .cloned()
            .ok_or_else(|| anyhow!("workflow not found: {id}"))
    }

    async fn decisions(&self, id: &str) -> Result<Vec<crate::types::WorkflowDecisionRecord>> {
        Ok(WorkflowServiceApi::get(self, id).await?.decision_history)
    }

    async fn list_checkpoints(&self, id: &str) -> Result<Vec<usize>> {
        self.workflow_manager().list_checkpoints(id)
    }

    async fn get_checkpoint(
        &self,
        id: &str,
        checkpoint_number: usize,
    ) -> Result<OrchestratorWorkflow> {
        self.workflow_manager()
            .load_checkpoint(id, checkpoint_number)
    }

    async fn run(&self, input: WorkflowRunInput) -> Result<OrchestratorWorkflow> {
        let id = Uuid::new_v4().to_string();
        let state_machines = load_compiled_state_machines(self.project_root.as_path())?;
        let task = self.state.read().await.tasks.get(&input.task_id).cloned();
        let pipeline_id = effective_pipeline_id(input.pipeline_id.as_deref(), task.as_ref());
        let executor = WorkflowLifecycleExecutor::with_state_machines(
            crate::resolve_phase_plan_for_pipeline(
                Some(self.project_root.as_path()),
                Some(pipeline_id.as_str()),
            )?,
            state_machines,
        );
        let workflow = executor.bootstrap(
            id.clone(),
            WorkflowRunInput {
                task_id: input.task_id,
                pipeline_id: Some(pipeline_id),
            },
        );

        let manager = self.workflow_manager();
        manager.save(&workflow)?;
        let workflow = manager.save_checkpoint(&workflow, CheckpointReason::Start)?;

        self.mutate_persistent_state(|state| {
            state.workflows.insert(id, workflow.clone());
            Ok(())
        })
        .await?;
        Ok(workflow)
    }

    async fn resume(&self, id: &str) -> Result<OrchestratorWorkflow> {
        let manager = self.workflow_manager();
        let mut workflow = manager.load(id)?;
        let state_machines = load_compiled_state_machines(self.project_root.as_path())?;
        let executor = WorkflowLifecycleExecutor::with_state_machines(
            crate::resolve_phase_plan_for_pipeline(
                Some(self.project_root.as_path()),
                workflow.pipeline_id.as_deref(),
            )?,
            state_machines,
        );
        executor.resume(&mut workflow);
        manager.save(&workflow)?;
        let workflow = manager.save_checkpoint(&workflow, CheckpointReason::Resume)?;

        self.mutate_persistent_state(|state| {
            state.workflows.insert(id.to_string(), workflow.clone());
            Ok(())
        })
        .await?;
        Ok(workflow)
    }

    async fn pause(&self, id: &str) -> Result<OrchestratorWorkflow> {
        let manager = self.workflow_manager();
        let mut workflow = manager.load(id)?;
        let state_machines = load_compiled_state_machines(self.project_root.as_path())?;
        WorkflowLifecycleExecutor::with_state_machines(
            crate::resolve_phase_plan_for_pipeline(
                Some(self.project_root.as_path()),
                workflow.pipeline_id.as_deref(),
            )?,
            state_machines,
        )
        .pause(&mut workflow);
        manager.save(&workflow)?;
        let workflow = manager.save_checkpoint(&workflow, CheckpointReason::Pause)?;

        self.mutate_persistent_state(|state| {
            state.workflows.insert(id.to_string(), workflow.clone());
            Ok(())
        })
        .await?;
        Ok(workflow)
    }

    async fn cancel(&self, id: &str) -> Result<OrchestratorWorkflow> {
        let manager = self.workflow_manager();
        let mut workflow = manager.load(id)?;
        let state_machines = load_compiled_state_machines(self.project_root.as_path())?;
        WorkflowLifecycleExecutor::with_state_machines(
            crate::resolve_phase_plan_for_pipeline(
                Some(self.project_root.as_path()),
                workflow.pipeline_id.as_deref(),
            )?,
            state_machines,
        )
        .cancel(&mut workflow);
        manager.save(&workflow)?;
        let workflow = manager.save_checkpoint(&workflow, CheckpointReason::Cancel)?;

        self.mutate_persistent_state(|state| {
            state.workflows.insert(id.to_string(), workflow.clone());
            Ok(())
        })
        .await?;
        Ok(workflow)
    }

    async fn request_research(&self, id: &str, reason: String) -> Result<OrchestratorWorkflow> {
        let manager = self.workflow_manager();
        let mut workflow = manager.load(id)?;
        let state_machines = load_compiled_state_machines(self.project_root.as_path())?;
        WorkflowLifecycleExecutor::with_state_machines(
            crate::resolve_phase_plan_for_pipeline(
                Some(self.project_root.as_path()),
                workflow.pipeline_id.as_deref(),
            )?,
            state_machines,
        )
        .request_research_phase(&mut workflow, reason);
        manager.save(&workflow)?;
        let workflow = manager.save_checkpoint(&workflow, CheckpointReason::Recovery)?;

        self.mutate_persistent_state(|state| {
            state.workflows.insert(id.to_string(), workflow.clone());
            Ok(())
        })
        .await?;
        Ok(workflow)
    }

    async fn complete_current_phase(&self, id: &str) -> Result<OrchestratorWorkflow> {
        let manager = self.workflow_manager();
        let mut workflow = manager.load(id)?;
        let state_machines = load_compiled_state_machines(self.project_root.as_path())?;
        WorkflowLifecycleExecutor::with_state_machines(
            crate::resolve_phase_plan_for_pipeline(
                Some(self.project_root.as_path()),
                workflow.pipeline_id.as_deref(),
            )?,
            state_machines,
        )
        .mark_current_phase_success(&mut workflow);
        manager.save(&workflow)?;
        let workflow = manager.save_checkpoint(&workflow, CheckpointReason::StatusChange)?;

        self.mutate_persistent_state(|state| {
            state.workflows.insert(id.to_string(), workflow.clone());
            Ok(())
        })
        .await?;
        Ok(workflow)
    }

    async fn fail_current_phase(&self, id: &str, error: String) -> Result<OrchestratorWorkflow> {
        let manager = self.workflow_manager();
        let mut workflow = manager.load(id)?;
        let state_machines = load_compiled_state_machines(self.project_root.as_path())?;
        WorkflowLifecycleExecutor::with_state_machines(
            crate::resolve_phase_plan_for_pipeline(
                Some(self.project_root.as_path()),
                workflow.pipeline_id.as_deref(),
            )?,
            state_machines,
        )
        .mark_current_phase_failed(&mut workflow, error);
        manager.save(&workflow)?;
        let workflow = manager.save_checkpoint(&workflow, CheckpointReason::StatusChange)?;

        self.mutate_persistent_state(|state| {
            state.workflows.insert(id.to_string(), workflow.clone());
            Ok(())
        })
        .await?;
        Ok(workflow)
    }

    async fn mark_merge_conflict(&self, id: &str, error: String) -> Result<OrchestratorWorkflow> {
        let manager = self.workflow_manager();
        let mut workflow = manager.load(id)?;
        let state_machines = load_compiled_state_machines(self.project_root.as_path())?;
        WorkflowLifecycleExecutor::with_state_machines(
            crate::resolve_phase_plan_for_pipeline(
                Some(self.project_root.as_path()),
                workflow.pipeline_id.as_deref(),
            )?,
            state_machines,
        )
        .mark_merge_conflict(&mut workflow, error);
        manager.save(&workflow)?;
        let workflow = manager.save_checkpoint(&workflow, CheckpointReason::StatusChange)?;

        let snapshot = {
            let mut lock = self.state.write().await;
            lock.workflows.insert(id.to_string(), workflow.clone());
            lock.clone()
        };

        Self::persist_snapshot(&self.state_file, &snapshot)?;
        Ok(workflow)
    }

    async fn resolve_merge_conflict(&self, id: &str) -> Result<OrchestratorWorkflow> {
        let manager = self.workflow_manager();
        let mut workflow = manager.load(id)?;
        let state_machines = load_compiled_state_machines(self.project_root.as_path())?;
        WorkflowLifecycleExecutor::with_state_machines(
            crate::resolve_phase_plan_for_pipeline(
                Some(self.project_root.as_path()),
                workflow.pipeline_id.as_deref(),
            )?,
            state_machines,
        )
        .resolve_merge_conflict(&mut workflow);
        manager.save(&workflow)?;
        let workflow = manager.save_checkpoint(&workflow, CheckpointReason::StatusChange)?;

        let snapshot = {
            let mut lock = self.state.write().await;
            lock.workflows.insert(id.to_string(), workflow.clone());
            lock.clone()
        };

        Self::persist_snapshot(&self.state_file, &snapshot)?;
        Ok(workflow)
    }
}
