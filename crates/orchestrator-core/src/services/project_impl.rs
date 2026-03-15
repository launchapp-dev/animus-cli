use super::*;

#[async_trait]
impl ProjectServiceApi for InMemoryServiceHub {
    async fn list(&self) -> Result<Vec<OrchestratorProject>> {
        let lock = self.state.read().await;
        Ok(project_shared::list_projects(&lock))
    }

    async fn get(&self, id: &str) -> Result<OrchestratorProject> {
        let lock = self.state.read().await;
        project_shared::get_project(&lock, id)
    }

    async fn active(&self) -> Result<Option<OrchestratorProject>> {
        let lock = self.state.read().await;
        Ok(project_shared::active_project(&lock))
    }

    async fn create(&self, input: ProjectCreateInput) -> Result<OrchestratorProject> {
        let now = Utc::now();
        let project = {
            let mut lock = self.state.write().await;
            project_shared::create_project(&mut lock, input, now)
        };
        self.log(LogLevel::Info, format!("project created: {}", project.name));
        Ok(project)
    }

    async fn upsert(&self, project: OrchestratorProject) -> Result<OrchestratorProject> {
        let now = Utc::now();
        let project = {
            let mut lock = self.state.write().await;
            project_shared::upsert_project(&mut lock, project, now)
        };
        self.log(LogLevel::Info, format!("project upserted: {}", project.name));
        Ok(project)
    }

    async fn load(&self, id: &str) -> Result<OrchestratorProject> {
        let mut lock = self.state.write().await;
        project_shared::load_project(&mut lock, id)
    }

    async fn rename(&self, id: &str, new_name: &str) -> Result<OrchestratorProject> {
        let mut lock = self.state.write().await;
        project_shared::rename_project(&mut lock, id, new_name, Utc::now())
    }

    async fn archive(&self, id: &str) -> Result<OrchestratorProject> {
        let mut lock = self.state.write().await;
        project_shared::archive_project(&mut lock, id, Utc::now())
    }

    async fn remove(&self, id: &str) -> Result<()> {
        let mut lock = self.state.write().await;
        project_shared::remove_project(&mut lock, id)
    }
}

#[async_trait]
impl ProjectServiceApi for FileServiceHub {
    async fn list(&self) -> Result<Vec<OrchestratorProject>> {
        let lock = self.state.read().await;
        Ok(project_shared::list_projects(&lock))
    }

    async fn get(&self, id: &str) -> Result<OrchestratorProject> {
        let lock = self.state.read().await;
        project_shared::get_project(&lock, id)
    }

    async fn active(&self) -> Result<Option<OrchestratorProject>> {
        let lock = self.state.read().await;
        Ok(project_shared::active_project(&lock))
    }

    async fn create(&self, input: ProjectCreateInput) -> Result<OrchestratorProject> {
        FileServiceHub::bootstrap_project_base_configs(std::path::Path::new(&input.path))?;
        let now = Utc::now();
        let (project, _) = self
            .mutate_persistent_state(|state| {
                let project = project_shared::create_project(state, input, now);
                state.logs.push(LogEntry {
                    timestamp: Utc::now(),
                    level: LogLevel::Info,
                    message: format!("project created: {}", project.name),
                });
                Ok(project)
            })
            .await?;
        Ok(project)
    }

    async fn upsert(&self, project: OrchestratorProject) -> Result<OrchestratorProject> {
        FileServiceHub::bootstrap_project_base_configs(std::path::Path::new(&project.path))?;
        let now = Utc::now();
        let (project, _) = self
            .mutate_persistent_state(|state| {
                let project = project_shared::upsert_project(state, project, now);
                state.logs.push(LogEntry {
                    timestamp: Utc::now(),
                    level: LogLevel::Info,
                    message: format!("project upserted: {}", project.name),
                });
                Ok(project)
            })
            .await?;
        Ok(project)
    }

    async fn load(&self, id: &str) -> Result<OrchestratorProject> {
        let (project, _) = self.mutate_persistent_state(|state| project_shared::load_project(state, id)).await?;

        FileServiceHub::bootstrap_project_base_configs(std::path::Path::new(&project.path))?;
        Ok(project)
    }

    async fn rename(&self, id: &str, new_name: &str) -> Result<OrchestratorProject> {
        let (project, _) = self
            .mutate_persistent_state(|state| project_shared::rename_project(state, id, new_name, Utc::now()))
            .await?;
        Ok(project)
    }

    async fn archive(&self, id: &str) -> Result<OrchestratorProject> {
        let (project, _) =
            self.mutate_persistent_state(|state| project_shared::archive_project(state, id, Utc::now())).await?;
        Ok(project)
    }

    async fn remove(&self, id: &str) -> Result<()> {
        self.mutate_persistent_state(|state| project_shared::remove_project(state, id)).await?;
        Ok(())
    }
}
