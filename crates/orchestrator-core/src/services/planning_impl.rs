use super::*;

#[async_trait]
impl PlanningServiceApi for InMemoryServiceHub {
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
            requirement.source = "tauri".to_string();
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

    async fn execute_requirements(
        &self,
        input: RequirementsExecutionInput,
    ) -> Result<RequirementsExecutionResult> {
        let mut lock = self.state.write().await;
        planning_shared::execute_requirements_and_record(&mut lock, input, None, None)
    }
}

#[async_trait]
impl PlanningServiceApi for FileServiceHub {
    async fn draft_vision(&self, input: VisionDraftInput) -> Result<VisionDocument> {
        let now = Utc::now();
        let active_project_name = {
            let lock = self.state.read().await;
            lock.active_project_id
                .as_ref()
                .and_then(|id| lock.projects.get(id).map(|project| project.name.clone()))
        };
        let project_name = input
            .project_name
            .clone()
            .or(active_project_name)
            .unwrap_or_else(|| default_vision_project_name(&self.project_root));

        let (vision, snapshot) = {
            let mut lock = self.state.write().await;
            let vision = planning_shared::draft_vision_and_record(
                &mut lock,
                self.project_root.display().to_string(),
                project_name,
                input,
                now,
            );
            (vision, lock.clone())
        };

        write_planning_artifacts(
            &self.project_root,
            snapshot.vision.as_ref(),
            &snapshot.requirements,
        )?;
        Self::persist_snapshot(&self.state_file, &snapshot)?;
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

        let (snapshot, appended_ids, appended_count) = {
            let mut lock = self.state.write().await;
            let (appended_ids, appended_count) = planning_shared::draft_requirements_and_record(
                &mut lock,
                input,
                codebase_insight.as_ref(),
            )?;

            (lock.clone(), appended_ids, appended_count)
        };

        write_planning_artifacts(
            &self.project_root,
            snapshot.vision.as_ref(),
            &snapshot.requirements,
        )?;
        Self::persist_snapshot(&self.state_file, &snapshot)?;

        let requirements = planning_shared::requirements_by_ids_sorted(&snapshot, &appended_ids);

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

    async fn get_requirement(&self, id: &str) -> Result<RequirementItem> {
        let lock = self.state.read().await;
        planning_shared::get_requirement(&lock, id)
    }

    async fn refine_requirements(
        &self,
        input: RequirementsRefineInput,
    ) -> Result<Vec<RequirementItem>> {
        let (snapshot, refined) = {
            let mut lock = self.state.write().await;
            let refined = planning_shared::refine_requirements_and_record(&mut lock, input);

            (lock.clone(), refined)
        };

        write_planning_artifacts(
            &self.project_root,
            snapshot.vision.as_ref(),
            &snapshot.requirements,
        )?;
        Self::persist_snapshot(&self.state_file, &snapshot)?;
        Ok(refined)
    }

    async fn upsert_requirement(
        &self,
        mut requirement: RequirementItem,
    ) -> Result<RequirementItem> {
        let snapshot = {
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
                requirement.source = "tauri".to_string();
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
            lock.clone()
        };

        write_planning_artifacts(
            &self.project_root,
            snapshot.vision.as_ref(),
            &snapshot.requirements,
        )?;
        Self::persist_snapshot(&self.state_file, &snapshot)?;
        Ok(requirement)
    }

    async fn delete_requirement(&self, id: &str) -> Result<()> {
        let snapshot = {
            let mut lock = self.state.write().await;
            if lock.requirements.remove(id).is_none() {
                return Err(anyhow!("requirement not found: {id}"));
            }
            lock.logs.push(LogEntry {
                timestamp: Utc::now(),
                level: LogLevel::Info,
                message: format!("requirement deleted ({id})"),
            });
            lock.clone()
        };

        write_planning_artifacts(
            &self.project_root,
            snapshot.vision.as_ref(),
            &snapshot.requirements,
        )?;
        Self::persist_snapshot(&self.state_file, &snapshot)?;
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

        let (snapshot, result) = {
            let mut lock = self.state.write().await;
            let result = planning_shared::execute_requirements_and_record(
                &mut lock,
                input,
                Some(&manager),
                Some(&loaded_state_machines.compiled),
            )?;

            (lock.clone(), result)
        };

        write_planning_artifacts(
            &self.project_root,
            snapshot.vision.as_ref(),
            &snapshot.requirements,
        )?;
        Self::persist_snapshot(&self.state_file, &snapshot)?;
        Ok(result)
    }
}
