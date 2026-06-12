//! Disk persistence for [`CostState`] and budget-exceeded decision
//! records.
//!
//! State location: `<scoped-root>/cost-state.v1.json`. Writes go via a
//! temp file + rename so a crash mid-write cannot leave a partial JSON
//! document.

#![allow(dead_code)]

use std::fs;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use super::aggregator::{BudgetExceededRecord, CostState, COST_STATE_SCHEMA_ID};

pub const COST_STATE_FILE_NAME: &str = "cost-state.v1.json";
pub const DECISIONS_FILE_NAME: &str = "decisions.jsonl";
pub const BUDGET_ENFORCEMENT_FILE_NAME: &str = "budget-enforcement.v1.json";
pub const BUDGET_ENFORCEMENT_SCHEMA_ID: &str = "animus.budget-enforcement.v1";

/// Last-known status of the daemon's budget-enforcement housekeeping leg,
/// written by the sweep each heartbeat (even when the kill-switch skips the
/// actual enforcement) so `daemon health` / `animus status` can report
/// `{enabled, last_sweep_at}` without reading the daemon's process env.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct BudgetEnforcementStatus {
    pub schema: String,
    /// `false` when `ANIMUS_DAEMON_DISABLE_BUDGET_ENFORCEMENT=1` skipped the
    /// enforcement leg on the most recent sweep.
    pub enabled: bool,
    /// RFC3339 timestamp of the most recent sweep (enabled or skipped).
    pub last_sweep_at: chrono::DateTime<chrono::Utc>,
}

pub fn budget_enforcement_status_path(project_root: &Path) -> PathBuf {
    scoped_root(project_root).join(BUDGET_ENFORCEMENT_FILE_NAME)
}

/// Persist the budget-enforcement leg status (atomic write). Best-effort:
/// the caller logs but does not fail the tick on a write error.
pub fn save_budget_enforcement_status(project_root: &Path, enabled: bool) -> Result<()> {
    let status = BudgetEnforcementStatus {
        schema: BUDGET_ENFORCEMENT_SCHEMA_ID.to_string(),
        enabled,
        last_sweep_at: chrono::Utc::now(),
    };
    let path = budget_enforcement_status_path(project_root);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("create budget-enforcement parent {}", parent.display()))?;
    }
    let serialized = serde_json::to_string_pretty(&status).context("serialize budget-enforcement status")?;
    atomic_write(&path, serialized.as_bytes())
        .with_context(|| format!("write budget-enforcement status {}", path.display()))
}

/// Read the persisted budget-enforcement leg status, or `None` when the
/// daemon has not run a sweep yet (file absent / unreadable / malformed).
pub fn load_budget_enforcement_status(project_root: &Path) -> Option<BudgetEnforcementStatus> {
    let path = budget_enforcement_status_path(project_root);
    let text = fs::read_to_string(&path).ok()?;
    serde_json::from_str(text.trim()).ok()
}

/// `<scoped-root>` for the given project root, falling back to
/// `<project_root>/.animus` when the repository scope cannot be
/// computed (mirrors the resolver used by `animus-runtime-shared`).
///
/// `ANIMUS_COST_STATE_ROOT` overrides the resolved path. This is the
/// supported test seam: production callers leave it unset and the
/// resolver picks the standard scoped root. Tests under this crate set
/// it to a `TempDir` so they do not need to mutate `HOME`, which would
/// race with other tests that also read `HOME`.
pub fn scoped_root(project_root: &Path) -> PathBuf {
    if let Some(override_path) = std::env::var_os("ANIMUS_COST_STATE_ROOT") {
        return PathBuf::from(override_path);
    }
    protocol::scoped_state_root(project_root).unwrap_or_else(|| project_root.join(".animus"))
}

pub fn cost_state_path(project_root: &Path) -> PathBuf {
    scoped_root(project_root).join(COST_STATE_FILE_NAME)
}

pub fn decisions_log_path(project_root: &Path) -> PathBuf {
    scoped_root(project_root).join(DECISIONS_FILE_NAME)
}

pub fn load_cost_state(project_root: &Path) -> Result<CostState> {
    let path = cost_state_path(project_root);
    if !path.exists() {
        return Ok(CostState::default());
    }
    let text = fs::read_to_string(&path).with_context(|| format!("read cost state {}", path.display()))?;
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(CostState::default());
    }
    let parsed: CostState =
        serde_json::from_str(trimmed).with_context(|| format!("parse cost state {}", path.display()))?;
    if parsed.schema != COST_STATE_SCHEMA_ID {
        anyhow::bail!(
            "cost state schema mismatch at {}: expected '{}', got '{}'",
            path.display(),
            COST_STATE_SCHEMA_ID,
            parsed.schema
        );
    }
    Ok(parsed)
}

pub fn save_cost_state(project_root: &Path, state: &CostState) -> Result<()> {
    let path = cost_state_path(project_root);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("create cost state parent {}", parent.display()))?;
    }
    let serialized = serde_json::to_string_pretty(state).context("serialize cost state")?;
    atomic_write(&path, serialized.as_bytes()).with_context(|| format!("write cost state {}", path.display()))
}

pub fn append_decision_record(project_root: &Path, record: &BudgetExceededRecord) -> Result<()> {
    let path = decisions_log_path(project_root);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("create decisions parent {}", parent.display()))?;
    }
    let line = serde_json::to_string(record).context("serialize budget-exceeded record")?;
    let mut file = fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(&path)
        .with_context(|| format!("open decisions log {}", path.display()))?;
    file.write_all(line.as_bytes())
        .and_then(|()| file.write_all(b"\n"))
        .with_context(|| format!("write decision record to {}", path.display()))
}

pub fn read_decision_records(project_root: &Path) -> Result<Vec<BudgetExceededRecord>> {
    let path = decisions_log_path(project_root);
    if !path.exists() {
        return Ok(Vec::new());
    }
    let text = fs::read_to_string(&path).with_context(|| format!("read decisions log {}", path.display()))?;
    let mut records = Vec::new();
    for (line_no, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let record: BudgetExceededRecord = serde_json::from_str(line)
            .with_context(|| format!("parse decision record at {}:{}", path.display(), line_no + 1))?;
        records.push(record);
    }
    Ok(records)
}

fn atomic_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let tmp_path = match path.file_name() {
        Some(name) => {
            let mut tmp_name = name.to_os_string();
            tmp_name.push(".tmp");
            path.with_file_name(tmp_name)
        }
        None => return Err(io::Error::new(io::ErrorKind::InvalidInput, "cost state path has no file name")),
    };
    {
        let mut file = fs::File::create(&tmp_path)?;
        file.write_all(bytes)?;
        file.sync_all()?;
    }
    fs::rename(&tmp_path, path)
}

#[cfg(test)]
mod tests {
    use super::super::aggregator::{BudgetLimitField, BudgetLimitKind, WorkflowCost};
    use super::*;
    use crate::shared::test_env_lock;
    use chrono::Utc;
    use protocol::test_utils::EnvVarGuard;
    use tempfile::TempDir;

    fn arrange_override(tmp: &TempDir) -> (EnvVarGuard, std::path::PathBuf) {
        let state_root = tmp.path().join("scope");
        fs::create_dir_all(&state_root).unwrap();
        let project_root = tmp.path().join("project");
        fs::create_dir_all(&project_root).unwrap();
        let guard = EnvVarGuard::set("ANIMUS_COST_STATE_ROOT", Some(state_root.to_string_lossy().as_ref()));
        (guard, project_root)
    }

    #[test]
    fn save_then_load_round_trips_state() {
        let _lock = test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let tmp = TempDir::new().unwrap();
        let (_override_guard, project_root) = arrange_override(&tmp);
        let now = Utc::now();
        let mut state = CostState::default();
        state.workflows.insert("wf-flow-1".to_string(), WorkflowCost::new("flow", now));
        save_cost_state(&project_root, &state).unwrap();
        let reloaded = load_cost_state(&project_root).unwrap();
        assert!(reloaded.workflows.contains_key("wf-flow-1"));
    }

    #[test]
    fn missing_state_returns_default() {
        let _lock = test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let tmp = TempDir::new().unwrap();
        let (_override_guard, project_root) = arrange_override(&tmp);
        let loaded = load_cost_state(&project_root).unwrap();
        assert!(loaded.workflows.is_empty());
    }

    #[test]
    fn append_and_read_decision_records_round_trip() {
        let _lock = test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let tmp = TempDir::new().unwrap();
        let (_override_guard, project_root) = arrange_override(&tmp);
        let record = BudgetExceededRecord {
            schema: super::super::aggregator::BUDGET_EXCEEDED_SCHEMA_ID.to_string(),
            workflow_run_id: "wf-x-1".to_string(),
            workflow_id: "flow".to_string(),
            phase_id: Some("impl".to_string()),
            limit_kind: BudgetLimitKind::Phase,
            limit_field: BudgetLimitField::MaxTokens,
            actual: 150_000.0,
            budget: 100_000.0,
            on_exceed: "pause".to_string(),
            observed_at: Utc::now(),
        };
        append_decision_record(&project_root, &record).unwrap();
        append_decision_record(&project_root, &record).unwrap();
        let read_back = read_decision_records(&project_root).unwrap();
        assert_eq!(read_back.len(), 2);
        assert_eq!(read_back[0].workflow_run_id, "wf-x-1");
    }

    #[test]
    fn budget_enforcement_status_round_trips() {
        let _lock = test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let tmp = TempDir::new().unwrap();
        let (_override_guard, project_root) = arrange_override(&tmp);
        // No sweep yet → None.
        assert!(load_budget_enforcement_status(&project_root).is_none());
        save_budget_enforcement_status(&project_root, true).unwrap();
        let loaded = load_budget_enforcement_status(&project_root).expect("status persisted");
        assert!(loaded.enabled);
        assert_eq!(loaded.schema, BUDGET_ENFORCEMENT_SCHEMA_ID);
        // A later sweep can flip enabled → false (kill-switch engaged).
        save_budget_enforcement_status(&project_root, false).unwrap();
        assert!(!load_budget_enforcement_status(&project_root).unwrap().enabled);
    }

    #[test]
    fn schema_mismatch_errors() {
        let _lock = test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let tmp = TempDir::new().unwrap();
        let (_override_guard, project_root) = arrange_override(&tmp);
        let path = cost_state_path(&project_root);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&path, r#"{"schema":"bogus.v0","workflows":{}}"#).unwrap();
        let err = load_cost_state(&project_root).unwrap_err().to_string();
        assert!(err.contains("schema mismatch"), "expected schema mismatch error, got: {err}");
    }
}
