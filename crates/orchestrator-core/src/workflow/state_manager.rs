use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::PathBuf;

use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Duration, Utc};
use rusqlite::{params, params_from_iter, types::Value, Connection};
use serde::{Deserialize, Serialize};

use crate::types::{
    CheckpointReason, ListPageRequest, OrchestratorTask, OrchestratorWorkflow, RequirementFilter, RequirementItem,
    RequirementPriority, RequirementQuery, RequirementQuerySort, TaskFilter,
    TaskPriorityDistribution, TaskPriorityPolicyReport, TaskQuery, TaskQuerySort, TaskStatistics, TaskStatus,
    WorkflowCheckpoint, WorkflowPhaseStatus, WorkflowStatus,
};

pub const DEFAULT_CHECKPOINT_RETENTION_KEEP_LAST_PER_PHASE: usize = 3;
const TASK_SUMMARY_COLUMNS_MARKER_FILE: &str = "task-summary-columns-v2.marker";
const WORKFLOW_SUMMARY_COLUMNS_MARKER_FILE: &str = "workflow-summary-columns-v2.marker";
const REQUIREMENT_SUMMARY_COLUMNS_MARKER_FILE: &str = "requirement-summary-columns-v1.marker";

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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowActivitySummary {
    pub workflow_id: String,
    pub task_id: String,
    pub status: String,
    pub phase_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowFailureSummary {
    pub workflow_id: String,
    pub task_id: String,
    pub phase_id: String,
    pub failed_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowHistorySummary {
    pub workflow_id: String,
    pub task_id: String,
    pub status: String,
    pub started_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequirementLinkSummary {
    pub requirement_id: String,
    pub title: String,
    pub priority: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockedTaskSummary {
    pub task_id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StaleTaskSummary {
    pub task_id: String,
    pub title: String,
    pub updated_at: DateTime<Utc>,
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
        let summary = workflow_summary_fields(workflow);
        conn.execute(
            "INSERT OR REPLACE INTO workflows (
                id,
                status,
                task_id,
                phase_id,
                failed_at,
                failure_reason,
                started_at,
                completed_at,
                json
            )
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                workflow.id,
                status_str(workflow.status),
                summary.task_id,
                summary.phase_id,
                summary.failed_at,
                summary.failure_reason,
                summary.started_at,
                summary.completed_at,
                data
            ],
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

    pub fn query_ids(
        &self,
        page: ListPageRequest,
        status: Option<crate::types::WorkflowStatus>,
    ) -> Result<(Vec<String>, usize)> {
        let conn = self.open_db()?;

        let total: usize = match status {
            Some(status) => conn.query_row(
                "SELECT COUNT(*) FROM workflows WHERE status = ?1",
                params![status_str(status)],
                |row| row.get(0),
            )?,
            None => conn.query_row("SELECT COUNT(*) FROM workflows", [], |row| row.get(0))?,
        };

        let (start, end) = page.bounds(total);
        let limit = end.saturating_sub(start);
        if limit == 0 {
            return Ok((Vec::new(), total));
        }

        let sql_with_status = "SELECT id
             FROM workflows
             WHERE status = ?1
             ORDER BY started_at DESC, id ASC
             LIMIT ?2 OFFSET ?3";
        let sql_all = "SELECT id
             FROM workflows
             ORDER BY started_at DESC, id ASC
             LIMIT ?1 OFFSET ?2";

        let mut stmt = conn.prepare(match status {
            Some(_) => sql_with_status,
            None => sql_all,
        })?;
        let ids: Vec<String> = match status {
            Some(status) => stmt
                .query_map(params![status_str(status), limit as i64, start as i64], |row| row.get::<_, String>(0))?
                .filter_map(|row| row.ok())
                .collect(),
            None => stmt
                .query_map(params![limit as i64, start as i64], |row| row.get::<_, String>(0))?
                .filter_map(|row| row.ok())
                .collect(),
        };

        Ok((ids, total))
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

pub fn load_active_workflow_summaries(project_root: &std::path::Path) -> Result<Vec<WorkflowActivitySummary>> {
    let conn = open_project_db(project_root)?;
    let mut stmt = conn.prepare(
        "SELECT id, task_id, status, phase_id
         FROM workflows
         WHERE status IN ('running', 'paused')
         ORDER BY id ASC",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, Option<String>>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, Option<String>>(3)?,
        ))
    })?;

    let mut workflows = Vec::new();
    for row in rows {
        let (workflow_id, task_id, status, phase_id) = row?;
        workflows.push(WorkflowActivitySummary {
            workflow_id,
            task_id: task_id.unwrap_or_default(),
            status,
            phase_id: phase_id.unwrap_or_else(|| "unknown".to_string()),
        });
    }
    Ok(workflows)
}

pub fn load_recent_failed_workflow_summaries(
    project_root: &std::path::Path,
    limit: usize,
) -> Result<Vec<WorkflowFailureSummary>> {
    let conn = open_project_db(project_root)?;
    let limit = i64::try_from(limit).context("recent failed workflow limit overflow")?;
    let mut stmt = conn.prepare(
        "SELECT id, task_id, phase_id, failed_at, failure_reason
         FROM workflows
         WHERE status = 'failed'
           AND failed_at IS NOT NULL
         ORDER BY failed_at DESC, id ASC
         LIMIT ?1",
    )?;
    let rows = stmt.query_map([limit], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, Option<String>>(1)?,
            row.get::<_, Option<String>>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, Option<String>>(4)?,
        ))
    })?;

    let mut workflows = Vec::new();
    for row in rows {
        let (workflow_id, task_id, phase_id, failed_at, failure_reason) = row?;
        let failed_at = DateTime::parse_from_rfc3339(&failed_at)
            .with_context(|| format!("invalid workflow failed_at timestamp for {workflow_id}"))?
            .with_timezone(&Utc);
        workflows.push(WorkflowFailureSummary {
            workflow_id,
            task_id: task_id.unwrap_or_default(),
            phase_id: phase_id.unwrap_or_else(|| "unknown".to_string()),
            failed_at,
            failure_reason,
        });
    }
    Ok(workflows)
}

pub fn load_workflow_history_summaries(project_root: &std::path::Path) -> Result<Vec<WorkflowHistorySummary>> {
    let conn = open_project_db(project_root)?;
    let mut stmt = conn.prepare(
        "SELECT id, task_id, status, started_at, completed_at
         FROM workflows
         WHERE started_at IS NOT NULL
         ORDER BY started_at DESC, id ASC",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, Option<String>>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, Option<String>>(4)?,
        ))
    })?;

    let mut workflows = Vec::new();
    for row in rows {
        let (workflow_id, task_id, status, started_at, completed_at) = row?;
        workflows.push(WorkflowHistorySummary {
            workflow_id,
            task_id: task_id.unwrap_or_default(),
            status,
            started_at: parse_rfc3339(&started_at, "workflows.started_at")?,
            completed_at: parse_optional_rfc3339(completed_at, "workflows.completed_at")?,
        });
    }
    Ok(workflows)
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
            task_id TEXT,
            phase_id TEXT,
            failed_at TEXT,
            failure_reason TEXT,
            started_at TEXT,
            completed_at TEXT,
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
            id           TEXT PRIMARY KEY,
            status       TEXT NOT NULL,
            title        TEXT,
            priority     TEXT,
            task_type    TEXT,
            updated_at   TEXT,
            completed_at TEXT,
            blocked_reason TEXT,
            blocked_at   TEXT,
            json         TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_task_status ON tasks(status);
        CREATE TABLE IF NOT EXISTS requirements (
            id               TEXT PRIMARY KEY,
            status           TEXT NOT NULL,
            priority         TEXT,
            category         TEXT,
            requirement_type TEXT,
            updated_at       TEXT,
            json             TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_req_status ON requirements(status);",
    )
    .with_context(|| "failed to create tables")?;

    ensure_workflow_summary_columns(project_root, &conn).context("failed to ensure workflow summary columns")?;
    ensure_task_summary_columns(project_root, &conn).context("failed to ensure task summary columns")?;
    ensure_requirement_summary_columns(project_root, &conn).context("failed to ensure requirement summary columns")?;
    maybe_migrate_workflow_json(project_root, &conn);

    Ok(conn)
}

fn ensure_workflow_summary_columns(project_root: &std::path::Path, conn: &Connection) -> Result<()> {
    let columns = workflow_table_columns(conn)?;

    if !columns.contains("task_id") {
        conn.execute("ALTER TABLE workflows ADD COLUMN task_id TEXT", [])
            .context("failed to add workflows.task_id column")?;
    }
    if !columns.contains("phase_id") {
        conn.execute("ALTER TABLE workflows ADD COLUMN phase_id TEXT", [])
            .context("failed to add workflows.phase_id column")?;
    }
    if !columns.contains("failed_at") {
        conn.execute("ALTER TABLE workflows ADD COLUMN failed_at TEXT", [])
            .context("failed to add workflows.failed_at column")?;
    }
    if !columns.contains("failure_reason") {
        conn.execute("ALTER TABLE workflows ADD COLUMN failure_reason TEXT", [])
            .context("failed to add workflows.failure_reason column")?;
    }
    if !columns.contains("started_at") {
        conn.execute("ALTER TABLE workflows ADD COLUMN started_at TEXT", [])
            .context("failed to add workflows.started_at column")?;
    }
    if !columns.contains("completed_at") {
        conn.execute("ALTER TABLE workflows ADD COLUMN completed_at TEXT", [])
            .context("failed to add workflows.completed_at column")?;
    }

    conn.execute("CREATE INDEX IF NOT EXISTS idx_wf_failed_at ON workflows(status, failed_at)", [])
        .context("failed to create idx_wf_failed_at")?;
    conn.execute("CREATE INDEX IF NOT EXISTS idx_wf_started_at ON workflows(started_at)", [])
        .context("failed to create idx_wf_started_at")?;

    maybe_backfill_workflow_summary_columns(project_root, conn)
}

fn workflow_table_columns(conn: &Connection) -> Result<BTreeSet<String>> {
    let mut stmt = conn.prepare("PRAGMA table_info(workflows)")?;
    let columns = stmt.query_map([], |row| row.get::<_, String>(1))?.filter_map(|row| row.ok()).collect();
    Ok(columns)
}

fn maybe_backfill_workflow_summary_columns(project_root: &std::path::Path, conn: &Connection) -> Result<()> {
    let marker = db_path_for_project(project_root).with_file_name(WORKFLOW_SUMMARY_COLUMNS_MARKER_FILE);
    if marker.exists() {
        return Ok(());
    }

    let has_rows: bool =
        conn.query_row("SELECT EXISTS(SELECT 1 FROM workflows LIMIT 1)", [], |row| row.get(0)).unwrap_or(false);
    if !has_rows {
        std::fs::File::create(marker).context("failed to create workflow summary marker")?;
        return Ok(());
    }

    let mut select = conn.prepare("SELECT id, json FROM workflows")?;
    let rows: Vec<(String, Vec<u8>)> = select
        .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(select);

    let mut update = conn.prepare(
        "UPDATE workflows
         SET task_id = ?2,
             phase_id = ?3,
             failed_at = ?4,
             failure_reason = ?5,
             started_at = ?6,
             completed_at = ?7
         WHERE id = ?1",
    )?;

    for (workflow_id, data) in rows {
        let json = decompress_json(&data)?;
        let workflow = serde_json::from_str::<OrchestratorWorkflow>(&json)?;
        let summary = workflow_summary_fields(&workflow);
        update.execute(params![
            workflow_id,
            summary.task_id,
            summary.phase_id,
            summary.failed_at,
            summary.failure_reason,
            summary.started_at,
            summary.completed_at
        ])?;
    }

    std::fs::File::create(marker).context("failed to create workflow summary marker")?;
    Ok(())
}

fn ensure_task_summary_columns(project_root: &std::path::Path, conn: &Connection) -> Result<()> {
    let columns = task_table_columns(conn)?;

    if !columns.contains("title") {
        conn.execute("ALTER TABLE tasks ADD COLUMN title TEXT", []).context("failed to add tasks.title column")?;
    }
    if !columns.contains("priority") {
        conn.execute("ALTER TABLE tasks ADD COLUMN priority TEXT", [])
            .context("failed to add tasks.priority column")?;
    }
    if !columns.contains("task_type") {
        conn.execute("ALTER TABLE tasks ADD COLUMN task_type TEXT", [])
            .context("failed to add tasks.task_type column")?;
    }
    if !columns.contains("updated_at") {
        conn.execute("ALTER TABLE tasks ADD COLUMN updated_at TEXT", [])
            .context("failed to add tasks.updated_at column")?;
    }
    if !columns.contains("completed_at") {
        conn.execute("ALTER TABLE tasks ADD COLUMN completed_at TEXT", [])
            .context("failed to add tasks.completed_at column")?;
    }
    if !columns.contains("blocked_reason") {
        conn.execute("ALTER TABLE tasks ADD COLUMN blocked_reason TEXT", [])
            .context("failed to add tasks.blocked_reason column")?;
    }
    if !columns.contains("blocked_at") {
        conn.execute("ALTER TABLE tasks ADD COLUMN blocked_at TEXT", [])
            .context("failed to add tasks.blocked_at column")?;
    }
    conn.execute("CREATE INDEX IF NOT EXISTS idx_task_completed_at ON tasks(completed_at)", [])
        .context("failed to create idx_task_completed_at")?;
    conn.execute("CREATE INDEX IF NOT EXISTS idx_task_status_updated_at ON tasks(status, updated_at)", [])
        .context("failed to create idx_task_status_updated_at")?;

    maybe_backfill_task_summary_columns(project_root, conn)
}

fn ensure_requirement_summary_columns(project_root: &std::path::Path, conn: &Connection) -> Result<()> {
    let columns = requirement_table_columns(conn)?;

    if !columns.contains("priority") {
        conn.execute("ALTER TABLE requirements ADD COLUMN priority TEXT", [])
            .context("failed to add requirements.priority column")?;
    }
    if !columns.contains("category") {
        conn.execute("ALTER TABLE requirements ADD COLUMN category TEXT", [])
            .context("failed to add requirements.category column")?;
    }
    if !columns.contains("requirement_type") {
        conn.execute("ALTER TABLE requirements ADD COLUMN requirement_type TEXT", [])
            .context("failed to add requirements.requirement_type column")?;
    }
    if !columns.contains("updated_at") {
        conn.execute("ALTER TABLE requirements ADD COLUMN updated_at TEXT", [])
            .context("failed to add requirements.updated_at column")?;
    }

    conn.execute("CREATE INDEX IF NOT EXISTS idx_req_priority ON requirements(priority)", [])
        .context("failed to create idx_req_priority")?;
    conn.execute("CREATE INDEX IF NOT EXISTS idx_req_updated_at ON requirements(updated_at)", [])
        .context("failed to create idx_req_updated_at")?;

    maybe_backfill_requirement_summary_columns(project_root, conn)
}

fn task_table_columns(conn: &Connection) -> Result<BTreeSet<String>> {
    let mut stmt = conn.prepare("PRAGMA table_info(tasks)")?;
    let columns = stmt.query_map([], |row| row.get::<_, String>(1))?.filter_map(|row| row.ok()).collect();
    Ok(columns)
}

fn requirement_table_columns(conn: &Connection) -> Result<BTreeSet<String>> {
    let mut stmt = conn.prepare("PRAGMA table_info(requirements)")?;
    let columns = stmt.query_map([], |row| row.get::<_, String>(1))?.filter_map(|row| row.ok()).collect();
    Ok(columns)
}

fn maybe_backfill_task_summary_columns(project_root: &std::path::Path, conn: &Connection) -> Result<()> {
    let marker = db_path_for_project(project_root).with_file_name(TASK_SUMMARY_COLUMNS_MARKER_FILE);
    if marker.exists() {
        return Ok(());
    }

    let has_rows: bool =
        conn.query_row("SELECT EXISTS(SELECT 1 FROM tasks LIMIT 1)", [], |row| row.get(0)).unwrap_or(false);
    if !has_rows {
        std::fs::File::create(marker).context("failed to create task summary marker")?;
        return Ok(());
    }

    let mut select = conn.prepare(
        "SELECT id, json
         FROM tasks
         WHERE title IS NULL
            OR priority IS NULL
            OR task_type IS NULL
            OR updated_at IS NULL
            OR (status = 'done' AND completed_at IS NULL)
            OR status = 'blocked'",
    )?;
    let rows: Vec<(String, Vec<u8>)> = select
        .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(select);

    let mut update = conn.prepare(
        "UPDATE tasks
         SET title = ?2,
             priority = ?3,
             task_type = ?4,
             updated_at = ?5,
             completed_at = ?6,
             blocked_reason = ?7,
             blocked_at = ?8
         WHERE id = ?1",
    )?;

    for row in rows {
        let (task_id, data) = row;
        let json = decompress_json(&data)?;
        let task = serde_json::from_str::<OrchestratorTask>(&json)?;
        let updated_at = task.metadata.updated_at.to_rfc3339();
        let completed_at = task.metadata.completed_at.as_ref().map(chrono::DateTime::to_rfc3339);
        let blocked_at = task.blocked_at.as_ref().map(chrono::DateTime::to_rfc3339);
        update.execute(params![
            task_id,
            task.title,
            task.priority.as_str(),
            task.task_type.as_str(),
            updated_at,
            completed_at,
            task.blocked_reason,
            blocked_at
        ])?;
    }

    std::fs::File::create(marker).context("failed to create task summary marker")?;
    Ok(())
}

fn maybe_backfill_requirement_summary_columns(project_root: &std::path::Path, conn: &Connection) -> Result<()> {
    let marker = db_path_for_project(project_root).with_file_name(REQUIREMENT_SUMMARY_COLUMNS_MARKER_FILE);
    let needs_backfill: bool = conn
        .query_row(
            "SELECT EXISTS(
                SELECT 1
                FROM requirements
                WHERE priority IS NULL
                   OR updated_at IS NULL
                   OR (status IS NOT NULL AND category IS NULL AND requirement_type IS NULL)
                LIMIT 1
            )",
            [],
            |row| row.get(0),
        )
        .context("failed to determine whether requirement summary backfill is needed")?;

    if marker.exists() && !needs_backfill {
        return Ok(());
    }
    if !needs_backfill {
        std::fs::File::create(marker).context("failed to create requirement summary marker")?;
        return Ok(());
    }

    let mut select = conn.prepare(
        "SELECT id, json
         FROM requirements
         WHERE priority IS NULL
            OR updated_at IS NULL
            OR category IS NULL
            OR requirement_type IS NULL",
    )?;
    let rows: Vec<(String, Vec<u8>)> = select
        .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(select);

    let mut update = conn.prepare(
        "UPDATE requirements
         SET priority = ?2, category = ?3, requirement_type = ?4, updated_at = ?5
         WHERE id = ?1",
    )?;

    for (requirement_id, data) in rows {
        let json = decompress_json(&data)?;
        let requirement = serde_json::from_str::<RequirementItem>(&json)?;
        update.execute(params![
            requirement_id,
            requirement_priority_str(requirement.priority),
            requirement.category,
            requirement.requirement_type.map(requirement_type_str),
            requirement.updated_at.to_rfc3339()
        ])?;
    }

    std::fs::File::create(marker).context("failed to create requirement summary marker")?;
    Ok(())
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
        let summary = workflow_summary_fields(&workflow);
        let _ = conn.execute(
            "INSERT OR IGNORE INTO workflows (
                id,
                status,
                task_id,
                phase_id,
                failed_at,
                failure_reason,
                started_at,
                completed_at,
                json
            )
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                workflow.id,
                status_str(workflow.status),
                summary.task_id,
                summary.phase_id,
                summary.failed_at,
                summary.failure_reason,
                summary.started_at,
                summary.completed_at,
                data
            ],
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

#[derive(Debug, Clone)]
struct WorkflowSummaryFields {
    task_id: String,
    phase_id: Option<String>,
    failed_at: Option<String>,
    failure_reason: Option<String>,
    started_at: String,
    completed_at: Option<String>,
}

#[derive(Debug, Clone)]
struct FailedPhaseSummary {
    phase_id: String,
    failed_at: DateTime<Utc>,
    error_message: Option<String>,
}

fn latest_failed_phase_summary(workflow: &OrchestratorWorkflow) -> Option<FailedPhaseSummary> {
    workflow
        .phases
        .iter()
        .enumerate()
        .filter(|(_, phase)| phase.status == WorkflowPhaseStatus::Failed)
        .max_by(|left, right| left.1.completed_at.cmp(&right.1.completed_at).then_with(|| left.0.cmp(&right.0)))
        .map(|(_, phase)| FailedPhaseSummary {
            phase_id: phase.phase_id.clone(),
            failed_at: phase.completed_at.unwrap_or(workflow.started_at),
            error_message: phase.error_message.clone(),
        })
}

fn workflow_summary_fields(workflow: &OrchestratorWorkflow) -> WorkflowSummaryFields {
    let failed_phase = latest_failed_phase_summary(workflow);
    let phase_id = match workflow.status {
        WorkflowStatus::Failed => failed_phase
            .as_ref()
            .map(|phase| phase.phase_id.clone())
            .or_else(|| checkpoint_phase_id(workflow)),
        _ => workflow
            .phases
            .iter()
            .find(|phase| phase.status == WorkflowPhaseStatus::Running)
            .map(|phase| phase.phase_id.clone())
            .or_else(|| checkpoint_phase_id(workflow)),
    };
    let failed_at = if workflow.status == WorkflowStatus::Failed {
        failed_phase
            .as_ref()
            .map(|phase| phase.failed_at.to_rfc3339())
            .or_else(|| workflow.completed_at.as_ref().map(chrono::DateTime::to_rfc3339))
            .or_else(|| Some(workflow.started_at.to_rfc3339()))
    } else {
        None
    };
    let failure_reason = if workflow.status == WorkflowStatus::Failed {
        workflow.failure_reason.clone().or_else(|| failed_phase.and_then(|phase| phase.error_message))
    } else {
        workflow.failure_reason.clone()
    };

    WorkflowSummaryFields {
        task_id: workflow.task_id.clone(),
        phase_id,
        failed_at,
        failure_reason,
        started_at: workflow.started_at.to_rfc3339(),
        completed_at: workflow.completed_at.as_ref().map(chrono::DateTime::to_rfc3339),
    }
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

fn requirement_priority_str(priority: RequirementPriority) -> &'static str {
    match priority {
        RequirementPriority::Must => "must",
        RequirementPriority::Should => "should",
        RequirementPriority::Could => "could",
        RequirementPriority::Wont => "wont",
    }
}

fn requirement_type_str(requirement_type: crate::RequirementType) -> &'static str {
    match requirement_type {
        crate::RequirementType::Product => "product",
        crate::RequirementType::Functional => "functional",
        crate::RequirementType::NonFunctional => "non-functional",
        crate::RequirementType::Technical => "technical",
        crate::RequirementType::Other => "other",
    }
}

fn parse_rfc3339(value: &str, field_name: &str) -> Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .with_context(|| format!("invalid timestamp in {field_name}: {value}"))
        .map(|timestamp| timestamp.with_timezone(&Utc))
}

fn parse_optional_rfc3339(value: Option<String>, field_name: &str) -> Result<Option<DateTime<Utc>>> {
    value.as_deref().map(|value| parse_rfc3339(value, field_name)).transpose()
}

pub fn save_task(project_root: &std::path::Path, task: &crate::types::OrchestratorTask) -> Result<()> {
    let conn = open_project_db(project_root)?;
    let data = compress_json(&serde_json::to_string(task)?);
    let updated_at = task.metadata.updated_at.to_rfc3339();
    let completed_at = task.metadata.completed_at.as_ref().map(chrono::DateTime::to_rfc3339);
    let blocked_at = task.blocked_at.as_ref().map(chrono::DateTime::to_rfc3339);
    conn.execute(
        "INSERT OR REPLACE INTO tasks (
            id,
            status,
            title,
            priority,
            task_type,
            updated_at,
            completed_at,
            blocked_reason,
            blocked_at,
            json
        )
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            task.id,
            task.status.to_string(),
            task.title,
            task.priority.as_str(),
            task.task_type.as_str(),
            updated_at,
            completed_at,
            task.blocked_reason,
            blocked_at,
            data
        ],
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

pub fn load_tasks_by_ids(
    project_root: &std::path::Path,
    task_ids: &[String],
) -> Result<Vec<crate::types::OrchestratorTask>> {
    if task_ids.is_empty() {
        return Ok(Vec::new());
    }

    let conn = open_project_db(project_root)?;
    let mut stmt = conn.prepare("SELECT json FROM tasks WHERE id = ?1")?;
    let mut tasks = Vec::with_capacity(task_ids.len());
    for task_id in task_ids {
        let Ok(data) = stmt.query_row([task_id], |row| row.get::<_, Vec<u8>>(0)) else {
            continue;
        };
        let json = decompress_json(&data)?;
        tasks.push(serde_json::from_str(&json)?);
    }
    Ok(tasks)
}

pub fn query_task_ids(project_root: &std::path::Path, query: &TaskQuery) -> Result<(Vec<String>, usize)> {
    let conn = open_project_db(project_root)?;
    let (where_sql, params) = supported_task_filter_sql(&query.filter)?;
    let total_sql = format!("SELECT COUNT(*) FROM tasks{where_sql}");
    let total: usize = conn
        .query_row(&total_sql, params_from_iter(params.iter()), |row| row.get::<_, i64>(0))
        .map(|count| count.max(0) as usize)?;

    let (start, end) = query.page.bounds(total);
    let limit = end.saturating_sub(start);
    if limit == 0 {
        return Ok((Vec::new(), total));
    }

    let order_sql = supported_task_sort_sql(query.sort)?;
    let ids_sql = format!("SELECT id FROM tasks{where_sql} ORDER BY {order_sql} LIMIT ? OFFSET ?");
    let mut stmt = conn.prepare(&ids_sql)?;
    let mut id_params = params;
    id_params.push(Value::Integer(limit as i64));
    id_params.push(Value::Integer(start as i64));
    let ids = stmt
        .query_map(params_from_iter(id_params.iter()), |row| row.get::<_, String>(0))?
        .filter_map(|row| row.ok())
        .collect::<Vec<_>>();

    Ok((ids, total))
}

pub fn load_next_task_by_priority(project_root: &std::path::Path) -> Result<Option<crate::types::OrchestratorTask>> {
    let conn = open_project_db(project_root)?;
    let data = conn.query_row(
        "SELECT json
         FROM tasks
         WHERE status IN ('ready', 'backlog')
         ORDER BY CASE priority
             WHEN 'critical' THEN 0
             WHEN 'high' THEN 1
             WHEN 'medium' THEN 2
             WHEN 'low' THEN 3
             ELSE 4
         END ASC,
         updated_at DESC,
         id ASC
         LIMIT 1",
        [],
        |row| row.get::<_, Vec<u8>>(0),
    );

    match data {
        Ok(data) => {
            let json = decompress_json(&data)?;
            Ok(Some(serde_json::from_str(&json)?))
        }
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(error) => Err(error.into()),
    }
}

pub fn load_task_titles_by_ids(
    project_root: &std::path::Path,
    task_ids: &[String],
) -> Result<HashMap<String, String>> {
    if task_ids.is_empty() {
        return Ok(HashMap::new());
    }

    let conn = open_project_db(project_root)?;
    let mut stmt = conn.prepare("SELECT title FROM tasks WHERE id = ?1")?;
    let mut titles = HashMap::new();
    for task_id in task_ids {
        let title = stmt.query_row([task_id], |row| row.get::<_, Option<String>>(0));
        if let Ok(Some(title)) = title {
            titles.insert(task_id.clone(), title);
        }
    }
    Ok(titles)
}

pub fn load_blocked_task_summaries(project_root: &std::path::Path) -> Result<Vec<BlockedTaskSummary>> {
    let conn = open_project_db(project_root)?;
    let mut stmt = conn.prepare(
        "SELECT id, title, blocked_reason, blocked_at
         FROM tasks
         WHERE status = 'blocked'
         ORDER BY updated_at DESC, id ASC",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, Option<String>>(1)?,
            row.get::<_, Option<String>>(2)?,
            row.get::<_, Option<String>>(3)?,
        ))
    })?;

    let mut blocked_items = Vec::new();
    for row in rows {
        let (task_id, title, blocked_reason, blocked_at) = row?;
        blocked_items.push(BlockedTaskSummary {
            task_id,
            title: title.unwrap_or_else(|| "Unknown task".to_string()),
            blocked_reason,
            blocked_at: parse_optional_rfc3339(blocked_at, "tasks.blocked_at")?,
        });
    }
    Ok(blocked_items)
}

pub fn load_stale_task_summaries(
    project_root: &std::path::Path,
    stale_before: DateTime<Utc>,
) -> Result<Vec<StaleTaskSummary>> {
    let conn = open_project_db(project_root)?;
    let stale_before = stale_before.to_rfc3339();
    let mut stmt = conn.prepare(
        "SELECT id, title, updated_at
         FROM tasks
         WHERE status = 'in_progress'
           AND updated_at IS NOT NULL
           AND updated_at < ?1
         ORDER BY updated_at ASC, id ASC",
    )?;
    let rows = stmt.query_map([stale_before], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, Option<String>>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;

    let mut stale_items = Vec::new();
    for row in rows {
        let (task_id, title, updated_at) = row?;
        stale_items.push(StaleTaskSummary {
            task_id,
            title: title.unwrap_or_else(|| "Unknown task".to_string()),
            updated_at: parse_rfc3339(&updated_at, "tasks.updated_at")?,
        });
    }
    Ok(stale_items)
}

fn supported_task_filter_sql(filter: &TaskFilter) -> Result<(String, Vec<Value>)> {
    if filter.risk.is_some()
        || filter.assignee_type.is_some()
        || filter.tags.is_some()
        || filter.linked_requirement.is_some()
        || filter.linked_architecture_entity.is_some()
        || filter.search_text.is_some()
    {
        anyhow::bail!("unsupported task filter for SQL-backed query");
    }

    let mut clauses = Vec::new();
    let mut params = Vec::new();
    if let Some(task_type) = filter.task_type {
        clauses.push("task_type = ?".to_string());
        params.push(Value::Text(task_type.as_str().to_string()));
    }
    if let Some(status) = filter.status {
        clauses.push("status = ?".to_string());
        params.push(Value::Text(status.to_string()));
    }
    if let Some(priority) = filter.priority {
        clauses.push("priority = ?".to_string());
        params.push(Value::Text(priority.as_str().to_string()));
    }

    let where_sql = if clauses.is_empty() {
        String::new()
    } else {
        format!(" WHERE {}", clauses.join(" AND "))
    };
    Ok((where_sql, params))
}

fn supported_task_sort_sql(sort: TaskQuerySort) -> Result<&'static str> {
    match sort {
        TaskQuerySort::Priority => Ok(
            "CASE priority
                WHEN 'critical' THEN 0
                WHEN 'high' THEN 1
                WHEN 'medium' THEN 2
                WHEN 'low' THEN 3
                ELSE 4
             END ASC,
             updated_at DESC,
             id ASC",
        ),
        TaskQuerySort::UpdatedAt => Ok("updated_at DESC, id ASC"),
        TaskQuerySort::Id => Ok("id ASC"),
        TaskQuerySort::CreatedAt => anyhow::bail!("created_at sort is not supported by the SQL-backed task query"),
    }
}

fn supported_requirement_filter_sql(filter: &RequirementFilter) -> Result<(String, Vec<Value>)> {
    if filter.tags.is_some() || filter.linked_task_id.is_some() || filter.search_text.is_some() {
        anyhow::bail!("unsupported requirement filter for SQL-backed query");
    }

    let mut clauses = Vec::new();
    let mut params = Vec::new();
    if let Some(status) = filter.status {
        clauses.push("status = ?".to_string());
        params.push(Value::Text(status.to_string()));
    }
    if let Some(priority) = filter.priority {
        clauses.push("priority = ?".to_string());
        params.push(Value::Text(requirement_priority_str(priority).to_string()));
    }
    if let Some(category) = filter.category.as_deref().map(str::trim).filter(|value| !value.is_empty()) {
        clauses.push("LOWER(category) = LOWER(?)".to_string());
        params.push(Value::Text(category.to_string()));
    }
    if let Some(requirement_type) = filter.requirement_type {
        clauses.push("requirement_type = ?".to_string());
        params.push(Value::Text(requirement_type_str(requirement_type).to_string()));
    }

    let where_sql = if clauses.is_empty() {
        String::new()
    } else {
        format!(" WHERE {}", clauses.join(" AND "))
    };
    Ok((where_sql, params))
}

fn supported_requirement_sort_sql(sort: RequirementQuerySort) -> &'static str {
    match sort {
        RequirementQuerySort::Id => "id ASC",
        RequirementQuerySort::UpdatedAt => "updated_at DESC, id ASC",
        RequirementQuerySort::Priority => "CASE priority
            WHEN 'must' THEN 0
            WHEN 'should' THEN 1
            WHEN 'could' THEN 2
            WHEN 'wont' THEN 3
            ELSE 4
         END ASC,
         updated_at DESC,
         id ASC",
        RequirementQuerySort::Status => "status ASC, updated_at DESC, id ASC",
    }
}

pub fn load_task_statistics(project_root: &std::path::Path) -> Result<TaskStatistics> {
    let conn = open_project_db(project_root)?;
    let total = query_count(&conn, "SELECT COUNT(*) FROM tasks")?;
    let by_status = query_grouped_counts(&conn, "SELECT status, COUNT(*) FROM tasks GROUP BY status")?;
    let by_priority = query_grouped_counts(
        &conn,
        "SELECT priority, COUNT(*) FROM tasks WHERE priority IS NOT NULL GROUP BY priority",
    )?;
    let by_type = query_grouped_counts(
        &conn,
        "SELECT task_type, COUNT(*) FROM tasks WHERE task_type IS NOT NULL GROUP BY task_type",
    )?;
    let in_progress = query_count(&conn, "SELECT COUNT(*) FROM tasks WHERE status = 'in-progress'")?;
    let blocked = query_count(&conn, "SELECT COUNT(*) FROM tasks WHERE status IN ('blocked', 'on-hold')")?;
    let completed = query_count(&conn, "SELECT COUNT(*) FROM tasks WHERE status IN ('done', 'cancelled')")?;

    Ok(TaskStatistics { total, by_status, by_priority, by_type, in_progress, blocked, completed })
}

pub fn count_tasks_with_status(project_root: &std::path::Path, status: TaskStatus) -> Result<usize> {
    let conn = open_project_db(project_root)?;
    let count: i64 =
        conn.query_row("SELECT COUNT(*) FROM tasks WHERE status = ?1", params![status.to_string()], |row| row.get(0))?;
    Ok(count.max(0) as usize)
}

pub fn load_task_priority_policy_report(
    project_root: &std::path::Path,
    high_budget_percent: u8,
) -> Result<TaskPriorityPolicyReport> {
    if high_budget_percent > 100 {
        anyhow::bail!("high_budget_percent must be between 0 and 100");
    }

    let conn = open_project_db(project_root)?;
    let total_tasks = query_count(&conn, "SELECT COUNT(*) FROM tasks")?;
    let active_tasks = query_count(&conn, "SELECT COUNT(*) FROM tasks WHERE status NOT IN ('done', 'cancelled')")?;
    let total_by_priority = query_priority_distribution(
        &conn,
        "SELECT priority, COUNT(*) FROM tasks WHERE priority IS NOT NULL GROUP BY priority",
    )?;
    let active_by_priority = query_priority_distribution(
        &conn,
        "SELECT priority, COUNT(*) FROM tasks
         WHERE priority IS NOT NULL
           AND status NOT IN ('done', 'cancelled')
         GROUP BY priority",
    )?;
    let high_budget_limit = active_tasks.saturating_mul(usize::from(high_budget_percent)) / 100;
    let high_budget_overflow = active_by_priority.high.saturating_sub(high_budget_limit);

    Ok(TaskPriorityPolicyReport {
        high_budget_percent,
        high_budget_limit,
        total_tasks,
        active_tasks,
        total_by_priority,
        active_by_priority,
        high_budget_compliant: high_budget_overflow == 0,
        high_budget_overflow,
    })
}

fn query_count(conn: &Connection, sql: &str) -> Result<usize> {
    let count: i64 = conn.query_row(sql, [], |row| row.get(0))?;
    Ok(count.max(0) as usize)
}

fn query_grouped_counts(conn: &Connection, sql: &str) -> Result<HashMap<String, usize>> {
    let mut stmt = conn.prepare(sql)?;
    let mut counts = HashMap::new();
    let rows = stmt.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)))?;
    for row in rows {
        let (key, value) = row?;
        counts.insert(key, value.max(0) as usize);
    }
    Ok(counts)
}

fn query_priority_distribution(conn: &Connection, sql: &str) -> Result<TaskPriorityDistribution> {
    let mut stmt = conn.prepare(sql)?;
    let mut distribution = TaskPriorityDistribution::default();
    let rows = stmt.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)))?;
    for row in rows {
        let (priority, count) = row?;
        let count = count.max(0) as usize;
        match priority.as_str() {
            "critical" => distribution.critical = count,
            "high" => distribution.high = count,
            "medium" => distribution.medium = count,
            "low" => distribution.low = count,
            _ => {}
        }
    }
    Ok(distribution)
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
        "INSERT OR REPLACE INTO requirements (id, status, priority, category, requirement_type, updated_at, json)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            req.id,
            req.status.to_string(),
            requirement_priority_str(req.priority),
            req.category,
            req.requirement_type.map(requirement_type_str),
            req.updated_at.to_rfc3339(),
            data
        ],
    )?;
    Ok(())
}

pub fn load_requirement(project_root: &std::path::Path, req_id: &str) -> Result<crate::types::RequirementItem> {
    let conn = open_project_db(project_root)?;
    let data: Vec<u8> = conn
        .query_row("SELECT json FROM requirements WHERE id = ?1", params![req_id], |row| row.get(0))
        .map_err(|_| anyhow!("requirement not found: {req_id}"))?;
    let json = decompress_json(&data)?;
    Ok(serde_json::from_str(&json)?)
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

pub fn load_requirements_by_ids(project_root: &std::path::Path, requirement_ids: &[String]) -> Result<Vec<RequirementItem>> {
    if requirement_ids.is_empty() {
        return Ok(Vec::new());
    }

    let conn = open_project_db(project_root)?;
    let mut stmt = conn.prepare("SELECT json FROM requirements WHERE id = ?1")?;
    let mut requirements = Vec::with_capacity(requirement_ids.len());
    for requirement_id in requirement_ids {
        let Ok(data) = stmt.query_row([requirement_id], |row| row.get::<_, Vec<u8>>(0)) else {
            continue;
        };
        let json = decompress_json(&data)?;
        requirements.push(serde_json::from_str(&json)?);
    }
    Ok(requirements)
}

pub fn load_requirement_link_summaries_by_ids(
    project_root: &std::path::Path,
    requirement_ids: &[String],
) -> Result<Vec<RequirementLinkSummary>> {
    if requirement_ids.is_empty() {
        return Ok(Vec::new());
    }

    let conn = open_project_db(project_root)?;
    let mut stmt = conn.prepare("SELECT title, priority FROM requirements WHERE id = ?1")?;
    let mut requirements = Vec::with_capacity(requirement_ids.len());
    for requirement_id in requirement_ids {
        let Ok((title, priority)) = stmt.query_row([requirement_id], |row| {
            Ok((row.get::<_, Option<String>>(0)?, row.get::<_, Option<String>>(1)?))
        }) else {
            continue;
        };
        requirements.push(RequirementLinkSummary {
            requirement_id: requirement_id.clone(),
            title: title.unwrap_or_else(|| "Unknown requirement".to_string()),
            priority: priority.unwrap_or_else(|| "unknown".to_string()),
        });
    }
    Ok(requirements)
}

pub fn query_requirement_ids(project_root: &std::path::Path, query: &RequirementQuery) -> Result<(Vec<String>, usize)> {
    let conn = open_project_db(project_root)?;
    let (where_sql, params) = supported_requirement_filter_sql(&query.filter)?;
    let total_sql = format!("SELECT COUNT(*) FROM requirements{where_sql}");
    let total: usize = conn
        .query_row(&total_sql, params_from_iter(params.iter()), |row| row.get::<_, i64>(0))
        .map(|count| count.max(0) as usize)?;

    let (start, end) = query.page.bounds(total);
    let limit = end.saturating_sub(start);
    if limit == 0 {
        return Ok((Vec::new(), total));
    }

    let order_sql = supported_requirement_sort_sql(query.sort);
    let ids_sql = format!("SELECT id FROM requirements{where_sql} ORDER BY {order_sql} LIMIT ? OFFSET ?");
    let mut stmt = conn.prepare(&ids_sql)?;
    let mut id_params = params;
    id_params.push(Value::Integer(limit as i64));
    id_params.push(Value::Integer(start as i64));
    let ids = stmt
        .query_map(params_from_iter(id_params.iter()), |row| row.get::<_, String>(0))?
        .filter_map(|row| row.ok())
        .collect::<Vec<_>>();

    Ok((ids, total))
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
