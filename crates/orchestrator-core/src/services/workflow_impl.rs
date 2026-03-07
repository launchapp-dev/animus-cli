use super::*;
use crate::types::PhaseDecision;

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

fn load_phase_retry_configs(
    project_root: &std::path::Path,
) -> std::collections::HashMap<String, crate::agent_runtime_config::PhaseRetryConfig> {
    let config = crate::agent_runtime_config::load_agent_runtime_config_or_default(project_root);
    config
        .phases
        .iter()
        .filter_map(|(phase_id, def)| {
            def.retry
                .as_ref()
                .map(|retry| (phase_id.clone(), retry.clone()))
        })
        .collect()
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
            let task = if let WorkflowSubject::Task { ref id } = input.subject {
                lock.tasks.get(id).cloned()
            } else {
                None
            };
            let pipeline_id = effective_pipeline_id(input.pipeline_id.as_deref(), task.as_ref());
            let executor = WorkflowLifecycleExecutor::new(crate::resolve_phase_plan_for_pipeline(
                None,
                Some(pipeline_id.as_str()),
            )?);
            let workflow = executor.bootstrap(
                id.clone(),
                input.with_pipeline(pipeline_id),
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
        self.complete_current_phase_with_decision(id, None).await
    }

    async fn complete_current_phase_with_decision(
        &self,
        id: &str,
        decision: Option<PhaseDecision>,
    ) -> Result<OrchestratorWorkflow> {
        let mut lock = self.state.write().await;
        let workflow = lock
            .workflows
            .get_mut(id)
            .ok_or_else(|| anyhow!("workflow not found: {id}"))?;
        WorkflowLifecycleExecutor::default()
            .mark_current_phase_success_with_decision(workflow, decision);
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
        let retry_configs = load_phase_retry_configs(self.project_root.as_path());
        let task = if let WorkflowSubject::Task { ref id } = input.subject {
            self.state.read().await.tasks.get(id).cloned()
        } else {
            None
        };
        let pipeline_id = effective_pipeline_id(input.pipeline_id.as_deref(), task.as_ref());
        let workflow_config =
            crate::load_workflow_config_or_default(self.project_root.as_path());
        let skip_guards = crate::resolve_pipeline_skip_guards(
            &workflow_config.config,
            Some(pipeline_id.as_str()),
        );
        let executor = WorkflowLifecycleExecutor::with_state_machines(
            crate::resolve_phase_plan_for_pipeline(
                Some(self.project_root.as_path()),
                Some(pipeline_id.as_str()),
            )?,
            state_machines,
        )
        .with_retry_configs(retry_configs)
        .with_skip_guards(skip_guards);
        let mut workflow = executor.bootstrap(
            id.clone(),
            input.with_pipeline(pipeline_id),
        );
        if let Some(ref task) = task {
            executor.skip_guarded_phases(&mut workflow, task);
        }

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
        self.complete_current_phase_with_decision(id, None).await
    }

    async fn complete_current_phase_with_decision(
        &self,
        id: &str,
        decision: Option<PhaseDecision>,
    ) -> Result<OrchestratorWorkflow> {
        let manager = self.workflow_manager();
        let mut workflow = manager.load(id)?;
        let state_machines = load_compiled_state_machines(self.project_root.as_path())?;
        let retry_configs = load_phase_retry_configs(self.project_root.as_path());
        let workflow_config =
            crate::load_workflow_config_or_default(self.project_root.as_path());
        let skip_guards = crate::resolve_pipeline_skip_guards(
            &workflow_config.config,
            workflow.pipeline_id.as_deref(),
        );
        let executor = WorkflowLifecycleExecutor::with_state_machines(
            crate::resolve_phase_plan_for_pipeline(
                Some(self.project_root.as_path()),
                workflow.pipeline_id.as_deref(),
            )?,
            state_machines,
        )
        .with_retry_configs(retry_configs)
        .with_skip_guards(skip_guards);
        executor.mark_current_phase_success_with_decision(&mut workflow, decision);
        let task_id = if let WorkflowSubject::Task { ref id } = workflow.subject {
            id.clone()
        } else {
            workflow.task_id.clone()
        };
        if let Some(task) = self.state.read().await.tasks.get(&task_id).cloned() {
            executor.skip_guarded_phases(&mut workflow, &task);
        }
        manager.save(&workflow)?;
        let mut workflow = manager.save_checkpoint(&workflow, CheckpointReason::StatusChange)?;
        if workflow.status == crate::types::WorkflowStatus::Completed {
            let retention = crate::load_workflow_config_or_default(self.project_root.as_path())
                .config
                .checkpoint_retention;
            if retention.auto_prune_on_completion {
                match manager.prune_checkpoints(
                    &workflow.id,
                    retention.keep_last_per_phase,
                    retention.max_age_hours,
                    false,
                ) {
                    Ok(_) => match manager.load(id) {
                        Ok(reloaded) => {
                            workflow = reloaded;
                        }
                        Err(err) => {
                            tracing::warn!(
                                target: "orchestrator_core::workflow",
                                workflow_id = %workflow.id,
                                error = %err,
                                "workflow completion succeeded but failed to reload pruned workflow state"
                            );
                        }
                    },
                    Err(err) => {
                        tracing::warn!(
                            target: "orchestrator_core::workflow",
                            workflow_id = %workflow.id,
                            error = %err,
                            "workflow completion succeeded but checkpoint auto-prune failed"
                        );
                    }
                }
            }
        }

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
