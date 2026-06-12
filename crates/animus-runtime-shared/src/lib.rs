//! `animus-runtime-shared` — runtime helpers shared by Animus's
//! `workflow_runner` plugins (e.g. `animus-workflow-runner-default`) and the
//! kernel daemon (`animus-cli`).
//!
//! The crate's role is to dedupe the modules that BOTH the plugin and the
//! kernel daemon need to understand byte-identically: phase session/output
//! state on disk, agent memory documents, the runtime contract construction,
//! the runner Unix-socket IPC bridge, workflow event emitters, and the
//! reattach back-channel.
//!
//! Heavy execution machinery (`phase_executor`, `workflow_execute`,
//! `phase_targets`, `phase_failover`, `phase_command`, `skill_dispatch`,
//! `direct_exec`) is plugin-private and intentionally NOT here.

pub mod agent_state;
pub mod config_context;
pub mod ensure_execution_cwd;
pub mod evals;
pub mod interactions;
pub mod ipc;
pub mod metrics_hook;
pub mod notification_log;
pub mod oauth_broker;
pub mod payload_traversal;
pub mod phase_git;
pub mod phase_metadata;
pub mod phase_output;
pub mod phase_prompt;
pub mod phase_session;
pub mod reattach;
pub mod recording;
pub mod runtime_contract;
pub mod runtime_support;
pub mod workflow_event_emitter;
pub mod workflow_helpers;
pub mod workflow_merge_recovery;

pub use agent_state::{
    append_agent_memory, clear_agent_memory, delete_agent_memory_entry, list_agent_messages, load_agent_memory,
    send_agent_message, AgentMemoryDocument, AgentMemoryEntry, AgentMessage,
};
pub use ensure_execution_cwd::ensure_execution_cwd;
pub use evals::llm_judge_runner::{JudgeError, JudgeRequest, JudgeResponse};
pub use evals::{
    evaluate_outcome, run_evals, EvalCheckResult, EvalContext, EvalsDecision, EvalsOutcome, JudgeBackend,
    EVAL_RESULT_SCHEMA_ID,
};
pub use interactions::{
    answer_interaction, create_approval_interaction, create_question_interaction, expire_interaction,
    list_interactions, load_interaction, mark_interaction_suspended, InteractionKind, InteractionRecord,
    InteractionStatus, INTERACTION_ANSWER_ALLOW, INTERACTION_ANSWER_DENY,
};
pub use ipc::*;
pub use payload_traversal::{
    fallback_implementation_commit_message, parse_commit_message_from_text, parse_phase_decision_from_text,
};
pub use phase_git::{commit_implementation_changes, ensure_git_identity, git_has_pending_changes, is_git_repo};
pub use phase_metadata::{PhaseExecutionMetadata, PhaseExecutionOutcome, PhaseExecutionSignal};
pub use phase_output::{
    is_phase_completed, persist_phase_output, persist_resumed_phase_completion, phase_completion_marker_path,
    phase_output_dir, read_persisted_decision, write_phase_completion_marker, PersistedDecisionReadError,
    PersistedPhaseOutput, PhaseCompletionMarker,
};
pub use phase_prompt::{apply_skill_prompt_to_body, merge_skill_system_prompt, skill_directives_section};
pub use phase_prompt::{
    build_phase_prompt, phase_requires_commit_message, phase_requires_commit_message_with_config,
    phase_requires_commit_message_with_ctx, phase_result_kind_for_ctx, render_phase_prompt,
    render_phase_prompt_with_ctx, render_phase_prompt_with_ctx_overrides, PhasePromptInputs, PhasePromptParams,
    PhaseRenderParams, RenderedPhasePrompt,
};
pub use runtime_contract::{install_memory_mcp_stdio_command_override, validate_basic_json_schema};
pub use runtime_support::*;
pub use workflow_event_emitter::{
    FanoutEmitter, NoopWorkflowEventEmitter, RuntimeWorkflowEvent, RuntimeWorkflowEventKind,
    SharedWorkflowEventEmitter, SubprocessPipeEmitter, WireWorkflowEvent, WorkflowEventEmitter,
    ANIMUS_WORKFLOW_EVENT_PIPE_ENV,
};
pub use workflow_helpers::{
    task_requires_research, workflow_has_active_research, workflow_has_completed_research, PhaseExecutionEvent,
};
pub use workflow_merge_recovery::{
    block_reason_sideeffecting, block_reason_unknown, classify_phase_recovery, phase_idempotency_for,
    MergeConflictContext, PhaseRecoveryAction,
};

#[cfg(test)]
pub(crate) mod test_env {
    use std::sync::{Mutex, MutexGuard, OnceLock};

    /// Returns the per-process test home directory and pins HOME to it on first call.
    pub fn stable_test_home() -> &'static std::path::Path {
        static HOME: OnceLock<std::path::PathBuf> = OnceLock::new();
        HOME.get_or_init(|| {
            let home_dir = std::env::temp_dir()
                .join(format!("animus-runtime-shared-test-home-{}", std::process::id()))
                .join("home");
            std::fs::create_dir_all(&home_dir).expect("create shared animus-runtime-shared test home");
            std::env::set_var("HOME", &home_dir);
            home_dir
        })
    }

    /// Process-wide lock for tests that depend on `protocol::scoped_state_root`.
    pub fn scoped_state_serializer() -> MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        stable_test_home();
        LOCK.get_or_init(|| Mutex::new(())).lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}
