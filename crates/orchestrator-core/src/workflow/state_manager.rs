use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use chrono::{Duration, Utc};
use serde::{Deserialize, Serialize};

use crate::types::{CheckpointReason, OrchestratorWorkflow, WorkflowCheckpoint};

pub const DEFAULT_CHECKPOINT_RETENTION_KEEP_LAST_PER_PHASE: usize = 10;
const UNKNOWN_CHECKPOINT_PHASE_BUCKET: &str = "unknown";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowCheckpointPruneResult {
    pub workflow_id: String,
    pub dry_run: bool,
    pub keep_last_per_phase: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_age_hours: Option<u64>,
    pub checkpoint_count_before: usize,
    pub checkpoint_count_after: usize,
    pub pruned_count: usize,
    #[serde(default)]
    pub pruned_checkpoint_numbers: Vec<usize>,
    #[serde(default)]
    pub pruned_by_phase: BTreeMap<String, usize>,
}

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
            phase_id: checkpoint_phase_id(&workflow),
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

    pub fn prune_checkpoints(
        &self,
        workflow_id: &str,
        keep_last_per_phase: usize,
        max_age_hours: Option<u64>,
        dry_run: bool,
    ) -> Result<WorkflowCheckpointPruneResult> {
        if keep_last_per_phase == 0 {
            return Err(anyhow!("keep_last_per_phase must be greater than zero"));
        }

        let mut workflow = self.load(workflow_id)?;
        let checkpoints = workflow.checkpoint_metadata.checkpoints.clone();
        let checkpoint_count_before = checkpoints.len();

        if checkpoints.is_empty() {
            return Ok(WorkflowCheckpointPruneResult {
                workflow_id: workflow_id.to_string(),
                dry_run,
                keep_last_per_phase,
                max_age_hours,
                checkpoint_count_before: 0,
                checkpoint_count_after: 0,
                pruned_count: 0,
                pruned_checkpoint_numbers: Vec::new(),
                pruned_by_phase: BTreeMap::new(),
            });
        }

        let mut checkpoint_numbers_to_prune = BTreeSet::new();
        let mut phase_by_checkpoint_number = BTreeMap::<usize, String>::new();
        let mut checkpoints_by_phase = BTreeMap::<String, Vec<WorkflowCheckpoint>>::new();
        for checkpoint in &checkpoints {
            let phase_bucket = self.resolve_checkpoint_phase_bucket(
                workflow_id,
                checkpoint,
                &mut phase_by_checkpoint_number,
            );
            phase_by_checkpoint_number.insert(checkpoint.number, phase_bucket.clone());
            checkpoints_by_phase
                .entry(phase_bucket)
                .or_default()
                .push(checkpoint.clone());
        }

        for phase_checkpoints in checkpoints_by_phase.values_mut() {
            phase_checkpoints.sort_by_key(|checkpoint| checkpoint.number);
            if phase_checkpoints.len() > keep_last_per_phase {
                for checkpoint in phase_checkpoints
                    .iter()
                    .take(phase_checkpoints.len() - keep_last_per_phase)
                {
                    checkpoint_numbers_to_prune.insert(checkpoint.number);
                }
            }
        }

        if let Some(hours) = max_age_hours {
            let hours_i64 =
                i64::try_from(hours).context("max_age_hours exceeds supported range")?;
            let cutoff = Utc::now() - Duration::hours(hours_i64);
            for checkpoint in &checkpoints {
                if checkpoint.timestamp < cutoff {
                    checkpoint_numbers_to_prune.insert(checkpoint.number);
                }
            }
        }

        if checkpoint_numbers_to_prune.is_empty() {
            return Ok(WorkflowCheckpointPruneResult {
                workflow_id: workflow_id.to_string(),
                dry_run,
                keep_last_per_phase,
                max_age_hours,
                checkpoint_count_before,
                checkpoint_count_after: checkpoint_count_before,
                pruned_count: 0,
                pruned_checkpoint_numbers: Vec::new(),
                pruned_by_phase: BTreeMap::new(),
            });
        }

        let mut pruned_checkpoint_numbers = Vec::new();
        let mut pruned_by_phase = BTreeMap::<String, usize>::new();
        let retained_checkpoints: Vec<WorkflowCheckpoint> = checkpoints
            .into_iter()
            .filter(|checkpoint| {
                if checkpoint_numbers_to_prune.contains(&checkpoint.number) {
                    pruned_checkpoint_numbers.push(checkpoint.number);
                    let phase = phase_by_checkpoint_number
                        .get(&checkpoint.number)
                        .cloned()
                        .unwrap_or_else(|| UNKNOWN_CHECKPOINT_PHASE_BUCKET.to_string());
                    *pruned_by_phase.entry(phase).or_insert(0) += 1;
                    false
                } else {
                    true
                }
            })
            .collect();

        let checkpoint_count_after = retained_checkpoints.len();

        if !dry_run {
            workflow.checkpoint_metadata.checkpoints = retained_checkpoints;
            self.save(&workflow)?;

            for checkpoint_num in &pruned_checkpoint_numbers {
                let path = self.checkpoint_path(workflow_id, *checkpoint_num);
                if path.exists() {
                    fs::remove_file(&path).with_context(|| {
                        format!("failed to remove checkpoint file {}", path.display())
                    })?;
                }
            }
        }

        Ok(WorkflowCheckpointPruneResult {
            workflow_id: workflow_id.to_string(),
            dry_run,
            keep_last_per_phase,
            max_age_hours,
            checkpoint_count_before,
            checkpoint_count_after,
            pruned_count: pruned_checkpoint_numbers.len(),
            pruned_checkpoint_numbers,
            pruned_by_phase,
        })
    }

    fn resolve_checkpoint_phase_bucket(
        &self,
        workflow_id: &str,
        checkpoint: &WorkflowCheckpoint,
        phase_cache: &mut BTreeMap<usize, String>,
    ) -> String {
        if let Some(phase_id) = &checkpoint.phase_id {
            return phase_id.clone();
        }

        if let Some(cached) = phase_cache.get(&checkpoint.number) {
            return cached.clone();
        }

        let phase = self
            .load_checkpoint(workflow_id, checkpoint.number)
            .ok()
            .and_then(|snapshot| checkpoint_phase_id(&snapshot))
            .unwrap_or_else(|| UNKNOWN_CHECKPOINT_PHASE_BUCKET.to_string());
        phase_cache.insert(checkpoint.number, phase.clone());
        phase
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

fn checkpoint_phase_id(workflow: &OrchestratorWorkflow) -> Option<String> {
    workflow.current_phase.clone().or_else(|| {
        workflow
            .phases
            .get(workflow.current_phase_index)
            .map(|phase| phase.phase_id.clone())
    })
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
