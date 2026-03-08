//! DEPRECATED: Will be replaced by GitProvider trait. See providers/git.rs
use super::*;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum GitIntegrationOperation {
    PushBranch {
        cwd: String,
        remote: String,
        branch: String,
    },
    PushRef {
        cwd: String,
        remote: String,
        source_ref: String,
        target_ref: String,
    },
    OpenPullRequest {
        cwd: String,
        base_branch: String,
        head_branch: String,
        title: String,
        body: String,
        draft: bool,
    },
    EnablePullRequestAutoMerge {
        cwd: String,
        head_branch: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct GitIntegrationOutboxEntry {
    id: String,
    key: String,
    created_at: String,
    attempts: u32,
    next_attempt_unix_secs: i64,
    last_error: Option<String>,
    operation: GitIntegrationOperation,
}

fn integration_outbox_path(project_root: &str) -> Result<PathBuf> {
    Ok(repo_ao_root(project_root)?
        .join("sync")
        .join("outbox.jsonl"))
}

fn load_git_integration_outbox(project_root: &str) -> Result<Vec<GitIntegrationOutboxEntry>> {
    let path = integration_outbox_path(project_root)?;
    if !path.exists() {
        return Ok(Vec::new());
    }

    let content = fs::read_to_string(&path).with_context(|| {
        format!(
            "failed to read git integration outbox at {}",
            path.display()
        )
    })?;
    let mut entries = Vec::new();
    for line in content
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        if let Ok(entry) = serde_json::from_str::<GitIntegrationOutboxEntry>(line) {
            entries.push(entry);
        }
    }
    Ok(entries)
}

fn save_git_integration_outbox(
    project_root: &str,
    entries: &[GitIntegrationOutboxEntry],
) -> Result<()> {
    let path = integration_outbox_path(project_root)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    if entries.is_empty() {
        if path.exists() {
            fs::remove_file(path)?;
        }
        return Ok(());
    }

    let mut payload = String::new();
    for entry in entries {
        payload.push_str(&serde_json::to_string(entry)?);
        payload.push('\n');
    }

    let tmp_path = path.with_file_name(format!(
        "{}.{}.tmp",
        path.file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("outbox"),
        Uuid::new_v4()
    ));
    fs::write(&tmp_path, payload)?;
    fs::rename(&tmp_path, &path)?;
    Ok(())
}

fn git_integration_operation_key(operation: &GitIntegrationOperation) -> String {
    match operation {
        GitIntegrationOperation::PushBranch {
            cwd,
            remote,
            branch,
        } => format!("push-branch:{cwd}:{remote}:{branch}"),
        GitIntegrationOperation::PushRef {
            cwd,
            remote,
            source_ref,
            target_ref,
        } => format!("push-ref:{cwd}:{remote}:{source_ref}:{target_ref}"),
        GitIntegrationOperation::OpenPullRequest {
            cwd,
            base_branch,
            head_branch,
            ..
        } => format!("open-pr:{cwd}:{base_branch}:{head_branch}"),
        GitIntegrationOperation::EnablePullRequestAutoMerge { cwd, head_branch } => {
            format!("enable-pr-auto-merge:{cwd}:{head_branch}")
        }
    }
}

pub fn enqueue_git_integration_operation(
    project_root: &str,
    operation: GitIntegrationOperation,
) -> Result<()> {
    let mut entries = load_git_integration_outbox(project_root)?;
    let key = git_integration_operation_key(&operation);
    if entries.iter().any(|entry| entry.key == key) {
        return Ok(());
    }

    entries.push(GitIntegrationOutboxEntry {
        id: Uuid::new_v4().to_string(),
        key,
        created_at: Utc::now().to_rfc3339(),
        attempts: 0,
        next_attempt_unix_secs: Utc::now().timestamp(),
        last_error: None,
        operation,
    });
    save_git_integration_outbox(project_root, &entries)
}

fn execute_git_integration_operation(operation: &GitIntegrationOperation) -> Result<()> {
    match operation {
        GitIntegrationOperation::PushBranch {
            cwd,
            remote,
            branch,
        } => push_branch(cwd, remote, branch),
        GitIntegrationOperation::PushRef {
            cwd,
            remote,
            source_ref,
            target_ref,
        } => push_ref(cwd, remote, source_ref, target_ref),
        GitIntegrationOperation::OpenPullRequest {
            cwd,
            base_branch,
            head_branch,
            title,
            body,
            draft,
        } => create_pull_request(cwd, base_branch, head_branch, title, body, *draft),
        GitIntegrationOperation::EnablePullRequestAutoMerge { cwd, head_branch } => {
            enable_pull_request_auto_merge(cwd, head_branch)
        }
    }
}

fn git_integration_retry_delay_secs(attempts: u32) -> i64 {
    let shift = attempts.min(8);
    (1_i64 << shift).clamp(2, 300)
}

pub fn flush_git_integration_outbox(project_root: &str) -> Result<()> {
    let entries = load_git_integration_outbox(project_root)?;
    if entries.is_empty() {
        return Ok(());
    }

    let now = Utc::now().timestamp();
    let mut remaining = Vec::new();
    for mut entry in entries {
        if entry.next_attempt_unix_secs > now {
            remaining.push(entry);
            continue;
        }

        match execute_git_integration_operation(&entry.operation) {
            Ok(()) => {}
            Err(error) => {
                entry.attempts = entry.attempts.saturating_add(1);
                entry.next_attempt_unix_secs =
                    now.saturating_add(git_integration_retry_delay_secs(entry.attempts));
                entry.last_error = Some(error.to_string());
                remaining.push(entry);
            }
        }
    }

    save_git_integration_outbox(project_root, &remaining)
}
