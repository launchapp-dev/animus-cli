use std::path::{Path, PathBuf};
use std::process::{Command as ProcessCommand, Stdio};
use std::sync::Arc;

use anyhow::{Context, Result};
use protocol::orchestrator::{
    DependencyType, OrchestratorTask, RequirementItem, RequirementsDraftInput, RequirementsDraftResult,
    RequirementsExecutionInput, RequirementsExecutionResult, RequirementsRefineInput, TaskCreateInput, TaskFilter,
    TaskStatistics, TaskStatus, TaskUpdateInput, WorkflowSubject,
};

use crate::{
    PlanningServiceApi, ProjectAdapter, RequirementsProvider, SubjectContext, SubjectResolver, TaskProvider,
    TaskServiceApi,
};

#[derive(Clone)]
pub struct BuiltinTaskProvider<T> {
    hub: Arc<T>,
}

impl<T> BuiltinTaskProvider<T>
where
    T: TaskServiceApi,
{
    pub fn new(hub: Arc<T>) -> Self {
        Self { hub }
    }
}

#[async_trait::async_trait]
impl<T> TaskProvider for BuiltinTaskProvider<T>
where
    T: TaskServiceApi,
{
    async fn list(&self) -> Result<Vec<OrchestratorTask>> {
        self.hub.list().await
    }

    async fn list_filtered(&self, filter: TaskFilter) -> Result<Vec<OrchestratorTask>> {
        self.hub.list_filtered(filter).await
    }

    async fn list_prioritized(&self) -> Result<Vec<OrchestratorTask>> {
        self.hub.list_prioritized().await
    }

    async fn next_task(&self) -> Result<Option<OrchestratorTask>> {
        self.hub.next_task().await
    }

    async fn statistics(&self) -> Result<TaskStatistics> {
        self.hub.statistics().await
    }

    async fn get(&self, id: &str) -> Result<OrchestratorTask> {
        self.hub.get(id).await
    }

    async fn create(&self, input: TaskCreateInput) -> Result<OrchestratorTask> {
        self.hub.create(input).await
    }

    async fn update(&self, id: &str, input: TaskUpdateInput) -> Result<OrchestratorTask> {
        self.hub.update(id, input).await
    }

    async fn replace(&self, task: OrchestratorTask) -> Result<OrchestratorTask> {
        self.hub.replace(task).await
    }

    async fn delete(&self, id: &str) -> Result<()> {
        self.hub.delete(id).await
    }

    async fn assign(&self, id: &str, assignee: String) -> Result<OrchestratorTask> {
        self.hub.assign(id, assignee).await
    }

    async fn set_status(&self, id: &str, status: TaskStatus, validate: bool) -> Result<OrchestratorTask> {
        self.hub.set_status(id, status, validate).await
    }

    async fn add_checklist_item(&self, id: &str, description: String, updated_by: String) -> Result<OrchestratorTask> {
        self.hub.add_checklist_item(id, description, updated_by).await
    }

    async fn update_checklist_item(
        &self,
        id: &str,
        item_id: &str,
        completed: bool,
        updated_by: String,
    ) -> Result<OrchestratorTask> {
        self.hub.update_checklist_item(id, item_id, completed, updated_by).await
    }

    async fn add_dependency(
        &self,
        id: &str,
        dependency_id: &str,
        dependency_type: DependencyType,
        updated_by: String,
    ) -> Result<OrchestratorTask> {
        self.hub.add_dependency(id, dependency_id, dependency_type, updated_by).await
    }

    async fn remove_dependency(&self, id: &str, dependency_id: &str, updated_by: String) -> Result<OrchestratorTask> {
        self.hub.remove_dependency(id, dependency_id, updated_by).await
    }
}

#[derive(Clone)]
pub struct BuiltinRequirementsProvider<T> {
    hub: Arc<T>,
}

impl<T> BuiltinRequirementsProvider<T>
where
    T: PlanningServiceApi,
{
    pub fn new(hub: Arc<T>) -> Self {
        Self { hub }
    }
}

#[async_trait::async_trait]
impl<T> RequirementsProvider for BuiltinRequirementsProvider<T>
where
    T: PlanningServiceApi,
{
    async fn draft_requirements(&self, input: RequirementsDraftInput) -> Result<RequirementsDraftResult> {
        self.hub.draft_requirements(input).await
    }

    async fn list_requirements(&self) -> Result<Vec<RequirementItem>> {
        self.hub.list_requirements().await
    }

    async fn get_requirement(&self, id: &str) -> Result<RequirementItem> {
        self.hub.get_requirement(id).await
    }

    async fn refine_requirements(&self, input: RequirementsRefineInput) -> Result<Vec<RequirementItem>> {
        self.hub.refine_requirements(input).await
    }

    async fn upsert_requirement(&self, requirement: RequirementItem) -> Result<RequirementItem> {
        self.hub.upsert_requirement(requirement).await
    }

    async fn delete_requirement(&self, id: &str) -> Result<()> {
        self.hub.delete_requirement(id).await
    }

    async fn execute_requirements(&self, input: RequirementsExecutionInput) -> Result<RequirementsExecutionResult> {
        self.hub.execute_requirements(input).await
    }
}

#[derive(Clone)]
pub struct BuiltinSubjectResolver<T> {
    hub: Arc<T>,
}

impl<T> BuiltinSubjectResolver<T> {
    pub fn new(hub: Arc<T>) -> Self {
        Self { hub }
    }
}

#[async_trait::async_trait]
impl<T> SubjectResolver for BuiltinSubjectResolver<T>
where
    T: TaskServiceApi + PlanningServiceApi + Send + Sync,
{
    async fn resolve_subject_context(
        &self,
        subject: &WorkflowSubject,
        fallback_title: Option<&str>,
        fallback_description: Option<&str>,
    ) -> Result<SubjectContext> {
        match subject {
            WorkflowSubject::Task { id } => {
                let task = self.hub.get(id).await?;
                Ok(SubjectContext {
                    subject_id: id.clone(),
                    subject_title: task.title.clone(),
                    subject_description: task.description.clone(),
                    task: Some(task),
                })
            }
            WorkflowSubject::Requirement { id } => {
                let requirement = self.hub.get_requirement(id).await?;
                Ok(SubjectContext {
                    subject_id: id.clone(),
                    subject_title: requirement.title.clone(),
                    subject_description: requirement.description.clone(),
                    task: None,
                })
            }
            WorkflowSubject::Custom { title, description } => Ok(SubjectContext {
                subject_id: title.clone(),
                subject_title: fallback_title.unwrap_or(title).to_string(),
                subject_description: fallback_description.unwrap_or(description).to_string(),
                task: None,
            }),
        }
    }
}

#[derive(Clone)]
pub struct BuiltinProjectAdapter<T> {
    hub: Arc<T>,
}

impl<T> BuiltinProjectAdapter<T> {
    pub fn new(hub: Arc<T>) -> Self {
        Self { hub }
    }
}

#[async_trait::async_trait]
impl<T> ProjectAdapter for BuiltinProjectAdapter<T>
where
    T: TaskServiceApi + Send + Sync,
{
    async fn ensure_execution_cwd(&self, project_root: &str, task: Option<&OrchestratorTask>) -> Result<String> {
        let Some(task) = task else {
            return Ok(project_root.to_string());
        };
        if !is_git_repo(project_root) {
            return Ok(project_root.to_string());
        }

        let worktree_root = ensure_repo_worktree_root(project_root)?;
        let branch_name = task
            .branch_name
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| default_task_branch_name(&task.id));

        if let Some(existing_path_raw) = task.worktree_path.as_deref().map(str::trim).filter(|value| !value.is_empty())
        {
            let existing_path = PathBuf::from(existing_path_raw);
            if existing_path.exists() {
                if !path_is_within_root(&existing_path, &worktree_root) {
                    anyhow::bail!(
                        "task {} worktree path '{}' is outside managed worktree root '{}'",
                        task.id,
                        existing_path.display(),
                        worktree_root.display()
                    );
                }
                if task.branch_name.as_deref() != Some(branch_name.as_str()) {
                    let mut updated = task.clone();
                    updated.branch_name = Some(branch_name.clone());
                    let _ = self.hub.replace(updated).await?;
                }
                return Ok(existing_path.to_string_lossy().to_string());
            }
        }

        let worktree_path = default_task_worktree_path(project_root, &task.id)?;
        if worktree_path.exists() {
            if !path_is_within_root(&worktree_path, &worktree_root) {
                anyhow::bail!(
                    "task {} worktree path '{}' is outside managed worktree root '{}'",
                    task.id,
                    worktree_path.display(),
                    worktree_root.display()
                );
            }
            let mut updated = task.clone();
            updated.worktree_path = Some(worktree_path.to_string_lossy().to_string());
            updated.branch_name = Some(branch_name);
            let _ = self.hub.replace(updated).await?;
            return Ok(worktree_path.to_string_lossy().to_string());
        }

        if let Some(parent) = worktree_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let worktree_path_str = worktree_path.to_string_lossy().to_string();
        let branch_ref = format!("refs/heads/{branch_name}");
        let status = if git_ref_exists(project_root, &branch_ref) {
            ProcessCommand::new("git")
                .arg("-C")
                .arg(project_root)
                .args(["worktree", "add", worktree_path_str.as_str(), branch_name.as_str()])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .with_context(|| {
                    format!(
                        "failed to create worktree '{}' for existing branch '{}' in {}",
                        worktree_path_str, branch_name, project_root
                    )
                })?
        } else {
            refresh_preferred_worktree_base_refs(project_root);
            let base_ref = preferred_worktree_base_ref(project_root);
            ProcessCommand::new("git")
                .arg("-C")
                .arg(project_root)
                .args(["worktree", "add", "-b", branch_name.as_str(), worktree_path_str.as_str(), base_ref.as_str()])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .with_context(|| {
                    format!(
                        "failed to create worktree '{}' for branch '{}' from '{}' in {}",
                        worktree_path_str, branch_name, base_ref, project_root
                    )
                })?
        };

        if !status.success() {
            anyhow::bail!(
                "failed to provision managed worktree '{}' for task {} on branch '{}'",
                worktree_path_str,
                task.id,
                branch_name
            );
        }

        let mut updated = task.clone();
        updated.worktree_path = Some(worktree_path_str.clone());
        updated.branch_name = Some(branch_name);
        let _ = self.hub.replace(updated).await?;
        Ok(worktree_path_str)
    }
}

fn is_git_repo(project_root: &str) -> bool {
    ProcessCommand::new("git")
        .arg("-C")
        .arg(project_root)
        .args(["rev-parse", "--git-dir"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn default_task_branch_name(task_id: &str) -> String {
    format!("ao/{}", protocol::sanitize_identifier(task_id, "task"))
}

fn repo_ao_root(project_root: &str) -> Result<PathBuf> {
    protocol::scoped_state_root(Path::new(project_root))
        .ok_or_else(|| anyhow::anyhow!("failed to resolve scoped state root for {project_root}"))
}

fn repo_worktrees_root(project_root: &str) -> Result<PathBuf> {
    Ok(repo_ao_root(project_root)?.join("worktrees"))
}

fn ensure_repo_worktree_root(project_root: &str) -> Result<PathBuf> {
    let repo_root = repo_ao_root(project_root)?;
    let root = repo_worktrees_root(project_root)?;
    std::fs::create_dir_all(&repo_root)?;
    std::fs::create_dir_all(&root)?;

    let canonical = Path::new(project_root).canonicalize().unwrap_or_else(|_| PathBuf::from(project_root));
    let marker_path = repo_root.join(".project-root");
    let marker_content = format!("{}\n", canonical.to_string_lossy());
    let should_write_marker =
        std::fs::read_to_string(&marker_path).map(|existing| existing != marker_content).unwrap_or(true);
    if should_write_marker {
        std::fs::write(&marker_path, marker_content)?;
    }

    #[cfg(unix)]
    {
        let link_path = repo_root.join("project-root");
        if !link_path.exists() {
            let _ = std::os::unix::fs::symlink(&canonical, &link_path);
        }
    }

    Ok(root)
}

fn default_task_worktree_path(project_root: &str, task_id: &str) -> Result<PathBuf> {
    Ok(repo_worktrees_root(project_root)?.join(format!("task-{}", protocol::sanitize_identifier(task_id, "task"))))
}

fn path_is_within_root(path: &Path, root: &Path) -> bool {
    let Ok(path_canonical) = path.canonicalize() else {
        return false;
    };
    let Ok(root_canonical) = root.canonicalize() else {
        return false;
    };
    path_canonical.starts_with(root_canonical)
}

fn git_ref_exists(project_root: &str, reference: &str) -> bool {
    ProcessCommand::new("git")
        .arg("-C")
        .arg(project_root)
        .args(["rev-parse", "--verify", reference])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn preferred_worktree_base_ref(project_root: &str) -> String {
    for reference in
        ["refs/remotes/origin/main", "refs/heads/main", "refs/remotes/origin/master", "refs/heads/master", "HEAD"]
    {
        if git_ref_exists(project_root, reference) {
            return reference.to_string();
        }
    }
    "HEAD".to_string()
}

fn refresh_preferred_worktree_base_refs(project_root: &str) {
    for branch in ["main", "master"] {
        let _ = ProcessCommand::new("git")
            .arg("-C")
            .arg(project_root)
            .args(["fetch", "--no-tags", "origin", branch])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}
