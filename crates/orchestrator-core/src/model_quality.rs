use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::types::PhaseDecisionVerdict;

pub const MODEL_QUALITY_LEDGER_FILE_NAME: &str = "model-quality-ledger.v1.json";

const MIN_ATTEMPTS_FOR_SUPPRESSION: u32 = 3;
const SUPPRESSION_FAIL_RATE_THRESHOLD: f64 = 0.7;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ModelQualityRecord {
    pub model_id: String,
    pub phase_id: String,
    pub attempts: u32,
    pub advances: u32,
    pub reworks: u32,
    pub fails: u32,
    #[serde(default)]
    pub suppressed: bool,
    pub last_recorded_at: String,
}

impl ModelQualityRecord {
    fn fail_rate(&self) -> f64 {
        if self.attempts == 0 {
            return 0.0;
        }
        self.fails as f64 / self.attempts as f64
    }

    fn should_suppress(&self) -> bool {
        self.attempts >= MIN_ATTEMPTS_FOR_SUPPRESSION && self.fail_rate() >= SUPPRESSION_FAIL_RATE_THRESHOLD
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ModelQualityLedger {
    #[serde(default)]
    pub schema: String,
    #[serde(default)]
    pub updated_at: String,
    #[serde(default)]
    pub records: HashMap<String, ModelQualityRecord>,
}

fn ledger_key(model_id: &str, phase_id: &str) -> String {
    format!("{}:{}", model_id, phase_id)
}

pub fn model_quality_ledger_path(project_root: &Path) -> PathBuf {
    project_root.join(".ao").join("state").join(MODEL_QUALITY_LEDGER_FILE_NAME)
}

pub fn load_model_quality_ledger(project_root: &Path) -> ModelQualityLedger {
    let path = model_quality_ledger_path(project_root);
    if !path.exists() {
        return ModelQualityLedger::default();
    }
    let Ok(content) = std::fs::read_to_string(&path) else {
        return ModelQualityLedger::default();
    };
    serde_json::from_str::<ModelQualityLedger>(&content).unwrap_or_default()
}

fn save_model_quality_ledger(project_root: &Path, ledger: &ModelQualityLedger) {
    let path = model_quality_ledger_path(project_root);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let Ok(payload) = serde_json::to_string_pretty(ledger) else {
        return;
    };
    let tmp_path = path.with_file_name(format!(
        "{}.{}.tmp",
        path.file_name().and_then(|n| n.to_str()).unwrap_or(MODEL_QUALITY_LEDGER_FILE_NAME),
        uuid::Uuid::new_v4()
    ));
    if std::fs::write(&tmp_path, payload).is_ok() {
        let _ = std::fs::rename(&tmp_path, &path);
    }
}

pub fn record_model_phase_outcome(project_root: &Path, model_id: &str, phase_id: &str, verdict: PhaseDecisionVerdict) {
    let model_id = model_id.trim();
    let phase_id = phase_id.trim();
    if model_id.is_empty() || phase_id.is_empty() {
        return;
    }

    let mut ledger = load_model_quality_ledger(project_root);
    let key = ledger_key(model_id, phase_id);
    let record = ledger.records.entry(key).or_insert_with(|| ModelQualityRecord {
        model_id: model_id.to_string(),
        phase_id: phase_id.to_string(),
        ..Default::default()
    });

    record.attempts = record.attempts.saturating_add(1);
    match verdict {
        PhaseDecisionVerdict::Advance | PhaseDecisionVerdict::Skip => {
            record.advances = record.advances.saturating_add(1);
        }
        PhaseDecisionVerdict::Rework => {
            record.reworks = record.reworks.saturating_add(1);
        }
        PhaseDecisionVerdict::Fail | PhaseDecisionVerdict::Unknown => {
            record.fails = record.fails.saturating_add(1);
        }
    }
    record.suppressed = record.should_suppress();
    record.last_recorded_at = chrono::Utc::now().to_rfc3339();

    ledger.schema = "ao.model-quality.v1".to_string();
    ledger.updated_at = chrono::Utc::now().to_rfc3339();

    save_model_quality_ledger(project_root, &ledger);
}

pub fn is_model_suppressed_for_phase(project_root: &Path, model_id: &str, phase_id: &str) -> bool {
    let ledger = load_model_quality_ledger(project_root);
    let key = ledger_key(model_id, phase_id);
    ledger.records.get(&key).map(|record| record.suppressed).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::PhaseDecisionVerdict;

    #[test]
    fn suppression_triggers_at_threshold() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();

        for _ in 0..3 {
            record_model_phase_outcome(root, "test-model", "implementation", PhaseDecisionVerdict::Fail);
        }

        assert!(is_model_suppressed_for_phase(root, "test-model", "implementation"));
    }

    #[test]
    fn suppression_requires_min_attempts() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();

        record_model_phase_outcome(root, "test-model", "implementation", PhaseDecisionVerdict::Fail);
        record_model_phase_outcome(root, "test-model", "implementation", PhaseDecisionVerdict::Fail);

        assert!(!is_model_suppressed_for_phase(root, "test-model", "implementation"));
    }

    #[test]
    fn suppression_clears_after_recovery() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();

        for _ in 0..3 {
            record_model_phase_outcome(root, "test-model", "implementation", PhaseDecisionVerdict::Fail);
        }
        assert!(is_model_suppressed_for_phase(root, "test-model", "implementation"));

        for _ in 0..7 {
            record_model_phase_outcome(root, "test-model", "implementation", PhaseDecisionVerdict::Advance);
        }
        assert!(!is_model_suppressed_for_phase(root, "test-model", "implementation"));
    }

    #[test]
    fn suppression_is_phase_scoped() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();

        for _ in 0..3 {
            record_model_phase_outcome(root, "test-model", "implementation", PhaseDecisionVerdict::Fail);
        }

        assert!(is_model_suppressed_for_phase(root, "test-model", "implementation"));
        assert!(!is_model_suppressed_for_phase(root, "test-model", "testing"));
        assert!(!is_model_suppressed_for_phase(root, "other-model", "implementation"));
    }

    #[test]
    fn rework_verdict_increments_reworks_not_fails() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();

        for _ in 0..10 {
            record_model_phase_outcome(root, "test-model", "implementation", PhaseDecisionVerdict::Rework);
        }

        let ledger = load_model_quality_ledger(root);
        let key = ledger_key("test-model", "implementation");
        let record = ledger.records.get(&key).expect("record should exist");
        assert_eq!(record.reworks, 10);
        assert_eq!(record.fails, 0);
        assert!(!record.suppressed);
    }

    #[test]
    fn advance_verdict_lifts_suppression_over_time() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();

        for _ in 0..7 {
            record_model_phase_outcome(root, "test-model", "implementation", PhaseDecisionVerdict::Fail);
        }
        assert!(is_model_suppressed_for_phase(root, "test-model", "implementation"));

        for _ in 0..10 {
            record_model_phase_outcome(root, "test-model", "implementation", PhaseDecisionVerdict::Advance);
        }
        assert!(!is_model_suppressed_for_phase(root, "test-model", "implementation"));
    }

    #[test]
    fn unknown_verdict_counts_as_fail() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();

        for _ in 0..3 {
            record_model_phase_outcome(root, "test-model", "implementation", PhaseDecisionVerdict::Unknown);
        }

        let ledger = load_model_quality_ledger(root);
        let key = ledger_key("test-model", "implementation");
        let record = ledger.records.get(&key).expect("record should exist");
        assert_eq!(record.fails, 3);
        assert!(record.suppressed);
    }
}
