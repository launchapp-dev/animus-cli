use std::collections::HashMap;
use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use std::process::{Command as ProcessCommand, Stdio};
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use protocol::orchestrator::{SubjectRef, SUBJECT_KIND_CUSTOM, SUBJECT_KIND_REQUIREMENT, SUBJECT_KIND_TASK};
use tracing::{debug, info};

use crate::{PlanningServiceApi, ProjectAdapter, SubjectContext, SubjectResolver, TaskServiceApi};

#[async_trait]
pub trait SubjectAdapter: Send + Sync {
    fn kind(&self) -> &'static str;

    async fn resolve_context(
        &self,
        subject: &SubjectRef,
        fallback_title: Option<&str>,
        fallback_description: Option<&str>,
    ) -> Result<SubjectContext>;

    async fn ensure_execution_cwd(
        &self,
        project_root: &str,
        subject: &SubjectRef,
        subject_context: &SubjectContext,
    ) -> Result<String>;
}

#[derive(Clone, Default)]
pub struct SubjectAdapterRegistry {
    adapters: HashMap<String, Arc<dyn SubjectAdapter>>,
}

impl SubjectAdapterRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn register(mut self, adapter: Arc<dyn SubjectAdapter>) -> Self {
        self.adapters.insert(adapter.kind().to_string(), adapter);
        self
    }

    pub async fn resolve_subject_context(
        &self,
        subject: &SubjectRef,
        fallback_title: Option<&str>,
        fallback_description: Option<&str>,
    ) -> Result<SubjectContext> {
        self.adapter_for(subject)?.resolve_context(subject, fallback_title, fallback_description).await
    }

    pub async fn ensure_execution_cwd(
        &self,
        project_root: &str,
        subject: &SubjectRef,
        subject_context: &SubjectContext,
    ) -> Result<String> {
        self.adapter_for(subject)?.ensure_execution_cwd(project_root, subject, subject_context).await
    }

    fn adapter_for(&self, subject: &SubjectRef) -> Result<&Arc<dyn SubjectAdapter>> {
        let kind = subject_kind(subject);
        self.adapters.get(kind).ok_or_else(|| anyhow!("no subject adapter registered for subject kind '{kind}'"))
    }
}

#[must_use]
pub fn builtin_subject_adapter_registry<T>(hub: Arc<T>) -> SubjectAdapterRegistry
where
    T: TaskServiceApi + PlanningServiceApi + Send + Sync + 'static,
{
    SubjectAdapterRegistry::new()
        .register(Arc::new(BuiltinTaskSubjectAdapter::new(hub.clone())))
        .register(Arc::new(BuiltinRequirementSubjectAdapter::new(hub)))
        .register(Arc::new(BuiltinCustomSubjectAdapter::default()))
}

#[derive(Clone)]
pub struct BuiltinTaskSubjectAdapter<T> {
    hub: Arc<T>,
}

impl<T> BuiltinTaskSubjectAdapter<T> {
    #[must_use]
    pub fn new(hub: Arc<T>) -> Self {
        Self { hub }
    }
}

#[async_trait]
impl<T> SubjectAdapter for BuiltinTaskSubjectAdapter<T>
where
    T: TaskServiceApi + Send + Sync + 'static,
{
    fn kind(&self) -> &'static str {
        SUBJECT_KIND_TASK
    }

    async fn resolve_context(
        &self,
        subject: &SubjectRef,
        _fallback_title: Option<&str>,
        _fallback_description: Option<&str>,
    ) -> Result<SubjectContext> {
        let Some(id) = subject.task_id() else {
            anyhow::bail!("task subject adapter received non-task subject '{}'", subject_kind(subject));
        };
        let task = self.hub.get(id).await?;
        let mut attributes = HashMap::new();
        attributes.insert("task_type".to_string(), task.task_type.as_str().to_string());
        attributes.insert("priority".to_string(), task.priority.as_str().to_string());
        Ok(SubjectContext {
            subject_kind: SUBJECT_KIND_TASK.to_string(),
            subject_id: id.to_string(),
            subject_title: task.title.clone(),
            subject_description: task.description.clone(),
            attributes,
            task: Some(task),
        })
    }

    async fn ensure_execution_cwd(
        &self,
        project_root: &str,
        subject: &SubjectRef,
        subject_context: &SubjectContext,
    ) -> Result<String> {
        let Some(id) = subject.task_id() else {
            anyhow::bail!("task subject adapter received non-task subject '{}'", subject_kind(subject));
        };

        let task = match subject_context.task.as_ref() {
            Some(task) => task.clone(),
            None => self.hub.get(id).await?,
        };

        if !is_git_repo(project_root) {
            info!(
                task_id = %task.id,
                project_root,
                "Project root is not a git repository; using project root as execution cwd"
            );
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
                sync_managed_worktree_mcp_config(project_root, &existing_path)?;
                info!(
                    task_id = %task.id,
                    branch_name,
                    execution_cwd = %existing_path.display(),
                    source = "task.worktree_path",
                    "Using existing managed task worktree as execution cwd"
                );
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
            sync_managed_worktree_mcp_config(project_root, &worktree_path)?;
            let mut updated = task.clone();
            updated.worktree_path = Some(worktree_path.to_string_lossy().to_string());
            updated.branch_name = Some(branch_name.clone());
            let _ = self.hub.replace(updated).await?;
            info!(
                task_id = %task.id,
                branch_name,
                execution_cwd = %worktree_path.display(),
                source = "default_task_worktree",
                "Reusing managed task worktree as execution cwd"
            );
            return Ok(worktree_path.to_string_lossy().to_string());
        }

        if let Some(parent) = worktree_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let worktree_path_str = worktree_path.to_string_lossy().to_string();
        let branch_ref = format!("refs/heads/{branch_name}");
        let status = if git_ref_exists(project_root, &branch_ref) {
            info!(
                task_id = %task.id,
                branch_name,
                execution_cwd = %worktree_path_str,
                source = "existing_branch",
                "Provisioning managed task worktree from existing branch"
            );
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
            info!(
                task_id = %task.id,
                branch_name,
                base_ref,
                execution_cwd = %worktree_path_str,
                source = "preferred_base_ref",
                "Provisioning managed task worktree from preferred base ref"
            );
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

        sync_managed_worktree_mcp_config(project_root, &worktree_path)?;
        let mut updated = task;
        let task_id = updated.id.clone();
        updated.worktree_path = Some(worktree_path_str.clone());
        updated.branch_name = Some(branch_name.clone());
        let _ = self.hub.replace(updated).await?;
        info!(
            task_id = %task_id,
            branch_name,
            execution_cwd = %worktree_path_str,
            "Provisioned managed task worktree"
        );
        Ok(worktree_path_str)
    }
}

#[derive(Clone)]
pub struct BuiltinRequirementSubjectAdapter<T> {
    hub: Arc<T>,
}

impl<T> BuiltinRequirementSubjectAdapter<T> {
    #[must_use]
    pub fn new(hub: Arc<T>) -> Self {
        Self { hub }
    }
}

#[async_trait]
impl<T> SubjectAdapter for BuiltinRequirementSubjectAdapter<T>
where
    T: PlanningServiceApi + Send + Sync + 'static,
{
    fn kind(&self) -> &'static str {
        SUBJECT_KIND_REQUIREMENT
    }

    async fn resolve_context(
        &self,
        subject: &SubjectRef,
        _fallback_title: Option<&str>,
        _fallback_description: Option<&str>,
    ) -> Result<SubjectContext> {
        let Some(id) = subject.requirement_id() else {
            anyhow::bail!("requirement subject adapter received non-requirement subject '{}'", subject_kind(subject));
        };
        let requirement = self.hub.get_requirement(id).await?;
        let mut attributes = HashMap::new();
        attributes.insert("priority".to_string(), format!("{:?}", requirement.priority).to_ascii_lowercase());
        attributes.insert("status".to_string(), requirement.status.to_string());
        Ok(SubjectContext {
            subject_kind: SUBJECT_KIND_REQUIREMENT.to_string(),
            subject_id: id.to_string(),
            subject_title: requirement.title,
            subject_description: requirement.description,
            attributes,
            task: None,
        })
    }

    async fn ensure_execution_cwd(
        &self,
        project_root: &str,
        _subject: &SubjectRef,
        _subject_context: &SubjectContext,
    ) -> Result<String> {
        Ok(project_root.to_string())
    }
}

#[derive(Clone, Default)]
pub struct BuiltinCustomSubjectAdapter {
    _private: PhantomData<()>,
}

#[async_trait]
impl SubjectAdapter for BuiltinCustomSubjectAdapter {
    fn kind(&self) -> &'static str {
        SUBJECT_KIND_CUSTOM
    }

    async fn resolve_context(
        &self,
        subject: &SubjectRef,
        fallback_title: Option<&str>,
        fallback_description: Option<&str>,
    ) -> Result<SubjectContext> {
        if !subject.kind().eq_ignore_ascii_case(SUBJECT_KIND_CUSTOM) {
            anyhow::bail!("custom subject adapter received non-custom subject '{}'", subject_kind(subject));
        }
        let title = subject.title.as_deref().unwrap_or(subject.id());
        let description = subject.description.as_deref().unwrap_or_default();
        Ok(SubjectContext {
            subject_kind: SUBJECT_KIND_CUSTOM.to_string(),
            subject_id: subject.id().to_string(),
            subject_title: fallback_title.unwrap_or(title).to_string(),
            subject_description: fallback_description.unwrap_or(description).to_string(),
            attributes: HashMap::new(),
            task: None,
        })
    }

    async fn ensure_execution_cwd(
        &self,
        project_root: &str,
        _subject: &SubjectRef,
        _subject_context: &SubjectContext,
    ) -> Result<String> {
        Ok(project_root.to_string())
    }
}

fn subject_kind(subject: &SubjectRef) -> &str {
    subject.kind()
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
        .ok_or_else(|| anyhow!("failed to resolve scoped state root for {project_root}"))
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

#[derive(Debug, Clone)]
struct ManagedWorktreeMcpLaunch {
    kind: &'static str,
    command: String,
    args: Vec<String>,
}

impl ManagedWorktreeMcpLaunch {
    fn as_json(&self) -> serde_json::Value {
        serde_json::json!({
            "command": self.command,
            "args": self.args
        })
    }
}

fn sync_managed_worktree_mcp_config(project_root: &str, worktree_path: &Path) -> Result<()> {
    let canonical_root = Path::new(project_root).canonicalize().unwrap_or_else(|_| PathBuf::from(project_root));
    let launch = managed_worktree_mcp_server_config(&canonical_root);
    let mcp_payload = serde_json::json!({
        "mcpServers": {
            "ao": launch.as_json()
        }
    });
    let serialized =
        format!("{}\n", serde_json::to_string_pretty(&mcp_payload).context("failed to serialize worktree MCP config")?);
    let mcp_path = worktree_path.join(".mcp.json");

    let should_write = std::fs::read_to_string(&mcp_path).map(|existing| existing != serialized).unwrap_or(true);
    if should_write {
        std::fs::write(&mcp_path, serialized)
            .with_context(|| format!("failed to write worktree MCP config at {}", mcp_path.display()))?;
        info!(
            project_root = %canonical_root.display(),
            worktree_path = %worktree_path.display(),
            mcp_path = %mcp_path.display(),
            launcher = launch.kind,
            command = %launch.command,
            args = ?launch.args,
            "Rewrote managed worktree MCP config"
        );
    } else {
        debug!(
            project_root = %canonical_root.display(),
            worktree_path = %worktree_path.display(),
            mcp_path = %mcp_path.display(),
            launcher = launch.kind,
            command = %launch.command,
            args = ?launch.args,
            "Managed worktree MCP config already up to date"
        );
    }

    Ok(())
}

fn managed_worktree_mcp_server_config(project_root: &Path) -> ManagedWorktreeMcpLaunch {
    if let Some(binary_path) = preferred_repo_ao_binary(project_root) {
        return ManagedWorktreeMcpLaunch {
            kind: "repo_binary",
            command: binary_path.to_string_lossy().to_string(),
            args: vec![
                "--project-root".to_string(),
                project_root.to_string_lossy().to_string(),
                "mcp".to_string(),
                "serve".to_string(),
            ],
        };
    }

    ManagedWorktreeMcpLaunch {
        kind: "cargo_manifest",
        command: "cargo".to_string(),
        args: vec![
            "run".to_string(),
            "--manifest-path".to_string(),
            project_root.join("crates/orchestrator-cli/Cargo.toml").to_string_lossy().to_string(),
            "--".to_string(),
            "--project-root".to_string(),
            project_root.to_string_lossy().to_string(),
            "mcp".to_string(),
            "serve".to_string(),
        ],
    }
}

fn preferred_repo_ao_binary(project_root: &Path) -> Option<PathBuf> {
    ["debug", "release"]
        .into_iter()
        .map(|profile| project_root.join("target").join(profile).join(repo_ao_binary_name()))
        .find(|path| path.exists())
}

fn repo_ao_binary_name() -> &'static str {
    #[cfg(target_os = "windows")]
    {
        "ao.exe"
    }

    #[cfg(not(target_os = "windows"))]
    {
        "ao"
    }
}

#[derive(Clone)]
pub struct BuiltinSubjectResolver {
    registry: SubjectAdapterRegistry,
}

impl BuiltinSubjectResolver {
    #[must_use]
    pub fn new<T>(hub: Arc<T>) -> Self
    where
        T: TaskServiceApi + PlanningServiceApi + Send + Sync + 'static,
    {
        Self { registry: builtin_subject_adapter_registry(hub) }
    }
}

#[async_trait]
impl SubjectResolver for BuiltinSubjectResolver {
    async fn resolve_subject_context(
        &self,
        subject: &SubjectRef,
        fallback_title: Option<&str>,
        fallback_description: Option<&str>,
    ) -> Result<SubjectContext> {
        self.registry.resolve_subject_context(subject, fallback_title, fallback_description).await
    }
}

#[derive(Clone)]
pub struct BuiltinProjectAdapter {
    registry: SubjectAdapterRegistry,
}

impl BuiltinProjectAdapter {
    #[must_use]
    pub fn new<T>(hub: Arc<T>) -> Self
    where
        T: TaskServiceApi + PlanningServiceApi + Send + Sync + 'static,
    {
        Self { registry: builtin_subject_adapter_registry(hub) }
    }
}

#[async_trait]
impl ProjectAdapter for BuiltinProjectAdapter {
    async fn ensure_execution_cwd(
        &self,
        project_root: &str,
        subject: &SubjectRef,
        subject_context: &SubjectContext,
    ) -> Result<String> {
        self.registry.ensure_execution_cwd(project_root, subject, subject_context).await
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Mutex;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;
    use protocol::orchestrator::{
        Assignee, Complexity, DependencyType, OrchestratorTask, Priority, RequirementItem, RequirementLinks,
        RequirementPriority, RequirementStatus, RequirementsDraftInput, RequirementsDraftResult,
        RequirementsExecutionInput, RequirementsExecutionResult, RequirementsRefineInput, ResourceRequirements,
        RiskLevel, Scope, SubjectRef, TaskCreateInput, TaskFilter, TaskMetadata, TaskStatistics, TaskStatus, TaskType,
        TaskUpdateInput, WorkflowMetadata,
    };

    #[derive(Default)]
    struct TestHub {
        tasks: Mutex<HashMap<String, OrchestratorTask>>,
        requirements: Mutex<HashMap<String, RequirementItem>>,
    }

    #[async_trait]
    impl TaskServiceApi for TestHub {
        async fn list(&self) -> Result<Vec<OrchestratorTask>> {
            unimplemented!()
        }

        async fn list_filtered(&self, _filter: TaskFilter) -> Result<Vec<OrchestratorTask>> {
            unimplemented!()
        }

        async fn list_prioritized(&self) -> Result<Vec<OrchestratorTask>> {
            unimplemented!()
        }

        async fn next_task(&self) -> Result<Option<OrchestratorTask>> {
            unimplemented!()
        }

        async fn statistics(&self) -> Result<TaskStatistics> {
            Ok(TaskStatistics {
                total: 0,
                by_status: HashMap::new(),
                by_priority: HashMap::new(),
                by_type: HashMap::new(),
                in_progress: 0,
                blocked: 0,
                completed: 0,
            })
        }

        async fn get(&self, id: &str) -> Result<OrchestratorTask> {
            self.tasks.lock().unwrap().get(id).cloned().ok_or_else(|| anyhow!("task not found: {id}"))
        }

        async fn create(&self, _input: TaskCreateInput) -> Result<OrchestratorTask> {
            unimplemented!()
        }

        async fn update(&self, _id: &str, _input: TaskUpdateInput) -> Result<OrchestratorTask> {
            unimplemented!()
        }

        async fn replace(&self, task: OrchestratorTask) -> Result<OrchestratorTask> {
            self.tasks.lock().unwrap().insert(task.id.clone(), task.clone());
            Ok(task)
        }

        async fn delete(&self, _id: &str) -> Result<()> {
            unimplemented!()
        }

        async fn assign(&self, _id: &str, _assignee: String) -> Result<OrchestratorTask> {
            unimplemented!()
        }

        async fn set_status(&self, _id: &str, _status: TaskStatus, _validate: bool) -> Result<OrchestratorTask> {
            unimplemented!()
        }

        async fn add_checklist_item(
            &self,
            _id: &str,
            _description: String,
            _updated_by: String,
        ) -> Result<OrchestratorTask> {
            unimplemented!()
        }

        async fn update_checklist_item(
            &self,
            _id: &str,
            _item_id: &str,
            _completed: bool,
            _updated_by: String,
        ) -> Result<OrchestratorTask> {
            unimplemented!()
        }

        async fn add_dependency(
            &self,
            _id: &str,
            _dependency_id: &str,
            _dependency_type: DependencyType,
            _updated_by: String,
        ) -> Result<OrchestratorTask> {
            unimplemented!()
        }

        async fn remove_dependency(
            &self,
            _id: &str,
            _dependency_id: &str,
            _updated_by: String,
        ) -> Result<OrchestratorTask> {
            unimplemented!()
        }
    }

    #[async_trait]
    impl PlanningServiceApi for TestHub {
        async fn draft_requirements(&self, _input: RequirementsDraftInput) -> Result<RequirementsDraftResult> {
            unimplemented!()
        }

        async fn list_requirements(&self) -> Result<Vec<RequirementItem>> {
            unimplemented!()
        }

        async fn get_requirement(&self, id: &str) -> Result<RequirementItem> {
            self.requirements.lock().unwrap().get(id).cloned().ok_or_else(|| anyhow!("requirement not found: {id}"))
        }

        async fn refine_requirements(&self, _input: RequirementsRefineInput) -> Result<Vec<RequirementItem>> {
            unimplemented!()
        }

        async fn upsert_requirement(&self, requirement: RequirementItem) -> Result<RequirementItem> {
            self.requirements.lock().unwrap().insert(requirement.id.clone(), requirement.clone());
            Ok(requirement)
        }

        async fn delete_requirement(&self, _id: &str) -> Result<()> {
            unimplemented!()
        }

        async fn execute_requirements(
            &self,
            _input: RequirementsExecutionInput,
        ) -> Result<RequirementsExecutionResult> {
            unimplemented!()
        }
    }

    fn sample_task(id: &str) -> OrchestratorTask {
        let now = chrono::Utc::now();
        OrchestratorTask {
            id: id.to_string(),
            title: "Task title".to_string(),
            description: "Task description".to_string(),
            task_type: TaskType::Feature,
            status: TaskStatus::Ready,
            blocked_reason: None,
            blocked_at: None,
            blocked_phase: None,
            blocked_by: None,
            priority: Priority::Medium,
            risk: RiskLevel::Medium,
            scope: Scope::Medium,
            complexity: Complexity::default(),
            impact_area: Vec::new(),
            assignee: Assignee::Unassigned,
            estimated_effort: None,
            linked_requirements: Vec::new(),
            linked_architecture_entities: Vec::new(),
            dependencies: Vec::new(),
            checklist: Vec::new(),
            tags: Vec::new(),
            workflow_metadata: WorkflowMetadata::default(),
            branch_name: None,
            worktree_path: None,
            metadata: TaskMetadata {
                created_at: now,
                updated_at: now,
                created_by: "test".to_string(),
                updated_by: "test".to_string(),
                started_at: None,
                completed_at: None,
                version: 1,
            },
            deadline: None,
            paused: false,
            cancelled: false,
            resolution: None,
            resource_requirements: ResourceRequirements::default(),
            consecutive_dispatch_failures: None,
            last_dispatch_failure_at: None,
            dispatch_history: Vec::new(),
        }
    }

    fn sample_requirement(id: &str) -> RequirementItem {
        let now = chrono::Utc::now();
        RequirementItem {
            id: id.to_string(),
            title: "Requirement title".to_string(),
            description: "Requirement description".to_string(),
            body: None,
            legacy_id: None,
            category: None,
            requirement_type: None,
            acceptance_criteria: Vec::new(),
            priority: RequirementPriority::Should,
            status: RequirementStatus::Refined,
            source: "test".to_string(),
            tags: Vec::new(),
            links: RequirementLinks::default(),
            comments: Vec::new(),
            relative_path: None,
            linked_task_ids: Vec::new(),
            created_at: now,
            updated_at: now,
        }
    }

    fn temp_dir(prefix: &str) -> PathBuf {
        let unique = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let path = std::env::temp_dir().join(format!("ao-subject-adapter-{prefix}-{unique}"));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    fn run_git(project_root: &Path, args: &[&str]) {
        let status = ProcessCommand::new("git").arg("-C").arg(project_root).args(args).status().unwrap();
        assert!(status.success(), "git {:?} failed in {}", args, project_root.display());
    }

    #[tokio::test]
    async fn builtin_subject_resolver_uses_requirement_adapter_registry() {
        let hub = Arc::new(TestHub::default());
        hub.upsert_requirement(sample_requirement("REQ-1")).await.unwrap();

        let resolver = BuiltinSubjectResolver::new(hub);
        let context =
            resolver.resolve_subject_context(&SubjectRef::requirement("REQ-1".to_string()), None, None).await.unwrap();

        assert_eq!(context.subject_kind, SUBJECT_KIND_REQUIREMENT);
        assert_eq!(context.subject_id, "REQ-1");
        assert_eq!(context.subject_title, "Requirement title");
        assert_eq!(context.subject_description, "Requirement description");
        assert!(context.task.is_none());
    }

    #[tokio::test]
    async fn builtin_project_adapter_returns_project_root_for_requirement_subjects() {
        let hub = Arc::new(TestHub::default());
        hub.upsert_requirement(sample_requirement("REQ-2")).await.unwrap();

        let resolver = BuiltinSubjectResolver::new(hub.clone());
        let adapter = BuiltinProjectAdapter::new(hub);
        let subject = SubjectRef::requirement("REQ-2".to_string());
        let context = resolver.resolve_subject_context(&subject, None, None).await.unwrap();
        let cwd = adapter.ensure_execution_cwd("/tmp/example-root", &subject, &context).await.unwrap();

        assert_eq!(cwd, "/tmp/example-root");
    }

    #[tokio::test]
    async fn builtin_project_adapter_provisions_task_worktree_via_task_adapter() {
        let project_root = temp_dir("task");
        let canonical_project_root = project_root.canonicalize().unwrap();
        run_git(&project_root, &["init", "--initial-branch=main"]);
        run_git(&project_root, &["config", "user.email", "ao@example.com"]);
        run_git(&project_root, &["config", "user.name", "AO"]);
        std::fs::write(project_root.join("README.md"), "hello\n").unwrap();
        let repo_binary_path = canonical_project_root.join("target").join("debug").join(repo_ao_binary_name());
        std::fs::create_dir_all(repo_binary_path.parent().unwrap()).unwrap();
        std::fs::write(&repo_binary_path, "#!/bin/sh\n").unwrap();
        run_git(&project_root, &["add", "README.md"]);
        run_git(&project_root, &["commit", "-m", "init"]);

        let hub = Arc::new(TestHub::default());
        hub.replace(sample_task("TASK-1")).await.unwrap();

        let resolver = BuiltinSubjectResolver::new(hub.clone());
        let adapter = BuiltinProjectAdapter::new(hub.clone());
        let subject = SubjectRef::task("TASK-1".to_string());
        let context = resolver.resolve_subject_context(&subject, None, None).await.unwrap();
        let cwd = adapter.ensure_execution_cwd(project_root.to_str().unwrap(), &subject, &context).await.unwrap();

        assert!(cwd.contains("task-task-1"), "unexpected worktree path: {cwd}");
        assert!(Path::new(&cwd).exists(), "worktree path should exist: {cwd}");
        let mcp_config: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(Path::new(&cwd).join(".mcp.json")).unwrap()).unwrap();
        assert_eq!(
            mcp_config.pointer("/mcpServers/ao/command").and_then(serde_json::Value::as_str),
            Some(repo_binary_path.to_string_lossy().as_ref())
        );
        assert_eq!(
            mcp_config.pointer("/mcpServers/ao/args").and_then(serde_json::Value::as_array).cloned(),
            Some(vec![
                serde_json::Value::String("--project-root".to_string()),
                serde_json::Value::String(canonical_project_root.to_string_lossy().to_string()),
                serde_json::Value::String("mcp".to_string()),
                serde_json::Value::String("serve".to_string()),
            ])
        );

        let updated = hub.get("TASK-1").await.unwrap();
        assert_eq!(updated.worktree_path.as_deref(), Some(cwd.as_str()));
        assert_eq!(updated.branch_name.as_deref(), Some("ao/task-1"));
    }

    #[test]
    fn managed_worktree_mcp_config_falls_back_to_primary_repo_manifest_path() {
        let project_root = temp_dir("mcp-project");
        let worktree_path = temp_dir("mcp-worktree");

        let server = managed_worktree_mcp_server_config(&project_root);
        assert_eq!(server.kind, "cargo_manifest");
        assert_eq!(server.command, "cargo");
        assert_eq!(
            server.args,
            vec![
                "run".to_string(),
                "--manifest-path".to_string(),
                project_root.join("crates/orchestrator-cli/Cargo.toml").to_string_lossy().to_string(),
                "--".to_string(),
                "--project-root".to_string(),
                project_root.to_string_lossy().to_string(),
                "mcp".to_string(),
                "serve".to_string(),
            ]
        );

        sync_managed_worktree_mcp_config(project_root.to_str().unwrap(), &worktree_path).unwrap();
        let persisted: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(worktree_path.join(".mcp.json")).unwrap()).unwrap();
        assert_eq!(persisted.get("mcpServers").and_then(serde_json::Value::as_object).map(|map| map.len()), Some(1));
        assert_eq!(persisted.pointer("/mcpServers/ao/command").and_then(serde_json::Value::as_str), Some("cargo"));
    }
}
