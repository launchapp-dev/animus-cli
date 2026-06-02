use serde::{Deserialize, Serialize};

use orchestrator_config::agent_runtime_config::Idempotency;
use orchestrator_core::AgentRuntimeConfig;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MergeConflictContext {
    pub source_branch: String,
    pub target_branch: String,
    pub merge_worktree_path: String,
    pub conflicted_files: Vec<String>,
    pub merge_queue_branch: String,
    pub push_remote: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PhaseRecoveryAction {
    AutoRetry,
    BlockSideeffecting { reason: String },
    BlockUnknown { reason: String },
}

impl PhaseRecoveryAction {
    pub fn is_blocking(&self) -> bool {
        !matches!(self, PhaseRecoveryAction::AutoRetry)
    }

    pub fn reason(&self) -> Option<&str> {
        match self {
            PhaseRecoveryAction::AutoRetry => None,
            PhaseRecoveryAction::BlockSideeffecting { reason } | PhaseRecoveryAction::BlockUnknown { reason } => {
                Some(reason.as_str())
            }
        }
    }
}

pub fn block_reason_sideeffecting(phase_id: &str) -> String {
    format!(
        "phase '{phase_id}' is sideeffecting and may have partially executed; resolve manually with `animus workflow resume <run_id> --force`"
    )
}

pub fn block_reason_unknown(phase_id: &str) -> String {
    format!(
        "phase '{phase_id}' has no idempotency annotation; treating as sideeffecting. Mark in workflow YAML to allow auto-retry."
    )
}

pub fn phase_idempotency_for(runtime: &AgentRuntimeConfig, phase_id: &str) -> Idempotency {
    runtime.phases.get(phase_id).map(|def| def.idempotency).unwrap_or_default()
}

pub fn classify_phase_recovery(runtime: &AgentRuntimeConfig, phase_id: &str) -> PhaseRecoveryAction {
    match phase_idempotency_for(runtime, phase_id) {
        Idempotency::Idempotent => PhaseRecoveryAction::AutoRetry,
        Idempotency::Sideeffecting => {
            PhaseRecoveryAction::BlockSideeffecting { reason: block_reason_sideeffecting(phase_id) }
        }
        Idempotency::Unknown => PhaseRecoveryAction::BlockUnknown { reason: block_reason_unknown(phase_id) },
    }
}
