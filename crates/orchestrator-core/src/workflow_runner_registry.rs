use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

const WORKFLOW_RUNNER_DIR: &str = "workflow-runner-pids";
const PID_FILE_EXT: &str = "pid";

fn workflow_runner_pid_dir(project_root: &Path) -> PathBuf {
    project_root
        .join(".ao")
        .join("state")
        .join(WORKFLOW_RUNNER_DIR)
}

fn workflow_runner_pid_path(project_root: &Path, workflow_id: &str) -> PathBuf {
    workflow_runner_pid_dir(project_root).join(format!("{workflow_id}.{PID_FILE_EXT}"))
}

pub fn register_workflow_runner_pid(
    project_root: &Path,
    workflow_id: &str,
    pid: u32,
) -> Result<()> {
    let dir = workflow_runner_pid_dir(project_root);
    fs::create_dir_all(&dir).with_context(|| {
        format!(
            "failed to create workflow runner registry directory at {}",
            dir.display()
        )
    })?;
    fs::write(
        workflow_runner_pid_path(project_root, workflow_id),
        pid.to_string(),
    )
    .with_context(|| {
        format!("failed to record active workflow runner pid for workflow '{workflow_id}'")
    })?;
    Ok(())
}

pub fn unregister_workflow_runner_pid(project_root: &Path, workflow_id: &str) -> Result<()> {
    let path = workflow_runner_pid_path(project_root, workflow_id);
    match fs::remove_file(&path) {
        Ok(_) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err).with_context(|| {
            format!(
                "failed to remove workflow runner registry entry at {}",
                path.display()
            )
        }),
    }
}

pub fn active_workflow_runner_ids(project_root: &Path) -> Result<HashSet<String>> {
    let dir = workflow_runner_pid_dir(project_root);
    if !dir.exists() {
        return Ok(HashSet::new());
    }

    let mut active = HashSet::new();
    for entry in fs::read_dir(&dir).with_context(|| {
        format!(
            "failed to read workflow runner registry directory at {}",
            dir.display()
        )
    })? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some(PID_FILE_EXT) {
            continue;
        }

        let Some(workflow_id) = path
            .file_stem()
            .and_then(|value| value.to_str())
            .map(ToOwned::to_owned)
        else {
            continue;
        };

        let pid = fs::read_to_string(&path)
            .ok()
            .and_then(|value| value.trim().parse::<u32>().ok());

        if pid.is_some_and(protocol::is_process_alive) {
            active.insert(workflow_id);
        } else {
            let _ = fs::remove_file(&path);
        }
    }

    Ok(active)
}
