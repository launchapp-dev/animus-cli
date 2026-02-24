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
        self.log(
            LogLevel::Info,
            format!("project upserted: {}", project.name),
        );
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
        let (snapshot, project) = {
            let mut lock = self.state.write().await;
            let project = project_shared::create_project(&mut lock, input, now);
            lock.logs.push(LogEntry {
                timestamp: Utc::now(),
                level: LogLevel::Info,
                message: format!("project created: {}", project.name),
            });
            (lock.clone(), project)
        };

        Self::persist_snapshot(&self.state_file, &snapshot)?;
        Ok(project)
    }

    async fn upsert(&self, project: OrchestratorProject) -> Result<OrchestratorProject> {
        FileServiceHub::bootstrap_project_base_configs(std::path::Path::new(&project.path))?;
        let now = Utc::now();
        let (snapshot, project) = {
            let mut lock = self.state.write().await;
            let project = project_shared::upsert_project(&mut lock, project, now);
            lock.logs.push(LogEntry {
                timestamp: Utc::now(),
                level: LogLevel::Info,
                message: format!("project upserted: {}", project.name),
            });
            (lock.clone(), project)
        };

        Self::persist_snapshot(&self.state_file, &snapshot)?;
        Ok(project)
    }

    async fn load(&self, id: &str) -> Result<OrchestratorProject> {
        let (snapshot, project) = {
            let mut lock = self.state.write().await;
            let project = project_shared::load_project(&mut lock, id)?;
            (lock.clone(), project)
        };

        FileServiceHub::bootstrap_project_base_configs(std::path::Path::new(&project.path))?;
        Self::persist_snapshot(&self.state_file, &snapshot)?;
        Ok(project)
    }

    async fn rename(&self, id: &str, new_name: &str) -> Result<OrchestratorProject> {
        let (snapshot, project) = {
            let mut lock = self.state.write().await;
            let project = project_shared::rename_project(&mut lock, id, new_name, Utc::now())?;
            (lock.clone(), project)
        };

        Self::persist_snapshot(&self.state_file, &snapshot)?;
        Ok(project)
    }

    async fn archive(&self, id: &str) -> Result<OrchestratorProject> {
        let (snapshot, project) = {
            let mut lock = self.state.write().await;
            let project = project_shared::archive_project(&mut lock, id, Utc::now())?;
            (lock.clone(), project)
        };

        Self::persist_snapshot(&self.state_file, &snapshot)?;
        Ok(project)
    }

    async fn remove(&self, id: &str) -> Result<()> {
        let snapshot = {
            let mut lock = self.state.write().await;
            project_shared::remove_project(&mut lock, id)?;
            lock.clone()
        };

        Self::persist_snapshot(&self.state_file, &snapshot)
    }
}
