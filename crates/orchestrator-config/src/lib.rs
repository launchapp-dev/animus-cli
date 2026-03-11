pub mod agent_runtime_config;
mod json;
pub mod skill_definition;
pub mod skill_resolution;
pub mod skill_scoping;
pub mod workflow_config;

pub const DEFAULT_CHECKPOINT_RETENTION_KEEP_LAST_PER_PHASE: usize = 3;

pub mod domain_state {
    pub use crate::json::write_json_pretty;
}

pub mod workflow {
    pub use crate::DEFAULT_CHECKPOINT_RETENTION_KEEP_LAST_PER_PHASE;
}

pub mod types {
    pub use protocol::orchestrator::{PhaseEvidenceKind, WorkflowDecisionRisk};
}

pub use agent_runtime_config::*;
pub use skill_definition::*;
pub use workflow_config::*;
