use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use anyhow::{anyhow, Context, Result};
use chrono::{Duration, Utc};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

use crate::types::{CheckpointReason, OrchestratorWorkflow, WorkflowCheckpoint};

pub const DEFAULT_CHECKPOINT_RETENTION_KEEP_LAST_PER_PHASE: usize = 3;

fn compress_json(json: &str) -> Vec<u8> {
    zstd::encode_all(json.as_bytes(), 3).unwrap_or_else(|_| json.as_bytes().to_vec())
}

fn decompress_json(data: &[u8]) -> Result<String> {
    if data.first() == Some(&b'{') || data.first() == Some(&b'[') {
        return Ok(String::from_utf8_lossy(data).into_owned());
    }
    let decoded = zstd::decode_all(data).context("failed to decompress zstd blob")?;
    Ok(String::from_utf8(decoded).context("decompressed data is not valid UTF-8")?)
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CleanupResult {
    pub deleted: usize,
}

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
        Self { project_root: project_root.into() }
    }

    pub fn save(&self, workflow: &OrchestratorWorkflow) -> Result<()> {
        let conn = self.open_db()?;
        let data = compress_json(&serde_json::to_string(workflow)?);
        conn.execute(
            "INSERT OR REPLACE INTO workflows (id, status, json) VALUES (?1, ?2, ?3)",
            params![workflow.id, status_str(workflow.status), data],
        )?;
        Ok(())
    }

    pub fn load(&self, workflow_id: &str) -> Result<OrchestratorWorkflow> {
        let conn = self.open_db()?;
        let data: Vec<u8> = conn
            .query_row("SELECT json FROM workflows WHERE id = ?1", params![workflow_id], |row| row.get(0))
            .map_err(|_| anyhow!("workflow not found: {workflow_id}"))?;
        let json = decompress_json(&data)?;
        Ok(serde_json::from_str(&json)?)
    }

    pub fn list(&self) -> Result<Vec<OrchestratorWorkflow>> {
        let conn = self.open_db()?;
        let mut stmt = conn.prepare("SELECT json FROM workflows WHERE status IN ('running', 'paused')")?;
        let workflows = stmt
            .query_map([], |row| row.get::<_, Vec<u8>>(0))?
            .filter_map(|r| r.ok())
            .filter_map(|data| decompress_json(&data).ok())
            .filter_map(|json| serde_json::from_str::<OrchestratorWorkflow>(&json).ok())
            .collect();
        Ok(workflows)
    }

    pub fn list_all(&self) -> Result<Vec<OrchestratorWorkflow>> {
        let conn = self.open_db()?;
        let mut stmt = conn.prepare("SELECT json FROM workflows")?;
        let workflows = stmt
            .query_map([], |row| row.get::<_, Vec<u8>>(0))?
            .filter_map(|r| r.ok())
            .filter_map(|data| decompress_json(&data).ok())
            .filter_map(|json| serde_json::from_str::<OrchestratorWorkflow>(&json).ok())
            .collect();
        Ok(workflows)
    }

    pub fn cleanup_terminal_workflows(&self, max_age_hours: u64) -> Result<CleanupResult> {
        let cutoff = Utc::now() - Duration::hours(max_age_hours as i64);
        let cutoff_str = cutoff.to_rfc3339();

        let conn = self.open_db()?;
        let deleted = conn.execute(
            "DELETE FROM workflows WHERE status NOT IN ('running', 'paused') AND json_extract(json, '$.completed_at') < ?1",
            params![cutoff_str],
        )? + conn.execute(
            "DELETE FROM workflows WHERE status NOT IN ('running', 'paused') AND json_extract(json, '$.completed_at') IS NULL AND json_extract(json, '$.started_at') < ?1",
            params![cutoff_str],
        )?;

        conn.execute("DELETE FROM checkpoints WHERE workflow_id NOT IN (SELECT id FROM workflows)", [])?;

        Ok(CleanupResult { deleted })
    }

    pub fn delete(&self, workflow_id: &str) -> Result<()> {
        let conn = self.open_db()?;
        conn.execute("DELETE FROM workflows WHERE id = ?1", params![workflow_id])?;
        conn.execute("DELETE FROM checkpoints WHERE workflow_id = ?1", params![workflow_id])?;
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
        workflow.checkpoint_metadata.checkpoints.push(checkpoint.clone());

        let conn = self.open_db()?;
        let snapshot_data = compress_json(&serde_json::to_string(&workflow)?);
        conn.execute(
            "INSERT OR REPLACE INTO checkpoints (workflow_id, number, timestamp, reason, phase_id, machine_state, status, snapshot_json) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                workflow.id,
                checkpoint.number as i64,
                checkpoint.timestamp.to_rfc3339(),
                format!("{:?}", checkpoint.reason).to_ascii_lowercase(),
                checkpoint.phase_id,
                format!("{:?}", checkpoint.machine_state).to_ascii_lowercase(),
                status_str(checkpoint.status),
                snapshot_data,
            ],
        )?;
        drop(conn);

        self.save(&workflow)?;

        if workflow.checkpoint_metadata.checkpoint_count.is_multiple_of(5) {
            let _ = self.prune_checkpoints(&workflow.id, DEFAULT_CHECKPOINT_RETENTION_KEEP_LAST_PER_PHASE, None, false);
        }

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
            let phase_bucket =
                self.resolve_checkpoint_phase_bucket(workflow_id, checkpoint, &mut phase_by_checkpoint_number);
            phase_by_checkpoint_number.insert(checkpoint.number, phase_bucket.clone());
            checkpoints_by_phase.entry(phase_bucket).or_default().push(checkpoint.clone());
        }

        for phase_checkpoints in checkpoints_by_phase.values_mut() {
            phase_checkpoints.sort_by_key(|checkpoint| checkpoint.number);
            if phase_checkpoints.len() > keep_last_per_phase {
                for checkpoint in phase_checkpoints.iter().take(phase_checkpoints.len() - keep_last_per_phase) {
                    checkpoint_numbers_to_prune.insert(checkpoint.number);
                }
            }
        }

        if let Some(hours) = max_age_hours {
            let hours_i64 = i64::try_from(hours).context("max_age_hours exceeds supported range")?;
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

            let conn = self.open_db()?;
            for checkpoint_num in &pruned_checkpoint_numbers {
                conn.execute(
                    "DELETE FROM checkpoints WHERE workflow_id = ?1 AND number = ?2",
                    params![workflow_id, *checkpoint_num as i64],
                )?;
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
        let conn = self.open_db()?;
        let mut stmt = conn.prepare("SELECT number FROM checkpoints WHERE workflow_id = ?1 ORDER BY number")?;
        let numbers: Vec<usize> = stmt
            .query_map(params![workflow_id], |row| row.get::<_, i64>(0))?
            .filter_map(|r| r.ok())
            .map(|n| n as usize)
            .collect();
        Ok(numbers)
    }

    pub fn load_checkpoint(&self, workflow_id: &str, checkpoint_num: usize) -> Result<OrchestratorWorkflow> {
        let conn = self.open_db()?;
        let data: Vec<u8> = conn
            .query_row(
                "SELECT snapshot_json FROM checkpoints WHERE workflow_id = ?1 AND number = ?2",
                params![workflow_id, checkpoint_num as i64],
                |row| row.get(0),
            )
            .map_err(|_| anyhow!("checkpoint not found: {} #{checkpoint_num}", workflow_id))?;
        let json = decompress_json(&data)?;
        Ok(serde_json::from_str(&json)?)
    }

    fn open_db(&self) -> Result<Connection> {
        open_project_db(&self.project_root)
    }
}

pub fn db_path_for_project(project_root: &std::path::Path) -> PathBuf {
    protocol::scoped_state_root(project_root).expect("scoped_state_root requires a home directory").join("workflow.db")
}

pub fn open_project_db(project_root: &std::path::Path) -> Result<Connection> {
    let path = db_path_for_project(project_root);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let conn = Connection::open(&path).with_context(|| format!("failed to open db at {}", path.display()))?;
    conn.execute_batch(
        "PRAGMA journal_mode=WAL;
         PRAGMA synchronous=NORMAL;
         PRAGMA busy_timeout=5000;",
    )
    .with_context(|| "failed to set SQLite pragmas")?;

    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS workflows (
            id     TEXT PRIMARY KEY,
            status TEXT NOT NULL,
            json   TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_wf_status ON workflows(status);
        CREATE TABLE IF NOT EXISTS checkpoints (
            workflow_id   TEXT NOT NULL,
            number        INTEGER NOT NULL,
            timestamp     TEXT NOT NULL,
            reason        TEXT NOT NULL,
            phase_id      TEXT,
            machine_state TEXT,
            status        TEXT,
            snapshot_json TEXT,
            PRIMARY KEY (workflow_id, number)
        );
        CREATE INDEX IF NOT EXISTS idx_cp_workflow ON checkpoints(workflow_id);
        CREATE TABLE IF NOT EXISTS tasks (
            id     TEXT PRIMARY KEY,
            status TEXT NOT NULL,
            json   TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_task_status ON tasks(status);
        CREATE TABLE IF NOT EXISTS requirements (
            id     TEXT PRIMARY KEY,
            status TEXT NOT NULL,
            json   TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_req_status ON requirements(status);",
    )
    .with_context(|| "failed to create tables")?;

    maybe_migrate_workflow_json(project_root, &conn);

    Ok(conn)
}

fn maybe_migrate_workflow_json(project_root: &std::path::Path, conn: &Connection) {
    let legacy_dir = protocol::scoped_state_root(project_root)
        .expect("scoped_state_root requires a home directory")
        .join("workflow-state");
    let marker = db_path_for_project(project_root).with_file_name("workflow-migrated.marker");

    if marker.exists() {
        return;
    }
    if !legacy_dir.exists() {
        return;
    }

    let has_rows: bool =
        conn.query_row("SELECT EXISTS(SELECT 1 FROM workflows LIMIT 1)", [], |row| row.get(0)).unwrap_or(false);
    if has_rows {
        let _ = std::fs::File::create(&marker);
        return;
    }

    eprintln!("[ao] migrating workflow JSON to SQLite...");

    let mut migrated = 0usize;
    let entries: Vec<_> = std::fs::read_dir(&legacy_dir).into_iter().flatten().filter_map(|e| e.ok()).collect();

    for entry in &entries {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        if path.file_name().and_then(|n| n.to_str()) == Some("_active_index.json") {
            continue;
        }

        let Ok(content) = std::fs::read_to_string(&path) else { continue };
        let Ok(workflow) = serde_json::from_str::<OrchestratorWorkflow>(&content) else { continue };

        let compact = serde_json::to_string(&workflow).unwrap_or_else(|_| content.clone());
        let data = compress_json(&compact);
        let _ = conn.execute(
            "INSERT OR IGNORE INTO workflows (id, status, json) VALUES (?1, ?2, ?3)",
            params![workflow.id, status_str(workflow.status), data],
        );
        migrated += 1;
    }

    eprintln!("[ao] migrated {} workflows to SQLite", migrated);
    let _ = std::fs::File::create(&marker);
}

fn checkpoint_phase_id(workflow: &OrchestratorWorkflow) -> Option<String> {
    workflow
        .current_phase
        .clone()
        .or_else(|| workflow.phases.get(workflow.current_phase_index).map(|phase| phase.phase_id.clone()))
}

fn status_str(status: crate::types::WorkflowStatus) -> &'static str {
    use crate::types::WorkflowStatus;
    match status {
        WorkflowStatus::Pending => "pending",
        WorkflowStatus::Running => "running",
        WorkflowStatus::Paused => "paused",
        WorkflowStatus::Completed => "completed",
        WorkflowStatus::Failed => "failed",
        WorkflowStatus::Escalated => "escalated",
        WorkflowStatus::Cancelled => "cancelled",
    }
}

pub fn save_task(project_root: &std::path::Path, task: &crate::types::OrchestratorTask) -> Result<()> {
    let conn = open_project_db(project_root)?;
    let data = compress_json(&serde_json::to_string(task)?);
    conn.execute(
        "INSERT OR REPLACE INTO tasks (id, status, json) VALUES (?1, ?2, ?3)",
        params![task.id, task.status.to_string(), data],
    )?;
    Ok(())
}

pub fn load_task(project_root: &std::path::Path, task_id: &str) -> Result<crate::types::OrchestratorTask> {
    let conn = open_project_db(project_root)?;
    let data: Vec<u8> = conn
        .query_row("SELECT json FROM tasks WHERE id = ?1", params![task_id], |row| row.get(0))
        .map_err(|_| anyhow!("task not found: {task_id}"))?;
    let json = decompress_json(&data)?;
    Ok(serde_json::from_str(&json)?)
}

pub fn load_all_tasks(
    project_root: &std::path::Path,
) -> Result<std::collections::HashMap<String, crate::types::OrchestratorTask>> {
    let conn = open_project_db(project_root)?;
    let mut stmt = conn.prepare("SELECT json FROM tasks")?;
    let tasks: std::collections::HashMap<String, crate::types::OrchestratorTask> = stmt
        .query_map([], |row| row.get::<_, Vec<u8>>(0))?
        .filter_map(|r| r.ok())
        .filter_map(|data| decompress_json(&data).ok())
        .filter_map(|json| serde_json::from_str::<crate::types::OrchestratorTask>(&json).ok())
        .map(|t| (t.id.clone(), t))
        .collect();
    Ok(tasks)
}

pub fn delete_task(project_root: &std::path::Path, task_id: &str) -> Result<()> {
    let conn = open_project_db(project_root)?;
    conn.execute("DELETE FROM tasks WHERE id = ?1", params![task_id])?;
    Ok(())
}

pub fn save_requirement(project_root: &std::path::Path, req: &crate::types::RequirementItem) -> Result<()> {
    let conn = open_project_db(project_root)?;
    let data = compress_json(&serde_json::to_string(req)?);
    conn.execute(
        "INSERT OR REPLACE INTO requirements (id, status, json) VALUES (?1, ?2, ?3)",
        params![req.id, req.status.to_string(), data],
    )?;
    Ok(())
}

pub fn load_all_requirements(
    project_root: &std::path::Path,
) -> Result<std::collections::HashMap<String, crate::types::RequirementItem>> {
    let conn = open_project_db(project_root)?;
    let mut stmt = conn.prepare("SELECT json FROM requirements")?;
    let reqs: std::collections::HashMap<String, crate::types::RequirementItem> = stmt
        .query_map([], |row| row.get::<_, Vec<u8>>(0))?
        .filter_map(|r| r.ok())
        .filter_map(|data| decompress_json(&data).ok())
        .filter_map(|json| serde_json::from_str::<crate::types::RequirementItem>(&json).ok())
        .map(|r| (r.id.clone(), r))
        .collect();
    Ok(reqs)
}

pub fn delete_requirement(project_root: &std::path::Path, req_id: &str) -> Result<()> {
    let conn = open_project_db(project_root)?;
    conn.execute("DELETE FROM requirements WHERE id = ?1", params![req_id])?;
    Ok(())
}

pub fn migrate_tasks_and_requirements_from_core_state(
    project_root: &std::path::Path,
    tasks: &std::collections::HashMap<String, crate::types::OrchestratorTask>,
    requirements: &std::collections::HashMap<String, crate::types::RequirementItem>,
) {
    let marker = db_path_for_project(project_root).with_file_name("tasks-migrated.marker");
    if marker.exists() {
        return;
    }

    let conn = match open_project_db(project_root) {
        Ok(c) => c,
        Err(_) => return,
    };

    let has_tasks: bool =
        conn.query_row("SELECT EXISTS(SELECT 1 FROM tasks LIMIT 1)", [], |row| row.get(0)).unwrap_or(false);
    if has_tasks {
        let _ = std::fs::File::create(&marker);
        return;
    }

    let scoped_root = match protocol::scoped_state_root(project_root) {
        Some(r) => r,
        None => {
            let _ = std::fs::File::create(&marker);
            return;
        }
    };

    let mut task_count = 0usize;

    for task in tasks.values() {
        if let Ok(json) = serde_json::to_string(task) {
            let data = compress_json(&json);
            let _ = conn.execute(
                "INSERT OR IGNORE INTO tasks (id, status, json) VALUES (?1, ?2, ?3)",
                params![task.id, task.status.to_string(), data],
            );
            task_count += 1;
        }
    }

    let tasks_dir = scoped_root.join("tasks");
    if tasks_dir.exists() {
        if let Ok(entries) = std::fs::read_dir(&tasks_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("json") {
                    continue;
                }
                let Ok(content) = std::fs::read_to_string(&path) else { continue };
                let Ok(task) = serde_json::from_str::<crate::types::OrchestratorTask>(&content) else { continue };
                let compact = serde_json::to_string(&task).unwrap_or(content);
                let data = compress_json(&compact);
                let _ = conn.execute(
                    "INSERT OR IGNORE INTO tasks (id, status, json) VALUES (?1, ?2, ?3)",
                    params![task.id, task.status.to_string(), data],
                );
                task_count += 1;
            }
        }
    }

    let mut req_count = 0usize;

    for req in requirements.values() {
        if let Ok(json) = serde_json::to_string(req) {
            let data = compress_json(&json);
            let _ = conn.execute(
                "INSERT OR IGNORE INTO requirements (id, status, json) VALUES (?1, ?2, ?3)",
                params![req.id, req.status.to_string(), data],
            );
            req_count += 1;
        }
    }

    let reqs_dir = scoped_root.join("requirements");
    if reqs_dir.exists() {
        fn walk_json_files(dir: &std::path::Path, files: &mut Vec<std::path::PathBuf>) {
            if let Ok(entries) = std::fs::read_dir(dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.is_dir() {
                        walk_json_files(&path, files);
                    } else if path.extension().and_then(|e| e.to_str()) == Some("json") {
                        files.push(path);
                    }
                }
            }
        }
        let mut req_files = Vec::new();
        walk_json_files(&reqs_dir, &mut req_files);
        for path in req_files {
            if path.file_name().and_then(|n| n.to_str()) == Some("index.json") {
                continue;
            }
            let Ok(content) = std::fs::read_to_string(&path) else { continue };
            let Ok(req) = serde_json::from_str::<crate::types::RequirementItem>(&content) else { continue };
            let compact = serde_json::to_string(&req).unwrap_or(content);
            let data = compress_json(&compact);
            let _ = conn.execute(
                "INSERT OR IGNORE INTO requirements (id, status, json) VALUES (?1, ?2, ?3)",
                params![req.id, req.status.to_string(), data],
            );
            req_count += 1;
        }
    }

    if task_count > 0 || req_count > 0 {
        eprintln!("[ao] migrated {} tasks, {} requirements to SQLite", task_count, req_count);
    }
    let _ = std::fs::File::create(&marker);
}
