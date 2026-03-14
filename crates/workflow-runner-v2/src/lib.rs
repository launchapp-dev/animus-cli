pub mod config_context;
pub mod ipc;
pub mod payload_traversal;
pub mod phase_failover;
pub mod phase_targets;
pub mod runtime_support;
pub mod skill_dispatch;
pub mod phase_git;
pub mod phase_output;
pub mod phase_prompt;
pub mod runtime_contract;
pub mod phase_command;
pub mod phase_executor;
pub mod workflow_helpers;
pub mod workflow_merge_recovery;
pub mod ensure_execution_cwd;
pub mod workflow_execute;

pub use ipc::*;
pub use phase_failover::{PhaseFailureClassifier, PhaseFailureKind, classify_phase_failure};
pub use phase_targets::PhaseTargetPlanner;
pub use runtime_support::*;
pub use phase_executor::{
    PhaseExecuteOverrides, PhaseExecutionOutcome, PhaseExecutionMetadata,
    PhaseExecutionSignal, CliPhaseExecutor, run_workflow_phase, PhaseRunResult,
    PhaseRunParams, load_agent_runtime_config,
};
pub use phase_output::{persist_phase_output, PersistedPhaseOutput, phase_output_dir};
pub use phase_prompt::{
    build_phase_prompt, render_phase_prompt, PhasePromptInputs, PhasePromptParams,
    PhaseRenderParams, RenderedPhasePrompt,
    phase_requires_commit_message, phase_requires_commit_message_with_config,
};
pub use phase_git::{is_git_repo, git_has_pending_changes, ensure_git_identity, commit_implementation_changes};
pub use workflow_merge_recovery::MergeConflictContext;
pub use workflow_helpers::{
    PhaseExecutionEvent, AiRecoveryAction, AiRecoverySubtask,
    task_requires_research, workflow_has_completed_research, workflow_has_active_research,
};
pub use payload_traversal::{
    parse_phase_decision_from_text, parse_commit_message_from_text,
    fallback_implementation_commit_message,
};
pub use workflow_execute::{execute_workflow, WorkflowExecuteParams, WorkflowExecuteResult, PhaseEvent};
pub use ensure_execution_cwd::ensure_execution_cwd;
