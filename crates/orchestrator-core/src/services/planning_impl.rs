use super::*;
use orchestrator_providers::PlanningServiceApi as ProviderPlanningServiceApi;

#[async_trait]
impl PlanningServiceApi for InMemoryServiceHub {
    fn requirements_provider(&self) -> Arc<dyn RequirementsProvider> {
        Arc::new(crate::providers::BuiltinRequirementsProvider::new(
            Arc::new(self.clone()),
        ))
    }

    async fn draft_vision(&self, input: VisionDraftInput) -> Result<VisionDocument> {
        let now = Utc::now();
        let project_name = input
            .project_name
            .clone()
            .unwrap_or_else(|| "Project".to_string());
        let mut lock = self.state.write().await;
        Ok(planning_shared::draft_vision_and_record(
            &mut lock,
            ".".to_string(),
            project_name,
            input,
            now,
        ))
    }

    async fn get_vision(&self) -> Result<Option<VisionDocument>> {
        Ok(self.state.read().await.vision.clone())
    }

    async fn draft_requirements(
        &self,
        input: RequirementsDraftInput,
    ) -> Result<RequirementsDraftResult> {
        let mut lock = self.state.write().await;
        let (appended_ids, appended_count) =
            planning_shared::draft_requirements_and_record(&mut lock, input, None)?;
        let requirements = planning_shared::requirements_by_ids_sorted(&lock, &appended_ids);

        Ok(RequirementsDraftResult {
            requirements,
            appended_count,
            codebase_insight: None,
        })
    }

    async fn list_requirements(&self) -> Result<Vec<RequirementItem>> {
        let lock = self.state.read().await;
        Ok(planning_shared::list_requirements_sorted(&lock))
    }

    async fn list_requirements_filtered(
        &self,
        filter: RequirementFilter,
    ) -> Result<Vec<RequirementItem>> {
        let lock = self.state.read().await;
        Ok(planning_shared::list_requirements_filtered(&lock, &filter))
    }

    async fn get_requirement(&self, id: &str) -> Result<RequirementItem> {
        let lock = self.state.read().await;
        planning_shared::get_requirement(&lock, id)
    }

    async fn refine_requirements(
        &self,
        input: RequirementsRefineInput,
    ) -> Result<Vec<RequirementItem>> {
        let mut lock = self.state.write().await;
        Ok(planning_shared::refine_requirements_and_record(
            &mut lock, input,
        ))
    }

    async fn upsert_requirement(
        &self,
        mut requirement: RequirementItem,
    ) -> Result<RequirementItem> {
        let mut lock = self.state.write().await;
        let now = Utc::now();

        if requirement.id.trim().is_empty() {
            requirement.id = next_requirement_id(&lock.requirements);
        }
        if requirement
            .relative_path
            .as_ref()
            .map(|value| value.trim().is_empty())
            .unwrap_or(true)
        {
            requirement.relative_path = Some(format!("generated/{}.json", requirement.id));
        }
        if requirement.source.trim().is_empty() {
            requirement.source = protocol::ACTOR_CORE.to_string();
        }
        if requirement.created_at.timestamp() == 0 {
            requirement.created_at = now;
        }
        requirement.updated_at = now;

        lock.requirements
            .insert(requirement.id.clone(), requirement.clone());
        lock.logs.push(LogEntry {
            timestamp: now,
            level: LogLevel::Info,
            message: format!("requirement upserted ({})", requirement.id),
        });

        Ok(requirement)
    }

    async fn delete_requirement(&self, id: &str) -> Result<()> {
        let mut lock = self.state.write().await;
        if lock.requirements.remove(id).is_none() {
            return Err(anyhow!("requirement not found: {id}"));
        }
        lock.logs.push(LogEntry {
            timestamp: Utc::now(),
            level: LogLevel::Info,
            message: format!("requirement deleted ({id})"),
        });
        Ok(())
    }

    async fn list_epics(&self) -> Result<Vec<EpicItem>> {
        let lock = self.state.read().await;
        Ok(planning_shared::list_epics_sorted(&lock))
    }

    async fn get_epic(&self, id: &str) -> Result<EpicItem> {
        let lock = self.state.read().await;
        planning_shared::get_epic(&lock, id)
    }

    async fn upsert_epic(&self, mut epic: EpicItem) -> Result<EpicItem> {
        let mut lock = self.state.write().await;
        let now = Utc::now();

        if epic.id.trim().is_empty() {
            epic.id = next_epic_id(&lock.epics);
        }
        if epic.source.trim().is_empty() {
            epic.source = protocol::ACTOR_CORE.to_string();
        }
        if epic.created_at.timestamp() == 0 {
            epic.created_at = now;
        }
        epic.updated_at = now;

        lock.epics.insert(epic.id.clone(), epic.clone());
        Ok(epic)
    }

    async fn delete_epic(&self, id: &str) -> Result<()> {
        let mut lock = self.state.write().await;
        if lock.epics.remove(id).is_none() {
            return Err(anyhow!("epic not found: {id}"));
        }
        Ok(())
    }

    async fn execute_requirements(
        &self,
        input: RequirementsExecutionInput,
    ) -> Result<RequirementsExecutionResult> {
        let mut lock = self.state.write().await;
        planning_shared::execute_requirements_and_record(&mut lock, input, None, None, None)
    }
}

#[async_trait]
impl PlanningServiceApi for FileServiceHub {
    fn requirements_provider(&self) -> Arc<dyn RequirementsProvider> {
        Arc::new(crate::providers::BuiltinRequirementsProvider::new(
            Arc::new(self.clone()),
        ))
    }

    async fn draft_vision(&self, input: VisionDraftInput) -> Result<VisionDocument> {
        let now = Utc::now();
        let requested_project_name = input.project_name.clone();
        let project_root_display = self.project_root.display().to_string();
        let default_project_name = default_vision_project_name(&self.project_root);

        let (vision, snapshot) = self
            .mutate_persistent_state(|state| {
                let active_project_name = state
                    .active_project_id
                    .as_ref()
                    .and_then(|id| state.projects.get(id).map(|project| project.name.clone()));
                let project_name = requested_project_name
                    .or(active_project_name)
                    .unwrap_or_else(|| default_project_name.clone());

                let vision = planning_shared::draft_vision_and_record(
                    state,
                    project_root_display.clone(),
                    project_name,
                    input,
                    now,
                );
                Ok(vision)
            })
            .await?;

        write_planning_artifacts(
            &self.project_root,
            snapshot.vision.as_ref(),
            &snapshot.requirements,
        )?;
        Ok(vision)
    }

    async fn get_vision(&self) -> Result<Option<VisionDocument>> {
        Ok(self.state.read().await.vision.clone())
    }

    async fn draft_requirements(
        &self,
        input: RequirementsDraftInput,
    ) -> Result<RequirementsDraftResult> {
        let codebase_insight = if input.include_codebase_scan {
            Some(collect_codebase_insight(&self.project_root))
        } else {
            None
        };
        let insight_for_drafting = codebase_insight.clone();

        let ((requirements, appended_count), snapshot) = self
            .mutate_persistent_state(|state| {
                let (appended_ids, appended_count) =
                    planning_shared::draft_requirements_and_record(
                        state,
                        input,
                        insight_for_drafting.as_ref(),
                    )?;
                state.all_requirements_dirty = true;
                let requirements =
                    planning_shared::requirements_by_ids_sorted(state, &appended_ids);
                Ok((requirements, appended_count))
            })
            .await?;

        write_planning_artifacts(
            &self.project_root,
            snapshot.vision.as_ref(),
            &snapshot.requirements,
        )?;

        Ok(RequirementsDraftResult {
            requirements,
            appended_count,
            codebase_insight,
        })
    }

    async fn list_requirements(&self) -> Result<Vec<RequirementItem>> {
        let lock = self.state.read().await;
        Ok(planning_shared::list_requirements_sorted(&lock))
    }

    async fn list_requirements_filtered(
        &self,
        filter: RequirementFilter,
    ) -> Result<Vec<RequirementItem>> {
        let lock = self.state.read().await;
        Ok(planning_shared::list_requirements_filtered(&lock, &filter))
    }

    async fn get_requirement(&self, id: &str) -> Result<RequirementItem> {
        let lock = self.state.read().await;
        planning_shared::get_requirement(&lock, id)
    }

    async fn refine_requirements(
        &self,
        input: RequirementsRefineInput,
    ) -> Result<Vec<RequirementItem>> {
        let (refined, snapshot) = self
            .mutate_persistent_state(|state| {
                let refined = planning_shared::refine_requirements_and_record(state, input);
                state.all_requirements_dirty = true;
                Ok(refined)
            })
            .await?;

        write_planning_artifacts(
            &self.project_root,
            snapshot.vision.as_ref(),
            &snapshot.requirements,
        )?;
        Ok(refined)
    }

    async fn upsert_requirement(&self, requirement: RequirementItem) -> Result<RequirementItem> {
        let (requirement, snapshot) = self
            .mutate_persistent_state(|state| {
                let mut requirement = requirement;
                let now = Utc::now();

                if requirement.id.trim().is_empty() {
                    requirement.id = next_requirement_id(&state.requirements);
                }
                if requirement
                    .relative_path
                    .as_ref()
                    .map(|value| value.trim().is_empty())
                    .unwrap_or(true)
                {
                    requirement.relative_path = Some(format!("generated/{}.json", requirement.id));
                }
                if requirement.source.trim().is_empty() {
                    requirement.source = protocol::ACTOR_CORE.to_string();
                }
                if requirement.created_at.timestamp() == 0 {
                    requirement.created_at = now;
                }
                requirement.updated_at = now;

                state
                    .requirements
                    .insert(requirement.id.clone(), requirement.clone());
                state.dirty_requirements.insert(requirement.id.clone());
                state.logs.push(LogEntry {
                    timestamp: now,
                    level: LogLevel::Info,
                    message: format!("requirement upserted ({})", requirement.id),
                });
                Ok(requirement)
            })
            .await?;

        write_planning_artifacts(
            &self.project_root,
            snapshot.vision.as_ref(),
            &snapshot.requirements,
        )?;
        Ok(requirement)
    }

    async fn delete_requirement(&self, id: &str) -> Result<()> {
        let (_, snapshot) = self
            .mutate_persistent_state(|state| {
                if state.requirements.remove(id).is_none() {
                    return Err(anyhow!("requirement not found: {id}"));
                }
                state.all_requirements_dirty = true;
                state.logs.push(LogEntry {
                    timestamp: Utc::now(),
                    level: LogLevel::Info,
                    message: format!("requirement deleted ({id})"),
                });
                Ok(())
            })
            .await?;

        write_planning_artifacts(
            &self.project_root,
            snapshot.vision.as_ref(),
            &snapshot.requirements,
        )?;
        Ok(())
    }

    async fn list_epics(&self) -> Result<Vec<EpicItem>> {
        let lock = self.state.read().await;
        Ok(planning_shared::list_epics_sorted(&lock))
    }

    async fn get_epic(&self, id: &str) -> Result<EpicItem> {
        let lock = self.state.read().await;
        planning_shared::get_epic(&lock, id)
    }

    async fn upsert_epic(&self, epic: EpicItem) -> Result<EpicItem> {
        let (epic, snapshot) = self
            .mutate_persistent_state(|state| {
                let mut epic = epic;
                let now = Utc::now();

                if epic.id.trim().is_empty() {
                    epic.id = next_epic_id(&state.epics);
                }
                if epic.source.trim().is_empty() {
                    epic.source = protocol::ACTOR_CORE.to_string();
                }
                if epic.created_at.timestamp() == 0 {
                    epic.created_at = now;
                }
                epic.updated_at = now;

                state.epics.insert(epic.id.clone(), epic.clone());
                state.dirty_epics.insert(epic.id.clone());
                Ok(epic)
            })
            .await?;

        write_planning_artifacts(
            &self.project_root,
            snapshot.vision.as_ref(),
            &snapshot.requirements,
        )?;
        Ok(epic)
    }

    async fn delete_epic(&self, id: &str) -> Result<()> {
        let (_, snapshot) = self
            .mutate_persistent_state(|state| {
                if state.epics.remove(id).is_none() {
                    return Err(anyhow!("epic not found: {id}"));
                }
                state.all_epics_dirty = true;
                Ok(())
            })
            .await?;

        write_planning_artifacts(
            &self.project_root,
            snapshot.vision.as_ref(),
            &snapshot.requirements,
        )?;
        Ok(())
    }

    async fn execute_requirements(
        &self,
        input: RequirementsExecutionInput,
    ) -> Result<RequirementsExecutionResult> {
        let manager = self.workflow_manager();
        let loaded_state_machines =
            crate::state_machines::load_state_machines_for_project(self.project_root.as_path())?;
        for warning in &loaded_state_machines.warnings {
            tracing::warn!(
                target: "orchestrator_core::state_machines",
                code = %warning.code,
                message = %warning.message,
                source = %loaded_state_machines.compiled.metadata.source.as_str(),
                hash = %loaded_state_machines.compiled.metadata.hash,
                version = loaded_state_machines.compiled.metadata.version,
                path = %loaded_state_machines.path.display(),
                "state machine fallback"
            );
        }

        let (result, snapshot) = self
            .mutate_persistent_state(|state| {
                let result = planning_shared::execute_requirements_and_record(
                    state,
                    input,
                    Some(self.project_root.as_path()),
                    Some(&manager),
                    Some(&loaded_state_machines.compiled),
                )?;
                state.all_requirements_dirty = true;
                state.all_tasks_dirty = true;
                Ok(result)
            })
            .await?;

        write_planning_artifacts(
            &self.project_root,
            snapshot.vision.as_ref(),
            &snapshot.requirements,
        )?;
        Ok(result)
    }
}

#[async_trait]
impl ProviderPlanningServiceApi for InMemoryServiceHub {
    async fn draft_requirements(
        &self,
        input: RequirementsDraftInput,
    ) -> Result<RequirementsDraftResult> {
        PlanningServiceApi::draft_requirements(self, input).await
    }

    async fn list_requirements(&self) -> Result<Vec<RequirementItem>> {
        PlanningServiceApi::list_requirements(self).await
    }

    async fn get_requirement(&self, id: &str) -> Result<RequirementItem> {
        PlanningServiceApi::get_requirement(self, id).await
    }

    async fn refine_requirements(
        &self,
        input: RequirementsRefineInput,
    ) -> Result<Vec<RequirementItem>> {
        PlanningServiceApi::refine_requirements(self, input).await
    }

    async fn upsert_requirement(&self, requirement: RequirementItem) -> Result<RequirementItem> {
        PlanningServiceApi::upsert_requirement(self, requirement).await
    }

    async fn delete_requirement(&self, id: &str) -> Result<()> {
        PlanningServiceApi::delete_requirement(self, id).await
    }

    async fn execute_requirements(
        &self,
        input: RequirementsExecutionInput,
    ) -> Result<RequirementsExecutionResult> {
        PlanningServiceApi::execute_requirements(self, input).await
    }
}

#[async_trait]
impl ProviderPlanningServiceApi for FileServiceHub {
    async fn draft_requirements(
        &self,
        input: RequirementsDraftInput,
    ) -> Result<RequirementsDraftResult> {
        PlanningServiceApi::draft_requirements(self, input).await
    }

    async fn list_requirements(&self) -> Result<Vec<RequirementItem>> {
        PlanningServiceApi::list_requirements(self).await
    }

    async fn get_requirement(&self, id: &str) -> Result<RequirementItem> {
        PlanningServiceApi::get_requirement(self, id).await
    }

    async fn refine_requirements(
        &self,
        input: RequirementsRefineInput,
    ) -> Result<Vec<RequirementItem>> {
        PlanningServiceApi::refine_requirements(self, input).await
    }

    async fn upsert_requirement(&self, requirement: RequirementItem) -> Result<RequirementItem> {
        PlanningServiceApi::upsert_requirement(self, requirement).await
    }

    async fn delete_requirement(&self, id: &str) -> Result<()> {
        PlanningServiceApi::delete_requirement(self, id).await
    }

    async fn execute_requirements(
        &self,
        input: RequirementsExecutionInput,
    ) -> Result<RequirementsExecutionResult> {
        PlanningServiceApi::execute_requirements(self, input).await
    }
}
