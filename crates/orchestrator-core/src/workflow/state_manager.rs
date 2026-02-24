use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use chrono::Utc;

use crate::types::{CheckpointReason, OrchestratorWorkflow, WorkflowCheckpoint};

#[derive(Clone)]
pub struct WorkflowStateManager {
    project_root: PathBuf,
}

impl WorkflowStateManager {
    pub fn new(project_root: impl Into<PathBuf>) -> Self {
        Self {
            project_root: project_root.into(),
        }
    }

    pub fn save(&self, workflow: &OrchestratorWorkflow) -> Result<()> {
        let path = self.workflow_path(&workflow.id);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        let json = serde_json::to_string_pretty(workflow)?;
        write_atomic(&path, json)
    }

    pub fn load(&self, workflow_id: &str) -> Result<OrchestratorWorkflow> {
        let path = self.workflow_path(workflow_id);
        if !path.exists() {
            return Err(anyhow!("workflow not found: {workflow_id}"));
        }

        let content = fs::read_to_string(path)?;
        Ok(serde_json::from_str(&content)?)
    }

    pub fn list(&self) -> Result<Vec<OrchestratorWorkflow>> {
        let dir = self.workflows_dir();
        if !dir.exists() {
            return Ok(Vec::new());
        }

        let mut workflows = Vec::new();
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }

            let content = fs::read_to_string(path)?;
            if let Ok(workflow) = serde_json::from_str::<OrchestratorWorkflow>(&content) {
                workflows.push(workflow);
            }
        }

        Ok(workflows)
    }

    pub fn delete(&self, workflow_id: &str) -> Result<()> {
        let path = self.workflow_path(workflow_id);
        if path.exists() {
            fs::remove_file(path)?;
        }

        let checkpoints_dir = self.checkpoints_dir(workflow_id);
        if checkpoints_dir.exists() {
            fs::remove_dir_all(checkpoints_dir)?;
        }

        Ok(())
    }

    pub fn save_checkpoint(
        &self,
        workflow: &OrchestratorWorkflow,
        reason: CheckpointReason,
    ) -> Result<OrchestratorWorkflow> {
        let mut workflow = workflow.clone();
        workflow.checkpoint_metadata.checkpoint_count += 1;

        let checkpoint = WorkflowCheckpoint {
            number: workflow.checkpoint_metadata.checkpoint_count,
            timestamp: Utc::now(),
            reason,
            machine_state: workflow.machine_state,
            status: workflow.status,
        };
        workflow
            .checkpoint_metadata
            .checkpoints
            .push(checkpoint.clone());

        let checkpoint_path = self.checkpoint_path(&workflow.id, checkpoint.number);
        if let Some(parent) = checkpoint_path.parent() {
            fs::create_dir_all(parent)?;
        }

        let json = serde_json::to_string_pretty(&workflow)?;
        write_atomic(&checkpoint_path, json)?;
        self.save(&workflow)?;

        Ok(workflow)
    }

    pub fn list_checkpoints(&self, workflow_id: &str) -> Result<Vec<usize>> {
        let checkpoint_dir = self.checkpoints_dir(workflow_id);
        if !checkpoint_dir.exists() {
            return Ok(Vec::new());
        }

        let mut checkpoints = Vec::new();
        for entry in fs::read_dir(checkpoint_dir)? {
            let entry = entry?;
            let path = entry.path();
            if let Some(name) = path.file_stem().and_then(|n| n.to_str()) {
                if let Some(num_str) = name.strip_prefix("checkpoint-") {
                    if let Ok(num) = num_str.parse::<usize>() {
                        checkpoints.push(num);
                    }
                }
            }
        }

        checkpoints.sort();
        Ok(checkpoints)
    }

    pub fn load_checkpoint(
        &self,
        workflow_id: &str,
        checkpoint_num: usize,
    ) -> Result<OrchestratorWorkflow> {
        let path = self.checkpoint_path(workflow_id, checkpoint_num);
        if !path.exists() {
            return Err(anyhow!(
                "checkpoint not found: {} #{checkpoint_num}",
                workflow_id
            ));
        }

        let content = fs::read_to_string(path)?;
        Ok(serde_json::from_str(&content)?)
    }

    fn workflows_dir(&self) -> PathBuf {
        self.project_root.join(".ao").join("workflow-state")
    }

    fn workflow_path(&self, workflow_id: &str) -> PathBuf {
        self.workflows_dir().join(format!("{workflow_id}.json"))
    }

    fn checkpoints_dir(&self, workflow_id: &str) -> PathBuf {
        self.workflows_dir().join("checkpoints").join(workflow_id)
    }

    fn checkpoint_path(&self, workflow_id: &str, checkpoint_num: usize) -> PathBuf {
        self.checkpoints_dir(workflow_id)
            .join(format!("checkpoint-{checkpoint_num:04}.json"))
    }
}

fn write_atomic(path: &Path, contents: String) -> Result<()> {
    let temp_path = path.with_extension("tmp");
    {
        let mut file = fs::File::create(&temp_path)?;
        file.write_all(contents.as_bytes())?;
        file.sync_all()?;
    }
    fs::rename(&temp_path, path).with_context(|| {
        format!(
            "failed to rename {} to {}",
            temp_path.display(),
            path.display()
        )
    })?;
    Ok(())
}
