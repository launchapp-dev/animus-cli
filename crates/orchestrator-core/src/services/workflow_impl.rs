use super::*;
use crate::types::PhaseDecision;
use animus_actor::Actor;

use super::query_support::paginate_items;

fn effective_workflow_ref(
    requested: Option<&str>,
    project_default_workflow_ref: &str,
    task: Option<&crate::types::OrchestratorTask>,
) -> String {
    if let Some(workflow_ref) = requested.map(str::trim).filter(|value| !value.is_empty()).map(ToOwned::to_owned) {
        return workflow_ref;
    }

    if task.map(|task| task.is_frontend_related()).unwrap_or(false) {
        return crate::workflow::UI_UX_WORKFLOW_REF.to_string();
    }

    project_default_workflow_ref.to_string()
}

fn load_phase_retry_configs(
    project_root: &std::path::Path,
    actor: Option<&Actor>,
) -> std::collections::HashMap<String, crate::agent_runtime_config::PhaseRetryConfig> {
    // Scope retry/backoff policy to the actor partition so a per-user config
    // source's `max_attempts` / backoff overrides apply (matches the workflow
    // config + phase-plan resolution). `actor = None` = global.
    let config = crate::agent_runtime_config::load_agent_runtime_config_or_default_for_actor(project_root, actor);
    config
        .phases
        .iter()
        .filter_map(|(phase_id, def)| def.retry.as_ref().map(|retry| (phase_id.clone(), retry.clone())))
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

fn workflow_matches_filter(workflow: &OrchestratorWorkflow, filter: &WorkflowFilter) -> bool {
    if let Some(status) = filter.status {
        if workflow.status != status {
            return false;
        }
    }

    if let Some(ref workflow_ref) = filter.workflow_ref {
        if workflow.workflow_ref.as_deref() != Some(workflow_ref.as_str()) {
            return false;
        }
    }

    if let Some(ref task_id) = filter.task_id {
        if workflow.task_id != *task_id {
            return false;
        }
    }

    if let Some(ref phase_id) = filter.phase_id {
        let matches_current = workflow.current_phase.as_deref() == Some(phase_id.as_str());
        let matches_history = workflow.phases.iter().any(|phase| phase.phase_id == *phase_id);
        if !matches_current && !matches_history {
            return false;
        }
    }

    if let Some(ref search_text) = filter.search_text {
        let needle = search_text.trim().to_ascii_lowercase();
        if !needle.is_empty() {
            let haystack = format!(
                "{} {} {} {} {}",
                workflow.id,
                workflow.task_id,
                workflow.workflow_ref.as_deref().unwrap_or_default(),
                workflow.current_phase.as_deref().unwrap_or_default(),
                workflow.failure_reason.as_deref().unwrap_or_default()
            )
            .to_ascii_lowercase();
            if !haystack.contains(&needle) {
                return false;
            }
        }
    }

    true
}

fn workflow_status_key(status: WorkflowStatus) -> &'static str {
    match status {
        WorkflowStatus::Pending => "pending",
        WorkflowStatus::Running => "running",
        WorkflowStatus::Paused => "paused",
        WorkflowStatus::Completed => "completed",
        WorkflowStatus::Failed => "failed",
        WorkflowStatus::Escalated => "escalated",
        WorkflowStatus::Cancelled => "cancelled",
    }
}

fn sort_workflows(workflows: &mut [OrchestratorWorkflow], sort: WorkflowQuerySort) {
    match sort {
        WorkflowQuerySort::StartedAt => {
            workflows.sort_by(|a, b| b.started_at.cmp(&a.started_at).then_with(|| a.id.cmp(&b.id)));
        }
        WorkflowQuerySort::Status => {
            workflows.sort_by(|a, b| {
                workflow_status_key(a.status)
                    .cmp(workflow_status_key(b.status))
                    .then_with(|| b.started_at.cmp(&a.started_at))
                    .then_with(|| a.id.cmp(&b.id))
            });
        }
        WorkflowQuerySort::WorkflowRef => {
            workflows.sort_by(|a, b| {
                a.workflow_ref
                    .cmp(&b.workflow_ref)
                    .then_with(|| b.started_at.cmp(&a.started_at))
                    .then_with(|| a.id.cmp(&b.id))
            });
        }
        WorkflowQuerySort::Id => {
            workflows.sort_by(|a, b| a.id.cmp(&b.id));
        }
    }
}

fn query_workflows(workflows: Vec<OrchestratorWorkflow>, query: &WorkflowQuery) -> ListPage<OrchestratorWorkflow> {
    let mut filtered: Vec<_> =
        workflows.into_iter().filter(|workflow| workflow_matches_filter(workflow, &query.filter)).collect();
    sort_workflows(&mut filtered, query.sort);
    paginate_items(filtered, query.page)
}

fn workflow_query_can_use_db_page(query: &WorkflowQuery) -> bool {
    // `workflow_ref` (the type filter) is pushed down to the journal backend's
    // bounded list/query_ids, so it stays on the fast DB-page path; task_id /
    // phase_id / search still fall back to the in-memory scan.
    query.page.limit.is_some()
        && matches!(query.sort, WorkflowQuerySort::StartedAt)
        && query.filter.task_id.is_none()
        && query.filter.phase_id.is_none()
        && query.filter.search_text.as_deref().is_none_or(|value| value.trim().is_empty())
}

#[async_trait]
impl WorkflowServiceApi for InMemoryServiceHub {
    async fn list(&self) -> Result<Vec<OrchestratorWorkflow>> {
        Ok(self.state.read().await.workflows.values().cloned().collect())
    }

    async fn list_summaries(&self) -> Result<Vec<crate::workflow::WorkflowRunSummary>> {
        Ok(self.state.read().await.workflows.values().map(crate::workflow::WorkflowRunSummary::from_workflow).collect())
    }

    async fn list_active(&self) -> Result<Vec<OrchestratorWorkflow>> {
        Ok(self
            .state
            .read()
            .await
            .workflows
            .values()
            .filter(|w| matches!(w.status, WorkflowStatus::Pending | WorkflowStatus::Running | WorkflowStatus::Paused))
            .cloned()
            .collect())
    }

    async fn query(&self, query: WorkflowQuery) -> Result<ListPage<OrchestratorWorkflow>> {
        let workflows = WorkflowServiceApi::list(self).await?;
        Ok(query_workflows(workflows, &query))
    }

    async fn get(&self, id: &str) -> Result<OrchestratorWorkflow> {
        self.state.read().await.workflows.get(id).cloned().ok_or_else(|| not_found(format!("workflow not found: {id}")))
    }

    async fn decisions(&self, id: &str) -> Result<Vec<crate::types::WorkflowDecisionRecord>> {
        Ok(WorkflowServiceApi::get(self, id).await?.decision_history)
    }

    async fn list_checkpoints(&self, id: &str) -> Result<Vec<usize>> {
        let workflow = WorkflowServiceApi::get(self, id).await?;
        Ok(workflow.checkpoint_metadata.checkpoints.iter().map(|checkpoint| checkpoint.number).collect())
    }

    async fn get_checkpoint(&self, id: &str, checkpoint_number: usize) -> Result<OrchestratorWorkflow> {
        let workflow = WorkflowServiceApi::get(self, id).await?;
        if workflow.checkpoint_metadata.checkpoints.iter().any(|checkpoint| checkpoint.number == checkpoint_number) {
            Ok(workflow)
        } else {
            Err(not_found(format!("checkpoint not found: {id} #{checkpoint_number}")))
        }
    }

    async fn run(&self, input: WorkflowRunInput, actor: Option<&Actor>) -> Result<OrchestratorWorkflow> {
        let id = Uuid::new_v4().to_string();
        let workflow = {
            let mut lock = self.state.write().await;
            let task = input.subject.as_ref().and_then(|s| s.task_id()).and_then(|id| lock.tasks.get(id).cloned());
            let workflow_ref =
                effective_workflow_ref(input.workflow_ref(), crate::workflow::STANDARD_WORKFLOW_REF, task.as_ref());
            let executor = WorkflowLifecycleExecutor::new(crate::resolve_phase_plan_for_workflow_ref_for_actor(
                None,
                Some(workflow_ref.as_str()),
                actor,
            )?);
            let workflow = executor.bootstrap(id.clone(), input.with_workflow_ref(workflow_ref));
            lock.workflows.insert(id.clone(), workflow.clone());
            workflow
        };
        Ok(workflow)
    }

    async fn resume(&self, id: &str) -> Result<OrchestratorWorkflow> {
        let mut lock = self.state.write().await;
        let workflow = lock.workflows.get_mut(id).ok_or_else(|| not_found(format!("workflow not found: {id}")))?;
        let executor = WorkflowLifecycleExecutor::default();
        executor.resume(workflow);
        Ok(workflow.clone())
    }

    async fn pause(&self, id: &str) -> Result<OrchestratorWorkflow> {
        let mut lock = self.state.write().await;
        let workflow = lock.workflows.get_mut(id).ok_or_else(|| not_found(format!("workflow not found: {id}")))?;
        WorkflowLifecycleExecutor::default().pause(workflow);
        Ok(workflow.clone())
    }

    async fn cancel(&self, id: &str) -> Result<OrchestratorWorkflow> {
        let mut lock = self.state.write().await;
        let workflow = lock.workflows.get_mut(id).ok_or_else(|| not_found(format!("workflow not found: {id}")))?;
        WorkflowLifecycleExecutor::default().cancel(workflow);
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
        let workflow = lock.workflows.get_mut(id).ok_or_else(|| not_found(format!("workflow not found: {id}")))?;
        let mut updated = workflow.clone();
        WorkflowLifecycleExecutor::default().mark_current_phase_success_with_decision(&mut updated, decision)?;
        *workflow = updated;
        Ok(workflow.clone())
    }

    async fn fail_current_phase(&self, id: &str, error: String) -> Result<OrchestratorWorkflow> {
        let mut lock = self.state.write().await;
        let workflow = lock.workflows.get_mut(id).ok_or_else(|| not_found(format!("workflow not found: {id}")))?;
        let mut updated = workflow.clone();
        WorkflowLifecycleExecutor::default().mark_current_phase_failed(&mut updated, error)?;
        *workflow = updated;
        Ok(workflow.clone())
    }

    async fn mark_completed_failed(&self, id: &str, error: String) -> Result<OrchestratorWorkflow> {
        let mut lock = self.state.write().await;
        let workflow = lock.workflows.get_mut(id).ok_or_else(|| not_found(format!("workflow not found: {id}")))?;
        WorkflowLifecycleExecutor::default().mark_completed_failed(workflow, error);
        Ok(workflow.clone())
    }

    async fn mark_merge_conflict(&self, id: &str, error: String) -> Result<OrchestratorWorkflow> {
        let mut lock = self.state.write().await;
        let workflow = lock.workflows.get_mut(id).ok_or_else(|| not_found(format!("workflow not found: {id}")))?;
        WorkflowLifecycleExecutor::default().mark_merge_conflict(workflow, error);
        Ok(workflow.clone())
    }

    async fn resolve_merge_conflict(&self, id: &str) -> Result<OrchestratorWorkflow> {
        let mut lock = self.state.write().await;
        let workflow = lock.workflows.get_mut(id).ok_or_else(|| not_found(format!("workflow not found: {id}")))?;
        WorkflowLifecycleExecutor::default().resolve_merge_conflict(workflow);
        Ok(workflow.clone())
    }

    async fn record_feedback(&self, id: &str, feedback: String) -> Result<()> {
        let mut lock = self.state.write().await;
        let workflow = lock.workflows.get_mut(id).ok_or_else(|| not_found(format!("workflow not found: {id}")))?;
        let phase_id = workflow.current_phase.clone().unwrap_or_else(|| "unknown".to_string());
        workflow.decision_history.push(crate::types::WorkflowDecisionRecord {
            timestamp: chrono::Utc::now(),
            phase_id,
            source: crate::types::WorkflowDecisionSource::Fallback,
            decision: crate::types::WorkflowDecisionAction::Advance,
            target_phase: None,
            reason: feedback,
            confidence: 1.0,
            risk: crate::types::WorkflowDecisionRisk::Low,
            guardrail_violations: Vec::new(),
            machine_version: None,
            machine_hash: None,
            machine_source: Some("human-feedback".to_string()),
        });
        Ok(())
    }

    async fn prune_runs(&self, filter: WorkflowRunPruneFilter, dry_run: bool) -> Result<WorkflowRunPruneReport> {
        use crate::workflow::{
            is_terminal_workflow_run_status, select_workflow_prune_candidates, WorkflowRunDeletion,
            WorkflowRunPruneCandidate,
        };
        if let Some(status) = filter.status {
            if !is_terminal_workflow_run_status(status) {
                return Err(crate::types::invalid_input(format!(
                    "cannot prune runs with non-terminal status '{}'; allowed: completed, failed, escalated, cancelled",
                    workflow_status_key(status)
                )));
            }
        }
        let mut lock = self.state.write().await;
        let candidates: Vec<WorkflowRunPruneCandidate> = lock
            .workflows
            .values()
            .map(|workflow| WorkflowRunPruneCandidate {
                workflow_id: workflow.id.clone(),
                status: workflow.status,
                effective_at: workflow.completed_at.unwrap_or(workflow.started_at),
            })
            .collect();
        let candidates = select_workflow_prune_candidates(candidates, &filter, chrono::Utc::now());
        let mut report = WorkflowRunPruneReport { dry_run, ..Default::default() };
        for candidate in candidates {
            if !dry_run {
                lock.workflows.remove(&candidate.workflow_id);
            }
            report.deleted.push(WorkflowRunDeletion {
                workflow_id: candidate.workflow_id,
                status: workflow_status_key(candidate.status).to_string(),
                bytes_reclaimed: 0,
            });
        }
        Ok(report)
    }

    async fn delete_run(&self, id: &str, dry_run: bool) -> Result<WorkflowRunPruneReport> {
        use crate::workflow::{is_terminal_workflow_run_status, WorkflowRunDeletion};
        let mut lock = self.state.write().await;
        let workflow = lock.workflows.get(id).ok_or_else(|| not_found(format!("workflow not found: {id}")))?;
        if !is_terminal_workflow_run_status(workflow.status) {
            return Err(crate::types::invalid_input(format!(
                "workflow run {} has status '{}'; only terminal runs (completed, failed, escalated, cancelled) can be deleted",
                id,
                workflow_status_key(workflow.status)
            )));
        }
        let status = workflow_status_key(workflow.status).to_string();
        if !dry_run {
            lock.workflows.remove(id);
        }
        Ok(WorkflowRunPruneReport {
            dry_run,
            deleted: vec![WorkflowRunDeletion { workflow_id: id.to_string(), status, bytes_reclaimed: 0 }],
            total_bytes_reclaimed: 0,
        })
    }
}

#[async_trait]
impl WorkflowServiceApi for FileServiceHub {
    async fn list(&self) -> Result<Vec<OrchestratorWorkflow>> {
        self.workflow_manager().list_all()
    }

    async fn list_summaries(&self) -> Result<Vec<crate::workflow::WorkflowRunSummary>> {
        self.workflow_manager().list_all_summaries()
    }

    async fn list_active(&self) -> Result<Vec<OrchestratorWorkflow>> {
        self.workflow_manager().list_active()
    }

    async fn query(&self, query: WorkflowQuery) -> Result<ListPage<OrchestratorWorkflow>> {
        if workflow_query_can_use_db_page(&query) {
            let manager = self.workflow_manager();
            // Fast path for the first page (the workflow-list UI + graphql poll):
            // fetch the bounded runs in ONE journal RPC, plus a cheap ids-only
            // query for `total`, instead of query_ids + a per-id load loop (N+1 —
            // one RPC per run, which serialized behind other journal traffic and
            // made the list intermittently multi-second).
            let workflow_ref = query.filter.workflow_ref.as_deref();
            if query.page.offset == 0 {
                if let Some(limit) = query.page.limit {
                    let (_ids, total) = manager.query_ids(query.page, query.filter.status, workflow_ref)?;
                    let items = manager.list_page(query.filter.status, workflow_ref, limit)?;
                    return Ok(ListPage::new(items, total, query.page));
                }
            }
            // Offset > 0: the journal list RPC has no offset, so keep the id-walk.
            let (ids, total) = manager.query_ids(query.page, query.filter.status, workflow_ref)?;
            let mut items = Vec::with_capacity(ids.len());
            for id in ids {
                if let Ok(workflow) = manager.load(&id) {
                    items.push(workflow);
                }
            }
            return Ok(ListPage::new(items, total, query.page));
        }

        let workflows = WorkflowServiceApi::list(self).await?;
        Ok(query_workflows(workflows, &query))
    }

    async fn get(&self, id: &str) -> Result<OrchestratorWorkflow> {
        self.workflow_manager().load(id)
    }

    async fn decisions(&self, id: &str) -> Result<Vec<crate::types::WorkflowDecisionRecord>> {
        Ok(WorkflowServiceApi::get(self, id).await?.decision_history)
    }

    async fn list_checkpoints(&self, id: &str) -> Result<Vec<usize>> {
        self.workflow_manager().list_checkpoints(id)
    }

    async fn get_checkpoint(&self, id: &str, checkpoint_number: usize) -> Result<OrchestratorWorkflow> {
        self.workflow_manager().load_checkpoint(id, checkpoint_number)
    }

    async fn run(&self, input: WorkflowRunInput, actor: Option<&Actor>) -> Result<OrchestratorWorkflow> {
        let id = Uuid::new_v4().to_string();
        let state_machines = load_compiled_state_machines(self.project_root.as_path())?;
        let retry_configs = load_phase_retry_configs(self.project_root.as_path(), actor);
        // Resolve the bootstrap config from the actor's partition so a per-user
        // `config_source` (global∪private∪shared) contributes the default
        // workflow ref, skip guards, and phase plan. `actor = None` = global.
        let workflow_config = crate::load_workflow_config_or_default_for_actor(self.project_root.as_path(), actor);
        let task = if let Some(task_id) = input.subject.as_ref().and_then(|s| s.task_id()) {
            crate::workflow::load_task(&self.project_root, task_id).ok()
        } else {
            None
        };
        let workflow_ref =
            effective_workflow_ref(input.workflow_ref(), &workflow_config.config.default_workflow_ref, task.as_ref());
        let skip_guards = crate::resolve_workflow_skip_guards(&workflow_config.config, Some(workflow_ref.as_str()));
        let executor = WorkflowLifecycleExecutor::with_state_machines(
            crate::resolve_phase_plan_for_workflow_ref_for_actor(
                Some(self.project_root.as_path()),
                Some(workflow_ref.as_str()),
                actor,
            )?,
            state_machines,
        )
        .with_retry_configs(retry_configs)
        .with_skip_guards(skip_guards);
        let mut workflow = executor.bootstrap(id.clone(), input.with_workflow_ref(workflow_ref));
        // A subjectless run binds a None subject context: no subject to resolve,
        // so subject template vars are simply absent and no guard phases are
        // skipped on subject identity here.
        if let Some(subject) = workflow.subject.clone() {
            if let Ok(subject_context) = self.subject_resolver().resolve_subject_context(&subject, None, None).await {
                executor.skip_guarded_phases(&mut workflow, &subject_context);
            }
        }

        let manager = self.workflow_manager();
        manager.save(&workflow)?;
        let workflow = manager.save_checkpoint(&workflow, CheckpointReason::Start)?;
        // Persist the actor partition so EVERY later lifecycle transition
        // (resume/complete/cancel/...) re-resolves config from the same partition
        // this run bootstrapped with — otherwise an actor-private workflow would
        // start, then fail or mis-route at the next transition (global lookup).
        manager.set_workflow_actor(&workflow.id, actor)?;

        self.state.write().await.workflows.insert(id, workflow.clone());
        Ok(workflow)
    }

    async fn resume(&self, id: &str) -> Result<OrchestratorWorkflow> {
        let manager = self.workflow_manager();
        let mut workflow = manager.load(id)?;
        let state_machines = load_compiled_state_machines(self.project_root.as_path())?;
        let executor = WorkflowLifecycleExecutor::with_state_machines(
            crate::resolve_phase_plan_for_workflow_ref_for_actor(
                Some(self.project_root.as_path()),
                workflow.workflow_ref.as_deref(),
                manager.load_workflow_actor(id).as_ref(),
            )?,
            state_machines,
        );
        executor.resume(&mut workflow);
        manager.save(&workflow)?;
        let workflow = manager.save_checkpoint(&workflow, CheckpointReason::Resume)?;

        self.state.write().await.workflows.insert(id.to_string(), workflow.clone());
        Ok(workflow)
    }

    async fn pause(&self, id: &str) -> Result<OrchestratorWorkflow> {
        let manager = self.workflow_manager();
        let mut workflow = manager.load(id)?;
        let state_machines = load_compiled_state_machines(self.project_root.as_path())?;
        WorkflowLifecycleExecutor::with_state_machines(
            crate::resolve_phase_plan_for_workflow_ref_for_actor(
                Some(self.project_root.as_path()),
                workflow.workflow_ref.as_deref(),
                manager.load_workflow_actor(id).as_ref(),
            )?,
            state_machines,
        )
        .pause(&mut workflow);
        manager.save(&workflow)?;
        let workflow = manager.save_checkpoint(&workflow, CheckpointReason::Pause)?;

        self.state.write().await.workflows.insert(id.to_string(), workflow.clone());
        Ok(workflow)
    }

    async fn cancel(&self, id: &str) -> Result<OrchestratorWorkflow> {
        let manager = self.workflow_manager();
        let mut workflow = manager.load(id)?;
        let state_machines = load_compiled_state_machines(self.project_root.as_path())?;
        let phase_plan = crate::resolve_phase_plan_for_workflow_ref_for_actor(
            Some(self.project_root.as_path()),
            workflow.workflow_ref.as_deref(),
            manager.load_workflow_actor(id).as_ref(),
        )
        .unwrap_or_default();
        WorkflowLifecycleExecutor::with_state_machines(phase_plan, state_machines).cancel(&mut workflow);
        manager.save(&workflow)?;
        let workflow = manager.save_checkpoint(&workflow, CheckpointReason::Cancel)?;

        self.state.write().await.workflows.insert(id.to_string(), workflow.clone());
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
        // The run's persisted actor partition: re-resolve config / phase plan /
        // retry policy from the SAME partition the run bootstrapped with.
        let actor = manager.load_workflow_actor(id);
        let state_machines = load_compiled_state_machines(self.project_root.as_path())?;
        let retry_configs = load_phase_retry_configs(self.project_root.as_path(), actor.as_ref());
        let workflow_config =
            crate::load_workflow_config_or_default_for_actor(self.project_root.as_path(), actor.as_ref());
        let verdict_routing =
            crate::resolve_workflow_verdict_routing(&workflow_config.config, workflow.workflow_ref.as_deref());
        let skip_guards =
            crate::resolve_workflow_skip_guards(&workflow_config.config, workflow.workflow_ref.as_deref());
        let executor = WorkflowLifecycleExecutor::with_state_machines(
            crate::resolve_phase_plan_for_workflow_ref_for_actor(
                Some(self.project_root.as_path()),
                workflow.workflow_ref.as_deref(),
                actor.as_ref(),
            )?,
            state_machines,
        )
        .with_verdict_routing_config(verdict_routing)
        .with_retry_configs(retry_configs)
        .with_skip_guards(skip_guards);
        executor.mark_current_phase_success_with_decision(&mut workflow, decision)?;
        // Subjectless runs bind a None subject context (nothing to resolve).
        if let Some(subject) = workflow.subject.clone() {
            if let Ok(subject_context) = self.subject_resolver().resolve_subject_context(&subject, None, None).await {
                executor.skip_guarded_phases(&mut workflow, &subject_context);
            }
        }
        manager.save(&workflow)?;
        let mut workflow = manager.save_checkpoint(&workflow, CheckpointReason::StatusChange)?;
        if workflow.status == crate::types::WorkflowStatus::Completed {
            let retention =
                crate::load_workflow_config_or_default(self.project_root.as_path()).config.checkpoint_retention;
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

        self.state.write().await.workflows.insert(id.to_string(), workflow.clone());
        Ok(workflow)
    }

    async fn fail_current_phase(&self, id: &str, error: String) -> Result<OrchestratorWorkflow> {
        let manager = self.workflow_manager();
        let mut workflow = manager.load(id)?;
        let state_machines = load_compiled_state_machines(self.project_root.as_path())?;
        WorkflowLifecycleExecutor::with_state_machines(
            crate::resolve_phase_plan_for_workflow_ref_for_actor(
                Some(self.project_root.as_path()),
                workflow.workflow_ref.as_deref(),
                manager.load_workflow_actor(id).as_ref(),
            )?,
            state_machines,
        )
        .mark_current_phase_failed(&mut workflow, error)?;
        manager.save(&workflow)?;
        let workflow = manager.save_checkpoint(&workflow, CheckpointReason::StatusChange)?;

        self.state.write().await.workflows.insert(id.to_string(), workflow.clone());
        Ok(workflow)
    }

    async fn mark_completed_failed(&self, id: &str, error: String) -> Result<OrchestratorWorkflow> {
        let manager = self.workflow_manager();
        let mut workflow = manager.load(id)?;
        let state_machines = load_compiled_state_machines(self.project_root.as_path())?;
        WorkflowLifecycleExecutor::with_state_machines(
            crate::resolve_phase_plan_for_workflow_ref_for_actor(
                Some(self.project_root.as_path()),
                workflow.workflow_ref.as_deref(),
                manager.load_workflow_actor(id).as_ref(),
            )?,
            state_machines,
        )
        .mark_completed_failed(&mut workflow, error);
        manager.save(&workflow)?;
        let workflow = manager.save_checkpoint(&workflow, CheckpointReason::StatusChange)?;

        self.state.write().await.workflows.insert(id.to_string(), workflow.clone());
        Ok(workflow)
    }

    async fn mark_merge_conflict(&self, id: &str, error: String) -> Result<OrchestratorWorkflow> {
        let manager = self.workflow_manager();
        let mut workflow = manager.load(id)?;
        let state_machines = load_compiled_state_machines(self.project_root.as_path())?;
        WorkflowLifecycleExecutor::with_state_machines(
            crate::resolve_phase_plan_for_workflow_ref_for_actor(
                Some(self.project_root.as_path()),
                workflow.workflow_ref.as_deref(),
                manager.load_workflow_actor(id).as_ref(),
            )?,
            state_machines,
        )
        .mark_merge_conflict(&mut workflow, error);
        manager.save(&workflow)?;
        let workflow = manager.save_checkpoint(&workflow, CheckpointReason::StatusChange)?;

        self.state.write().await.workflows.insert(id.to_string(), workflow.clone());
        Ok(workflow)
    }

    async fn resolve_merge_conflict(&self, id: &str) -> Result<OrchestratorWorkflow> {
        let manager = self.workflow_manager();
        let mut workflow = manager.load(id)?;
        let state_machines = load_compiled_state_machines(self.project_root.as_path())?;
        WorkflowLifecycleExecutor::with_state_machines(
            crate::resolve_phase_plan_for_workflow_ref_for_actor(
                Some(self.project_root.as_path()),
                workflow.workflow_ref.as_deref(),
                manager.load_workflow_actor(id).as_ref(),
            )?,
            state_machines,
        )
        .resolve_merge_conflict(&mut workflow);
        manager.save(&workflow)?;
        let workflow = manager.save_checkpoint(&workflow, CheckpointReason::StatusChange)?;

        self.state.write().await.workflows.insert(id.to_string(), workflow.clone());
        Ok(workflow)
    }

    async fn record_feedback(&self, id: &str, feedback: String) -> Result<()> {
        let manager = self.workflow_manager();
        let mut workflow = manager.load(id)?;
        let phase_id = workflow.current_phase.clone().unwrap_or_else(|| "unknown".to_string());
        workflow.decision_history.push(crate::types::WorkflowDecisionRecord {
            timestamp: chrono::Utc::now(),
            phase_id,
            source: crate::types::WorkflowDecisionSource::Fallback,
            decision: crate::types::WorkflowDecisionAction::Advance,
            target_phase: None,
            reason: feedback,
            confidence: 1.0,
            risk: crate::types::WorkflowDecisionRisk::Low,
            guardrail_violations: Vec::new(),
            machine_version: None,
            machine_hash: None,
            machine_source: Some("human-feedback".to_string()),
        });
        manager.save(&workflow)?;
        self.state.write().await.workflows.insert(id.to_string(), workflow.clone());
        Ok(())
    }

    async fn prune_runs(&self, filter: WorkflowRunPruneFilter, dry_run: bool) -> Result<WorkflowRunPruneReport> {
        let report = self.workflow_manager().prune_runs(&filter, dry_run)?;
        if !dry_run && !report.deleted.is_empty() {
            let mut lock = self.state.write().await;
            for deletion in &report.deleted {
                lock.workflows.remove(&deletion.workflow_id);
            }
        }
        Ok(report)
    }

    async fn delete_run(&self, id: &str, dry_run: bool) -> Result<WorkflowRunPruneReport> {
        let report = self.workflow_manager().delete_run(id, dry_run)?;
        if !dry_run {
            self.state.write().await.workflows.remove(id);
        }
        Ok(report)
    }
}
