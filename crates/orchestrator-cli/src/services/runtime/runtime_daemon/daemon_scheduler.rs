use super::daemon_registry::canonicalize_lossy;
use crate::cli_types::DaemonRunArgs;
use crate::shared::{
    build_runtime_contract, collect_json_payload_lines, connect_runner,
    ensure_ai_generated_tasks_for_requirements, event_matches_run, requirement_has_active_tasks,
    run_prompt_against_runner, runner_config_dir, write_json_line,
};
use anyhow::{anyhow, Context, Result};
use chrono::Utc;
use orchestrator_core::{
    services::ServiceHub, DependencyType, FileServiceHub, RequirementItem, RequirementStatus,
    RequirementsDraftInput, RequirementsExecutionInput, RequirementsRefineInput, TaskCreateInput,
    TaskStatus, TaskType, WorkflowResumeManager, WorkflowRunInput, WorkflowStatus,
};
use protocol::{
    default_primary_model_for_phase, tool_for_model_id, AgentRunEvent, AgentRunRequest, ModelId,
    RunId, PROTOCOL_VERSION,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashSet, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command as ProcessCommand, Stdio};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::time::sleep;
use uuid::Uuid;

#[path = "daemon_agent_pool.rs"]
pub(super) mod daemon_agent_pool;
#[path = "daemon_scheduler_frontend_gate.rs"]
mod frontend_phase_gate;
#[path = "daemon_scheduler_git_ops.rs"]
mod git_ops;
#[path = "daemon_scheduler_phase_exec.rs"]
mod phase_exec;
#[path = "daemon_scheduler_failover.rs"]
mod phase_failover;
#[path = "daemon_scheduler_phase_targets.rs"]
mod phase_targets;
#[path = "daemon_scheduler_project_tick.rs"]
mod project_tick_ops;

use phase_exec::{
    PhaseExecutionMetadata, PhaseExecutionOutcome, PhaseExecutionRunResult, PhaseExecutionSignal,
};
use phase_failover::PhaseFailureClassifier;
use phase_targets::PhaseTargetPlanner;

#[path = "daemon_scheduler_runtime_support.rs"]
mod runtime_support;

use runtime_support::WorkflowPhaseRuntimeSettings;
#[cfg(test)]
use runtime_support::WorkflowPipelineRuntimeRecord;
#[cfg(test)]
use runtime_support::WorkflowRuntimeConfigLite;

#[derive(Debug, Clone)]
pub(super) struct ProjectTickSummary {
    pub(super) project_root: String,
    pub(super) started_daemon: bool,
    pub(super) health: Value,
    pub(super) tasks_total: usize,
    pub(super) tasks_ready: usize,
    pub(super) tasks_in_progress: usize,
    pub(super) tasks_blocked: usize,
    pub(super) tasks_done: usize,
    pub(super) stale_in_progress_count: usize,
    pub(super) stale_in_progress_threshold_hours: u64,
    pub(super) stale_in_progress_task_ids: Vec<String>,
    pub(super) workflows_running: usize,
    pub(super) workflows_completed: usize,
    pub(super) workflows_failed: usize,
    pub(super) resumed_workflows: usize,
    pub(super) cleaned_stale_workflows: usize,
    pub(super) reconciled_stale_tasks: usize,
    pub(super) started_ready_workflows: usize,
    pub(super) executed_workflow_phases: usize,
    pub(super) failed_workflow_phases: usize,
    pub(super) phase_execution_events: Vec<PhaseExecutionEvent>,
    pub(super) requirement_lifecycle_transitions: Vec<RequirementLifecycleTransition>,
    pub(super) task_state_transitions: Vec<TaskStateTransition>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct PhaseExecutionEvent {
    pub(super) event_type: String,
    pub(super) project_root: String,
    pub(super) workflow_id: String,
    pub(super) task_id: String,
    pub(super) phase_id: String,
    pub(super) phase_mode: String,
    pub(super) metadata: PhaseExecutionMetadata,
    pub(super) payload: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct RequirementLifecycleTransition {
    pub(super) requirement_id: String,
    pub(super) requirement_title: String,
    pub(super) phase: String,
    pub(super) status: String,
    pub(super) transition_at: String,
    #[serde(default)]
    pub(super) comment: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct TaskStateTransition {
    pub(super) task_id: String,
    pub(super) from_status: String,
    pub(super) to_status: String,
    pub(super) changed_at: String,
    #[serde(default)]
    pub(super) workflow_id: Option<String>,
    #[serde(default)]
    pub(super) phase_id: Option<String>,
    #[serde(default)]
    pub(super) selection_source: Option<String>,
}

#[cfg(test)]
fn load_workflow_runtime_config(project_root: &str) -> WorkflowRuntimeConfigLite {
    runtime_support::load_workflow_runtime_config(project_root)
}

#[cfg(test)]
fn resolve_phase_runtime_settings(
    config: &WorkflowRuntimeConfigLite,
    pipeline_id: Option<&str>,
    phase_id: &str,
) -> Option<WorkflowPhaseRuntimeSettings> {
    runtime_support::resolve_phase_runtime_settings(config, pipeline_id, phase_id)
}

fn phase_timeout_secs() -> Option<u64> {
    runtime_support::phase_timeout_secs()
}

fn phase_runner_attempts() -> usize {
    runtime_support::phase_runner_attempts()
}

fn bootstrap_max_requirements() -> usize {
    runtime_support::bootstrap_max_requirements()
}

fn requirement_needs_refinement(requirement: &RequirementItem) -> bool {
    runtime_support::requirement_needs_refinement(requirement)
}

#[cfg(test)]
fn inject_codex_search_launch_flag(
    runtime_contract: &mut Value,
    tool_id: &str,
    web_search_override: Option<bool>,
) {
    runtime_support::inject_codex_search_launch_flag(runtime_contract, tool_id, web_search_override)
}

#[cfg(test)]
fn inject_codex_reasoning_effort(
    runtime_contract: &mut Value,
    tool_id: &str,
    reasoning_override: Option<&str>,
) {
    runtime_support::inject_codex_reasoning_effort(runtime_contract, tool_id, reasoning_override)
}

#[cfg(test)]
fn inject_codex_network_access(
    runtime_contract: &mut Value,
    tool_id: &str,
    network_access_override: Option<bool>,
) {
    runtime_support::inject_codex_network_access(runtime_contract, tool_id, network_access_override)
}

fn inject_cli_launch_overrides(
    runtime_contract: &mut Value,
    tool_id: &str,
    phase_runtime_settings: Option<&WorkflowPhaseRuntimeSettings>,
) {
    runtime_support::inject_cli_launch_overrides(runtime_contract, tool_id, phase_runtime_settings)
}

#[cfg(test)]
fn build_phase_prompt(
    project_root: &str,
    workflow_id: &str,
    task_id: &str,
    task_title: &str,
    task_description: &str,
    phase_id: &str,
) -> String {
    phase_exec::build_phase_prompt(
        project_root,
        workflow_id,
        task_id,
        task_title,
        task_description,
        phase_id,
    )
}

#[cfg(test)]
fn parse_commit_message_from_text(text: &str) -> Option<String> {
    phase_exec::parse_commit_message_from_text(text)
}

#[cfg(test)]
fn fallback_implementation_commit_message(task_id: &str, task_title: &str) -> String {
    phase_exec::fallback_implementation_commit_message(task_id, task_title)
}

async fn run_workflow_phase_with_agent(
    project_root: &str,
    execution_cwd: &str,
    workflow_id: &str,
    task_id: &str,
    task_title: &str,
    task_description: &str,
    task_complexity: Option<orchestrator_core::Complexity>,
    phase_id: &str,
    phase_attempt: u32,
) -> Result<PhaseExecutionRunResult> {
    phase_exec::run_workflow_phase(
        project_root,
        execution_cwd,
        workflow_id,
        task_id,
        task_title,
        task_description,
        task_complexity,
        phase_id,
        phase_attempt,
    )
    .await
}

#[cfg(test)]
async fn run_workflow_phase_with_agent_legacy(
    project_root: &str,
    execution_cwd: &str,
    workflow_id: &str,
    task_id: &str,
    task_title: &str,
    task_description: &str,
    task_complexity: Option<orchestrator_core::Complexity>,
    phase_id: &str,
    phase_runtime_settings: Option<&WorkflowPhaseRuntimeSettings>,
) -> Result<PhaseExecutionOutcome> {
    phase_exec::run_workflow_phase_with_agent(
        project_root,
        execution_cwd,
        workflow_id,
        task_id,
        task_title,
        task_description,
        task_complexity,
        phase_id,
        phase_runtime_settings,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use orchestrator_core::{
        InMemoryServiceHub, Priority, RequirementItem, RequirementPriority, RequirementStatus,
        TaskCreateInput, TaskType, VisionDraftInput,
    };
    use protocol::ModelRoutingComplexity;
    use std::sync::{Arc, Mutex, OnceLock};
    use tempfile::TempDir;

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    struct EnvVarGuard {
        key: &'static str,
        previous: Option<String>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: Option<&str>) -> Self {
            let previous = std::env::var(key).ok();
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
            Self { key, previous }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            if let Some(previous) = &self.previous {
                std::env::set_var(self.key, previous);
            } else {
                std::env::remove_var(self.key);
            }
        }
    }

    fn init_git_repo(temp: &TempDir) {
        let init_main = ProcessCommand::new("git")
            .arg("init")
            .arg("-b")
            .arg("main")
            .current_dir(temp.path())
            .status()
            .expect("git init should run");
        if !init_main.success() {
            let init = ProcessCommand::new("git")
                .arg("init")
                .current_dir(temp.path())
                .status()
                .expect("git init should run");
            assert!(init.success(), "git init should succeed");
            let rename = ProcessCommand::new("git")
                .args(["branch", "-M", "main"])
                .current_dir(temp.path())
                .status()
                .expect("git branch -M should run");
            assert!(rename.success(), "git branch -M main should succeed");
        }

        let email = ProcessCommand::new("git")
            .args(["config", "user.email", "ao-test@example.com"])
            .current_dir(temp.path())
            .status()
            .expect("git config user.email should run");
        assert!(email.success(), "git config user.email should succeed");
        let name = ProcessCommand::new("git")
            .args(["config", "user.name", "AO Test"])
            .current_dir(temp.path())
            .status()
            .expect("git config user.name should run");
        assert!(name.success(), "git config user.name should succeed");

        std::fs::write(temp.path().join("README.md"), "# test\n")
            .expect("readme should be written");
        let add = ProcessCommand::new("git")
            .args(["add", "README.md"])
            .current_dir(temp.path())
            .status()
            .expect("git add should run");
        assert!(add.success(), "git add should succeed");
        let commit = ProcessCommand::new("git")
            .args(["commit", "-m", "init"])
            .current_dir(temp.path())
            .status()
            .expect("git commit should run");
        assert!(commit.success(), "git commit should succeed");
    }

    #[test]
    fn tool_for_model_maps_known_providers() {
        assert_eq!(
            PhaseTargetPlanner::tool_for_model_id("gemini-2.5-pro"),
            "gemini"
        );
        assert_eq!(PhaseTargetPlanner::tool_for_model_id("sonnet"), "claude");
        assert_eq!(
            PhaseTargetPlanner::tool_for_model_id("claude-sonnet-4"),
            "claude"
        );
        assert_eq!(
            PhaseTargetPlanner::tool_for_model_id("opencode-x"),
            "opencode"
        );
        assert_eq!(PhaseTargetPlanner::tool_for_model_id("glm-4.5"), "opencode");
        assert_eq!(
            PhaseTargetPlanner::tool_for_model_id("minimax-m1"),
            "opencode"
        );
        assert_eq!(
            PhaseTargetPlanner::tool_for_model_id("gpt-5.3-codex"),
            "codex"
        );
    }

    #[test]
    fn phase_tool_defaults_to_model_provider() {
        let _lock = env_lock().lock().expect("env lock should be available");
        let _allow = EnvVarGuard::set("AO_ALLOW_NON_EDITING_PHASE_TOOL", Some("true"));
        let _global = EnvVarGuard::set("AO_PHASE_TOOL", None);
        let _phase = EnvVarGuard::set("AO_PHASE_TOOL_RESEARCH", None);
        let _uiux = EnvVarGuard::set("AO_PHASE_TOOL_UI_UX", None);

        let (research_tool, _) = PhaseTargetPlanner::resolve_phase_execution_target(
            "research",
            Some("gemini-2.5-pro"),
            None,
            None,
        );
        let (implementation_tool, _) = PhaseTargetPlanner::resolve_phase_execution_target(
            "implementation",
            Some("gpt-5.3-codex"),
            None,
            None,
        );
        assert_eq!(research_tool, "gemini");
        assert_eq!(implementation_tool, "codex");
    }

    #[test]
    fn phase_tool_respects_phase_override_precedence() {
        let _lock = env_lock().lock().expect("env lock should be available");
        let _allow = EnvVarGuard::set("AO_ALLOW_NON_EDITING_PHASE_TOOL", Some("true"));
        let _global = EnvVarGuard::set("AO_PHASE_TOOL", Some("claude"));
        let _research = EnvVarGuard::set("AO_PHASE_TOOL_RESEARCH", Some("gemini"));
        let _specific = EnvVarGuard::set("AO_PHASE_TOOL_IMPLEMENTATION", Some("opencode"));

        let (research_tool, _) = PhaseTargetPlanner::resolve_phase_execution_target(
            "research",
            Some("gpt-5.3-codex"),
            None,
            None,
        );
        let (implementation_tool, _) = PhaseTargetPlanner::resolve_phase_execution_target(
            "implementation",
            Some("gpt-5.3-codex"),
            None,
            None,
        );
        assert_eq!(research_tool, "gemini");
        assert_eq!(implementation_tool, "opencode");
    }

    #[test]
    fn resolve_phase_execution_target_falls_back_to_write_capable_tool() {
        let _lock = env_lock().lock().expect("env lock should be available");
        let _allow = EnvVarGuard::set("AO_ALLOW_NON_EDITING_PHASE_TOOL", None);
        let _phase_model = EnvVarGuard::set("AO_PHASE_MODEL_UI_UX", Some("gemini-2.5-pro"));
        let _fallback_model = EnvVarGuard::set("AO_PHASE_MODEL_FILE_EDIT", None);
        let _fallback_tool = EnvVarGuard::set("AO_PHASE_TOOL_FILE_EDIT", None);

        let (tool, model) =
            PhaseTargetPlanner::resolve_phase_execution_target("ux-research", None, None, None);
        assert_eq!(tool, "claude");
        assert_eq!(model, "claude-sonnet-4-6");
    }

    #[test]
    fn resolve_phase_execution_target_can_allow_non_editing_tool() {
        let _lock = env_lock().lock().expect("env lock should be available");
        let _allow = EnvVarGuard::set("AO_ALLOW_NON_EDITING_PHASE_TOOL", Some("true"));
        let _phase_model = EnvVarGuard::set("AO_PHASE_MODEL_UI_UX", Some("gemini-2.5-pro"));
        let _fallback_model = EnvVarGuard::set("AO_PHASE_MODEL_FILE_EDIT", None);
        let _fallback_tool = EnvVarGuard::set("AO_PHASE_TOOL_FILE_EDIT", None);

        let (tool, model) =
            PhaseTargetPlanner::resolve_phase_execution_target("ux-research", None, None, None);
        assert_eq!(tool, "gemini");
        assert_eq!(model, "gemini-2.5-pro");
    }

    #[test]
    fn resolve_phase_execution_target_prefers_runtime_overrides() {
        let _lock = env_lock().lock().expect("env lock should be available");
        let _allow = EnvVarGuard::set("AO_ALLOW_NON_EDITING_PHASE_TOOL", Some("true"));
        let (tool, model) = PhaseTargetPlanner::resolve_phase_execution_target(
            "implementation",
            Some("gpt-5.3-codex"),
            Some("codex"),
            None,
        );
        assert_eq!(tool, "codex");
        assert_eq!(model, "gpt-5.3-codex");
    }

    #[test]
    fn resolve_phase_execution_target_uses_complexity_for_review_model() {
        let _lock = env_lock().lock().expect("env lock should be available");
        let _allow = EnvVarGuard::set("AO_ALLOW_NON_EDITING_PHASE_TOOL", Some("true"));
        let _phase_model = EnvVarGuard::set("AO_PHASE_MODEL_CODE_REVIEW", None);
        let _global_model = EnvVarGuard::set("AO_PHASE_MODEL", None);

        let (_tool_medium, model_medium) = PhaseTargetPlanner::resolve_phase_execution_target(
            "code-review",
            None,
            None,
            Some(ModelRoutingComplexity::Medium),
        );
        let (_tool_high, model_high) = PhaseTargetPlanner::resolve_phase_execution_target(
            "code-review",
            None,
            None,
            Some(ModelRoutingComplexity::High),
        );

        assert_eq!(model_medium, "claude-sonnet-4-6");
        assert_eq!(model_high, "claude-opus-4-6");
    }

    #[test]
    fn bootstrap_max_requirements_defaults_to_uncapped() {
        let _lock = env_lock().lock().expect("env lock should be available");
        let _max_requirements = EnvVarGuard::set("AO_BOOTSTRAP_MAX_REQUIREMENTS", None);
        assert_eq!(bootstrap_max_requirements(), usize::MAX);
    }

    #[test]
    fn bootstrap_max_requirements_accepts_explicit_cap() {
        let _lock = env_lock().lock().expect("env lock should be available");
        let _max_requirements = EnvVarGuard::set("AO_BOOTSTRAP_MAX_REQUIREMENTS", Some("12"));
        assert_eq!(bootstrap_max_requirements(), 12);
    }

    #[test]
    fn phase_runner_attempts_defaults_and_clamps() {
        let _lock = env_lock().lock().expect("env lock should be available");

        {
            let _attempts = EnvVarGuard::set("AO_PHASE_RUN_ATTEMPTS", None);
            assert_eq!(phase_runner_attempts(), 3);
        }

        {
            let _attempts = EnvVarGuard::set("AO_PHASE_RUN_ATTEMPTS", Some("0"));
            assert_eq!(phase_runner_attempts(), 1);
        }

        {
            let _attempts = EnvVarGuard::set("AO_PHASE_RUN_ATTEMPTS", Some("6"));
            assert_eq!(phase_runner_attempts(), 6);
        }

        {
            let _attempts = EnvVarGuard::set("AO_PHASE_RUN_ATTEMPTS", Some("99"));
            assert_eq!(phase_runner_attempts(), 10);
        }
    }

    #[test]
    fn transient_runner_error_detection_matches_connection_failures() {
        assert!(PhaseFailureClassifier::is_transient_runner_error_message(
            "failed to connect runner for workflow wf-1 phase implementation",
        ));
        assert!(PhaseFailureClassifier::is_transient_runner_error_message(
            "runner disconnected before workflow wf-1 phase testing completed",
        ));
        assert!(PhaseFailureClassifier::is_transient_runner_error_message(
            "workflow wf-1 phase implementation error: connection reset by peer",
        ));
        assert!(PhaseFailureClassifier::is_transient_runner_error_message(
            "workflow wf-1 phase code-review timed out",
        ));
    }

    #[test]
    fn transient_runner_error_detection_ignores_non_transient_errors() {
        assert!(!PhaseFailureClassifier::is_transient_runner_error_message(
            "workflow wf-1 phase implementation exited with code Some(2)",
        ));
        assert!(!PhaseFailureClassifier::is_transient_runner_error_message(
            "workflow wf-1 cannot load task TASK-001: not found",
        ));
    }

    #[test]
    fn provider_exhaustion_reason_detects_codex_token_payload() {
        let text = r#"{"event_msg":{"token_count":{"secondary":{"used_percent":100},"credits":{"has_credits":false,"balance":"0"}}}}"#;
        let reason = PhaseFailureClassifier::provider_exhaustion_reason_from_text(text)
            .expect("provider exhaustion should parse");
        assert!(reason.contains("secondary token budget exhausted") || reason.contains("credits"));
    }

    #[test]
    fn should_failover_target_for_quota_or_tool_unavailable_errors() {
        assert!(PhaseFailureClassifier::should_failover_target(
            "workflow wf-1 phase implementation exited with code Some(1) (provider_exhausted: provider quota exceeded)",
        ));
        assert!(PhaseFailureClassifier::should_failover_target(
            "workflow wf-1 phase implementation error: Process execution failed: Failed to spawn CLI process 'claude'",
        ));
        assert!(!PhaseFailureClassifier::should_failover_target(
            "workflow wf-1 phase implementation failed lint checks",
        ));
    }

    #[test]
    fn build_phase_execution_targets_respects_fallback_models() {
        let _lock = env_lock().lock().expect("env lock should be available");
        let _allow = EnvVarGuard::set("AO_ALLOW_NON_EDITING_PHASE_TOOL", Some("true"));
        let _global_fallback = EnvVarGuard::set(
            "AO_PHASE_FALLBACK_MODELS",
            Some("sonnet,glm-4.5,minimax-m1"),
        );

        let targets = PhaseTargetPlanner::build_phase_execution_targets(
            "implementation",
            None,
            None,
            &[],
            None,
        );
        let models: Vec<_> = targets
            .iter()
            .map(|(_tool, model)| model.as_str())
            .collect();
        assert!(models.contains(&"gpt-5.3-codex"));
        assert!(models.contains(&"claude-sonnet-4-6"));
        assert!(models.contains(&"glm-4.5"));
        assert!(models.contains(&"minimax-m1"));
    }

    #[test]
    fn parse_commit_message_from_text_extracts_contract_payload() {
        let text = r#"{"kind":"implementation_result","commit_message":"feat(task-123): add image API contract"}"#;
        let message =
            parse_commit_message_from_text(text).expect("commit message should be parsed");
        assert_eq!(message, "feat(task-123): add image API contract");
    }

    #[test]
    fn parse_commit_message_from_text_extracts_payload_from_nested_result() {
        let text = r#"{"type":"result","result":"{\"kind\":\"implementation_result\",\"commit_message\":\"feat(task-123): nested payload\"}"}"#;
        let message =
            parse_commit_message_from_text(text).expect("nested commit message should be parsed");
        assert_eq!(message, "feat(task-123): nested payload");
    }

    #[test]
    fn parse_commit_message_from_text_extracts_payload_from_array_envelope() {
        let text = r#"[{"type":"system"},{"type":"result","result":"{\"kind\":\"implementation_result\",\"commit_message\":\"feat(task-123): array envelope\"}"}]"#;
        let message =
            parse_commit_message_from_text(text).expect("array envelope commit should be parsed");
        assert_eq!(message, "feat(task-123): array envelope");
    }

    #[test]
    fn parse_commit_message_from_text_ignores_non_contract_payload() {
        let text = r#"{"kind":"summary","message":"done"}"#;
        assert!(parse_commit_message_from_text(text).is_none());
    }

    #[test]
    fn fallback_implementation_commit_message_uses_task_context() {
        let message = fallback_implementation_commit_message(
            "TASK-123",
            "Implement   image   upload endpoint",
        );
        assert_eq!(message, "feat(task-123): Implement image upload endpoint");
    }

    #[test]
    fn fallback_implementation_commit_message_handles_empty_values() {
        let scoped = fallback_implementation_commit_message("TASK-123", "");
        assert_eq!(scoped, "feat(task-123): apply implementation changes");

        let unscoped = fallback_implementation_commit_message("", "");
        assert_eq!(unscoped, "feat: apply implementation changes");
    }

    #[test]
    fn implementation_prompt_requires_commit_message_payload() {
        let prompt = build_phase_prompt(
            "/tmp/project",
            "wf-1",
            "TASK-001",
            "Implement image endpoint",
            "Build API",
            "implementation",
        );
        assert!(prompt.contains("implementation_result"));
        assert!(prompt.contains("commit_message"));

        let requirements_prompt = build_phase_prompt(
            "/tmp/project",
            "wf-1",
            "TASK-001",
            "Implement image endpoint",
            "Build API",
            "requirements",
        );
        assert!(!requirements_prompt.contains("implementation_result"));
    }

    #[test]
    fn research_prompt_includes_greenfield_safety_rules() {
        let prompt = build_phase_prompt(
            "/tmp/project",
            "wf-1",
            "TASK-001",
            "Research receipt capture",
            "Investigate architecture constraints",
            "research",
        );

        assert!(prompt.contains("greenfield repositories as valid"));
        assert!(prompt.contains("missing app source files is not a blocker by itself"));
        assert!(prompt.contains("Emit `research_required` only for true external blockers"));
        assert!(prompt.contains("Do targeted discovery only"));
        assert!(prompt.contains("skip `node_modules/`"));
        assert!(prompt.contains("`.ao/workflow-state/checkpoints/`"));
        assert!(prompt.contains("`.ao/runs/`"));

        let implementation_prompt = build_phase_prompt(
            "/tmp/project",
            "wf-1",
            "TASK-001",
            "Implement endpoint",
            "Build API route",
            "implementation",
        );
        assert!(!implementation_prompt.contains("greenfield repositories as valid"));
    }

    #[test]
    fn inject_codex_search_launch_flag_enabled_by_default() {
        let _lock = env_lock().lock().expect("env lock should be available");
        let _search = EnvVarGuard::set("AO_CODEX_WEB_SEARCH", None);
        let mut contract = build_runtime_contract("codex", "gpt-5.3-codex", "hello")
            .expect("runtime contract should build");
        inject_codex_search_launch_flag(&mut contract, "codex", None);
        let args = contract
            .pointer("/cli/launch/args")
            .and_then(Value::as_array)
            .expect("launch args should be present");
        assert_eq!(args.first().and_then(Value::as_str), Some("--search"));
        assert!(args
            .iter()
            .any(|item| item.as_str().is_some_and(|value| value == "--search")));
        assert_eq!(
            contract
                .pointer("/cli/capabilities/supports_web_search")
                .and_then(Value::as_bool),
            Some(true)
        );
    }

    #[test]
    fn inject_codex_search_launch_flag_respects_disable_toggle() {
        let _lock = env_lock().lock().expect("env lock should be available");
        let _search = EnvVarGuard::set("AO_CODEX_WEB_SEARCH", Some("false"));
        let mut contract = build_runtime_contract("codex", "gpt-5.3-codex", "hello")
            .expect("runtime contract should build");
        inject_codex_search_launch_flag(&mut contract, "codex", None);
        let args = contract
            .pointer("/cli/launch/args")
            .and_then(Value::as_array)
            .expect("launch args should be present");
        assert!(!args
            .iter()
            .any(|item| item.as_str().is_some_and(|value| value == "--search")));
    }

    #[test]
    fn inject_codex_reasoning_effort_override() {
        let _lock = env_lock().lock().expect("env lock should be available");
        let _effort = EnvVarGuard::set("AO_CODEX_REASONING_EFFORT", Some("xhigh"));
        let mut contract = build_runtime_contract("codex", "gpt-5.3-codex", "hello")
            .expect("runtime contract should build");
        inject_codex_reasoning_effort(&mut contract, "codex", None);
        let args = contract
            .pointer("/cli/launch/args")
            .and_then(Value::as_array)
            .expect("launch args should be present")
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>();
        assert!(args.contains(&"-c"));
        assert!(args.contains(&"model_reasoning_effort=\"xhigh\""));
    }

    #[test]
    fn inject_codex_reasoning_effort_does_not_duplicate_existing_override() {
        let _lock = env_lock().lock().expect("env lock should be available");
        let _effort = EnvVarGuard::set("AO_CODEX_REASONING_EFFORT", Some("xhigh"));
        let mut contract = build_runtime_contract("codex", "gpt-5.3-codex", "hello")
            .expect("runtime contract should build");
        inject_codex_reasoning_effort(&mut contract, "codex", None);
        inject_codex_reasoning_effort(&mut contract, "codex", None);
        let args = contract
            .pointer("/cli/launch/args")
            .and_then(Value::as_array)
            .expect("launch args should be present")
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>();
        let overrides = args
            .iter()
            .filter(|value| value.starts_with("model_reasoning_effort="))
            .count();
        assert_eq!(overrides, 1);
    }

    #[test]
    fn inject_codex_network_access_enabled_by_default() {
        let _lock = env_lock().lock().expect("env lock should be available");
        let _network = EnvVarGuard::set("AO_CODEX_NETWORK_ACCESS", None);
        let mut contract = build_runtime_contract("codex", "gpt-5.3-codex", "hello")
            .expect("runtime contract should build");
        inject_codex_network_access(&mut contract, "codex", None);
        let args = contract
            .pointer("/cli/launch/args")
            .and_then(Value::as_array)
            .expect("launch args should be present")
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>();
        assert!(args.contains(&"-c"));
        assert!(args.contains(&"sandbox_workspace_write.network_access=true"));
    }

    #[test]
    fn inject_codex_network_access_respects_disable_toggle() {
        let _lock = env_lock().lock().expect("env lock should be available");
        let _network = EnvVarGuard::set("AO_CODEX_NETWORK_ACCESS", Some("false"));
        let mut contract = build_runtime_contract("codex", "gpt-5.3-codex", "hello")
            .expect("runtime contract should build");
        inject_codex_network_access(&mut contract, "codex", None);
        let args = contract
            .pointer("/cli/launch/args")
            .and_then(Value::as_array)
            .expect("launch args should be present")
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>();
        assert!(args.contains(&"sandbox_workspace_write.network_access=false"));
    }

    #[test]
    fn inject_cli_launch_overrides_disables_claude_bypass_permissions_by_default() {
        let _lock = env_lock().lock().expect("env lock should be available");
        let _bypass = EnvVarGuard::set("AO_CLAUDE_BYPASS_PERMISSIONS", None);
        let mut contract = build_runtime_contract("claude", "claude-opus-4-1", "hello")
            .expect("runtime contract should build");
        inject_cli_launch_overrides(&mut contract, "claude", None);
        let args = contract
            .pointer("/cli/launch/args")
            .and_then(Value::as_array)
            .expect("launch args should be present")
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>();
        assert!(!args.contains(&"--permission-mode"));
        assert!(!args.contains(&"bypassPermissions"));
    }

    #[test]
    fn inject_cli_launch_overrides_respects_claude_bypass_enable_toggle() {
        let _lock = env_lock().lock().expect("env lock should be available");
        let _bypass = EnvVarGuard::set("AO_CLAUDE_BYPASS_PERMISSIONS", Some("true"));
        let mut contract = build_runtime_contract("claude", "claude-opus-4-1", "hello")
            .expect("runtime contract should build");
        inject_cli_launch_overrides(&mut contract, "claude", None);
        let args = contract
            .pointer("/cli/launch/args")
            .and_then(Value::as_array)
            .expect("launch args should be present")
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>();
        assert!(args.contains(&"--permission-mode"));
        assert!(args.contains(&"bypassPermissions"));
    }

    #[test]
    fn inject_cli_launch_overrides_respects_claude_bypass_disable_toggle() {
        let _lock = env_lock().lock().expect("env lock should be available");
        let _bypass = EnvVarGuard::set("AO_CLAUDE_BYPASS_PERMISSIONS", Some("false"));
        let mut contract = build_runtime_contract("claude", "claude-opus-4-1", "hello")
            .expect("runtime contract should build");
        inject_cli_launch_overrides(&mut contract, "claude", None);
        let args = contract
            .pointer("/cli/launch/args")
            .and_then(Value::as_array)
            .expect("launch args should be present")
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>();
        assert!(!args.contains(&"--permission-mode"));
        assert!(!args.contains(&"bypassPermissions"));
    }

    #[test]
    fn inject_cli_launch_overrides_treats_empty_claude_bypass_value_as_disabled() {
        let _lock = env_lock().lock().expect("env lock should be available");
        let _bypass = EnvVarGuard::set("AO_CLAUDE_BYPASS_PERMISSIONS", Some(""));
        let mut contract = build_runtime_contract("claude", "claude-opus-4-1", "hello")
            .expect("runtime contract should build");
        inject_cli_launch_overrides(&mut contract, "claude", None);
        let args = contract
            .pointer("/cli/launch/args")
            .and_then(Value::as_array)
            .expect("launch args should be present")
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>();
        assert!(!args.contains(&"--permission-mode"));
        assert!(!args.contains(&"bypassPermissions"));
    }

    #[test]
    fn inject_cli_launch_overrides_applies_claude_extra_args_from_env() {
        let _lock = env_lock().lock().expect("env lock should be available");
        let _extra_args = EnvVarGuard::set(
            "AO_CLAUDE_EXTRA_ARGS_JSON",
            Some("[\"--max-turns\", \"2\"]"),
        );
        let mut contract = build_runtime_contract("claude", "claude-opus-4-1", "hello")
            .expect("runtime contract should build");
        inject_cli_launch_overrides(&mut contract, "claude", None);
        let args = contract
            .pointer("/cli/launch/args")
            .and_then(Value::as_array)
            .expect("launch args should be present")
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>();
        assert!(args.contains(&"--max-turns"));
        assert!(args.contains(&"2"));
    }

    #[test]
    fn inject_cli_launch_overrides_applies_codex_config_and_extra_args_from_phase_settings() {
        let mut contract = build_runtime_contract("codex", "gpt-5.3-codex", "hello")
            .expect("runtime contract should build");
        let runtime_settings = WorkflowPhaseRuntimeSettings {
            tool: None,
            model: None,
            fallback_models: vec![],
            reasoning_effort: None,
            web_search: None,
            network_access: Some(true),
            timeout_secs: None,
            max_attempts: None,
            extra_args: vec!["--search".to_string()],
            codex_config_overrides: vec![
                "sandbox_workspace_write.network_access=false".to_string(),
                "model_reasoning_effort=\"high\"".to_string(),
            ],
        };
        inject_cli_launch_overrides(&mut contract, "codex", Some(&runtime_settings));
        let args = contract
            .pointer("/cli/launch/args")
            .and_then(Value::as_array)
            .expect("launch args should be present")
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>();
        assert!(args.contains(&"--search"));
        assert!(args.contains(&"sandbox_workspace_write.network_access=false"));
        assert!(args.contains(&"model_reasoning_effort=\"high\""));
    }

    #[test]
    fn inject_cli_launch_overrides_applies_gemini_extra_args_from_phase_settings() {
        let mut contract = build_runtime_contract("gemini", "gemini-2.5-pro", "hello")
            .expect("runtime contract should build");
        let runtime_settings = WorkflowPhaseRuntimeSettings {
            tool: None,
            model: None,
            fallback_models: vec![],
            reasoning_effort: None,
            web_search: None,
            network_access: None,
            timeout_secs: None,
            max_attempts: None,
            extra_args: vec!["--experimental-foo".to_string(), "on".to_string()],
            codex_config_overrides: vec![],
        };
        inject_cli_launch_overrides(&mut contract, "gemini", Some(&runtime_settings));
        let args = contract
            .pointer("/cli/launch/args")
            .and_then(Value::as_array)
            .expect("launch args should be present")
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>();
        assert!(args.contains(&"--experimental-foo"));
        assert!(args.contains(&"on"));
    }

    #[test]
    fn phase_runtime_settings_lookup_matches_pipeline_and_phase() {
        let config = WorkflowRuntimeConfigLite {
            default_pipeline_id: "standard".to_string(),
            pipelines: vec![WorkflowPipelineRuntimeRecord {
                id: "ui-ux-standard".to_string(),
                phase_settings: std::collections::HashMap::from([(
                    "implementation".to_string(),
                    WorkflowPhaseRuntimeSettings {
                        tool: Some("codex".to_string()),
                        model: Some("gpt-5.3-codex".to_string()),
                        fallback_models: Vec::new(),
                        reasoning_effort: Some("xhigh".to_string()),
                        web_search: Some(true),
                        network_access: None,
                        timeout_secs: None,
                        max_attempts: None,
                        extra_args: vec![],
                        codex_config_overrides: vec![],
                    },
                )]),
            }],
        };

        let settings =
            resolve_phase_runtime_settings(&config, Some("ui-ux-standard"), "implementation")
                .expect("expected phase settings");
        assert_eq!(settings.reasoning_effort.as_deref(), Some("xhigh"));
        assert_eq!(settings.web_search, Some(true));
    }

    #[tokio::test]
    async fn bootstrap_from_vision_materializes_requirements_and_tasks() {
        let _lock = env_lock().lock().expect("env lock should be available");
        let _max_requirements = EnvVarGuard::set("AO_BOOTSTRAP_MAX_REQUIREMENTS", Some("4"));
        let hub = Arc::new(InMemoryServiceHub::new());
        hub.planning()
            .draft_vision(VisionDraftInput {
                project_name: Some("Bootstrap Test".to_string()),
                problem_statement: "Need a daemon-owned planning bootstrap".to_string(),
                target_users: vec!["Engineers".to_string()],
                goals: vec![
                    "Generate requirements and tasks from vision".to_string(),
                    "Run QA and review gates autonomously".to_string(),
                    "Ship production-ready increments".to_string(),
                ],
                constraints: vec!["No manual planning commands".to_string()],
                value_proposition: Some("Vision-only kickoff".to_string()),
                complexity_assessment: None,
            })
            .await
            .expect("vision should be drafted");

        assert!(hub
            .planning()
            .list_requirements()
            .await
            .expect("requirements list")
            .is_empty());
        assert!(hub.tasks().list().await.expect("task list").is_empty());

        bootstrap_from_vision_if_needed(hub.clone() as Arc<dyn ServiceHub>, false, false)
            .await
            .expect("bootstrap should succeed");

        let requirements = hub
            .planning()
            .list_requirements()
            .await
            .expect("requirements list");
        let materialized_requirements: Vec<&RequirementItem> = requirements
            .iter()
            .filter(|requirement| !requirement.source.eq_ignore_ascii_case("baseline"))
            .collect();
        assert!(!requirements.is_empty());
        assert!(requirements.len() >= 3);
        assert!(!materialized_requirements.is_empty());
        assert!(materialized_requirements
            .iter()
            .all(|requirement| requirement.status != RequirementStatus::Draft));
        assert!(materialized_requirements
            .iter()
            .all(|requirement| !requirement.linked_task_ids.is_empty()));
        assert!(requirements
            .iter()
            .any(|requirement| requirement
                .acceptance_criteria
                .iter()
                .any(|criterion| criterion
                    .to_ascii_lowercase()
                    .contains("automated test coverage"))));
        assert!(!hub.tasks().list().await.expect("task list").is_empty());
    }

    #[tokio::test]
    async fn dependency_gate_blocks_and_unblocks_tasks() {
        let hub = Arc::new(InMemoryServiceHub::new());

        let dependency = hub
            .tasks()
            .create(TaskCreateInput {
                title: "dependency".to_string(),
                description: "must be done first".to_string(),
                task_type: Some(TaskType::Feature),
                priority: Some(Priority::High),
                created_by: Some("test".to_string()),
                tags: Vec::new(),
                linked_requirements: Vec::new(),
                linked_architecture_entities: Vec::new(),
            })
            .await
            .expect("dependency task should be created");

        let blocked = hub
            .tasks()
            .create(TaskCreateInput {
                title: "blocked".to_string(),
                description: "wait for dependency".to_string(),
                task_type: Some(TaskType::Feature),
                priority: Some(Priority::Medium),
                created_by: Some("test".to_string()),
                tags: Vec::new(),
                linked_requirements: Vec::new(),
                linked_architecture_entities: Vec::new(),
            })
            .await
            .expect("blocked task should be created");

        hub.tasks()
            .set_status(&dependency.id, TaskStatus::Ready)
            .await
            .expect("dependency should become ready");
        hub.tasks()
            .set_status(&blocked.id, TaskStatus::Ready)
            .await
            .expect("blocked task should become ready");
        hub.tasks()
            .add_dependency(
                &blocked.id,
                &dependency.id,
                DependencyType::BlockedBy,
                "test".to_string(),
            )
            .await
            .expect("dependency edge should be added");

        let changed = reconcile_dependency_gate_tasks_for_project(
            hub.clone() as Arc<dyn ServiceHub>,
            "/tmp/ao-test-dependency-gate",
        )
        .await
        .expect("dependency gate reconciliation should succeed");
        assert_eq!(changed, 1);

        let blocked_state = hub
            .tasks()
            .get(&blocked.id)
            .await
            .expect("blocked task should load");
        assert_eq!(blocked_state.status, TaskStatus::Blocked);
        assert!(blocked_state
            .blocked_reason
            .unwrap_or_default()
            .starts_with(DEPENDENCY_GATE_PREFIX));

        hub.tasks()
            .set_status(&dependency.id, TaskStatus::Done)
            .await
            .expect("dependency should become done");

        let changed = reconcile_dependency_gate_tasks_for_project(
            hub.clone() as Arc<dyn ServiceHub>,
            "/tmp/ao-test-dependency-gate",
        )
        .await
        .expect("dependency gate reconciliation should succeed");
        assert_eq!(changed, 1);

        let unblocked_state = hub
            .tasks()
            .get(&blocked.id)
            .await
            .expect("blocked task should load");
        assert_eq!(unblocked_state.status, TaskStatus::Ready);
        assert!(!unblocked_state.paused);
        assert!(unblocked_state.blocked_reason.is_none());
    }

    #[tokio::test]
    async fn run_ready_does_not_start_backlog_tasks() {
        let hub = Arc::new(InMemoryServiceHub::new());
        let task = hub
            .tasks()
            .create(TaskCreateInput {
                title: "backlog-only".to_string(),
                description: "should not auto-start".to_string(),
                task_type: Some(TaskType::Feature),
                priority: Some(Priority::Low),
                created_by: Some("test".to_string()),
                tags: Vec::new(),
                linked_requirements: Vec::new(),
                linked_architecture_entities: Vec::new(),
            })
            .await
            .expect("task should be created");

        assert_eq!(task.status, TaskStatus::Backlog);
        let started = run_ready_task_workflows_for_project(
            hub.clone() as Arc<dyn ServiceHub>,
            "/tmp/ao-test-ready-only",
            5,
        )
        .await
        .expect("ready runner should succeed");
        assert_eq!(started, 0);
        assert!(hub
            .workflows()
            .list()
            .await
            .expect("workflow list should load")
            .is_empty());
    }

    #[tokio::test]
    async fn run_ready_assigns_started_task_to_daemon_agent() {
        let hub = Arc::new(InMemoryServiceHub::new());
        let task = hub
            .tasks()
            .create(TaskCreateInput {
                title: "ready-assignee".to_string(),
                description: "should auto-assign to daemon-selected agent".to_string(),
                task_type: Some(TaskType::Feature),
                priority: Some(Priority::Medium),
                created_by: Some("test".to_string()),
                tags: Vec::new(),
                linked_requirements: Vec::new(),
                linked_architecture_entities: Vec::new(),
            })
            .await
            .expect("task should be created");
        hub.tasks()
            .set_status(&task.id, TaskStatus::Ready)
            .await
            .expect("task should be ready");

        let project_root = TempDir::new().expect("temp dir should be created");
        let project_root_path = project_root.path().to_string_lossy().to_string();
        let started = run_ready_task_workflows_for_project(
            hub.clone() as Arc<dyn ServiceHub>,
            &project_root_path,
            5,
        )
        .await
        .expect("ready runner should succeed");
        assert_eq!(started, 1);

        let started_workflow = hub
            .workflows()
            .list()
            .await
            .expect("workflow list should load")
            .into_iter()
            .find(|workflow| workflow.task_id == task.id)
            .expect("workflow should exist for task");
        let phase_id = started_workflow
            .current_phase
            .clone()
            .or_else(|| {
                started_workflow
                    .phases
                    .get(started_workflow.current_phase_index)
                    .map(|phase| phase.phase_id.clone())
            })
            .unwrap_or_else(|| "unknown".to_string());
        let runtime_config =
            orchestrator_core::load_agent_runtime_config_or_default(Path::new(&project_root_path));
        let expected_role = runtime_config
            .phase_agent_id(&phase_id)
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| phase_id.clone());

        let task_state = hub.tasks().get(&task.id).await.expect("task should load");
        assert_eq!(task_state.status, TaskStatus::InProgress);
        match task_state.assignee {
            orchestrator_core::Assignee::Agent { role, model } => {
                assert_eq!(role, expected_role);
                assert!(model
                    .as_deref()
                    .is_some_and(|value| !value.trim().is_empty()));
            }
            other => panic!("expected agent assignee, found {other:?}"),
        }
        assert_eq!(task_state.metadata.updated_by, "ao-daemon");
    }

    #[tokio::test]
    async fn run_ready_skips_tasks_with_completed_workflow() {
        let hub = Arc::new(InMemoryServiceHub::new());
        let task = hub
            .tasks()
            .create(TaskCreateInput {
                title: "already-completed".to_string(),
                description: "should not re-run".to_string(),
                task_type: Some(TaskType::Feature),
                priority: Some(Priority::Medium),
                created_by: Some("test".to_string()),
                tags: Vec::new(),
                linked_requirements: Vec::new(),
                linked_architecture_entities: Vec::new(),
            })
            .await
            .expect("task should be created");

        let workflow = hub
            .workflows()
            .run(WorkflowRunInput {
                task_id: task.id.clone(),
                pipeline_id: None,
            })
            .await
            .expect("workflow should start");

        let mut workflow_state = workflow;
        for _ in 0..12 {
            if workflow_state.status == WorkflowStatus::Completed {
                break;
            }
            workflow_state = hub
                .workflows()
                .complete_current_phase(&workflow_state.id)
                .await
                .expect("phase completion should succeed");
        }
        assert_eq!(workflow_state.status, WorkflowStatus::Completed);

        hub.tasks()
            .set_status(&task.id, TaskStatus::Ready)
            .await
            .expect("task should be forced to ready");

        let started = run_ready_task_workflows_for_project(
            hub.clone() as Arc<dyn ServiceHub>,
            "/tmp/ao-test-ready-skip-completed",
            5,
        )
        .await
        .expect("ready runner should succeed");
        assert_eq!(started, 0);

        let task_state = hub.tasks().get(&task.id).await.expect("task should load");
        assert_eq!(task_state.status, TaskStatus::Done);

        let workflows = hub
            .workflows()
            .list()
            .await
            .expect("workflow list should load");
        assert_eq!(workflows.len(), 1);
        assert_eq!(workflows[0].status, WorkflowStatus::Completed);
    }

    #[tokio::test]
    async fn run_ready_does_not_treat_merge_conflict_as_completed_workflow() {
        let hub = Arc::new(InMemoryServiceHub::new());
        let task = hub
            .tasks()
            .create(TaskCreateInput {
                title: "merge-conflict-ready".to_string(),
                description: "non-terminal merge conflict should not be auto-done".to_string(),
                task_type: Some(TaskType::Feature),
                priority: Some(Priority::Medium),
                created_by: Some("test".to_string()),
                tags: Vec::new(),
                linked_requirements: Vec::new(),
                linked_architecture_entities: Vec::new(),
            })
            .await
            .expect("task should be created");

        let workflow = hub
            .workflows()
            .run(WorkflowRunInput {
                task_id: task.id.clone(),
                pipeline_id: None,
            })
            .await
            .expect("workflow should start");

        let mut workflow_state = workflow;
        for _ in 0..12 {
            if workflow_state.status == WorkflowStatus::Completed {
                break;
            }
            workflow_state = hub
                .workflows()
                .complete_current_phase(&workflow_state.id)
                .await
                .expect("phase completion should succeed");
        }
        assert_eq!(workflow_state.status, WorkflowStatus::Completed);
        assert!(workflow_state.completed_at.is_some());

        let conflicted = hub
            .workflows()
            .mark_merge_conflict(
                &workflow_state.id,
                "failed to merge source branch into target branch".to_string(),
            )
            .await
            .expect("workflow should enter merge-conflict state");
        assert_eq!(conflicted.status, WorkflowStatus::Completed);
        assert_eq!(
            conflicted.machine_state,
            orchestrator_core::WorkflowMachineState::MergeConflict
        );
        assert!(conflicted.completed_at.is_none());

        hub.tasks()
            .set_status(&task.id, TaskStatus::Ready)
            .await
            .expect("task should become ready");

        let started = run_ready_task_workflows_for_project(
            hub.clone() as Arc<dyn ServiceHub>,
            "/tmp/ao-test-ready-merge-conflict",
            5,
        )
        .await
        .expect("ready runner should succeed");
        assert_eq!(started, 1);

        let task_state = hub.tasks().get(&task.id).await.expect("task should load");
        assert_eq!(task_state.status, TaskStatus::InProgress);

        let workflows = hub
            .workflows()
            .list()
            .await
            .expect("workflow list should load");
        assert_eq!(workflows.len(), 2);
        assert!(workflows
            .iter()
            .any(|workflow| workflow.id == conflicted.id));
    }

    #[tokio::test]
    async fn reconcile_stale_in_progress_marks_completed_tasks_done() {
        let hub = Arc::new(InMemoryServiceHub::new());
        let task = hub
            .tasks()
            .create(TaskCreateInput {
                title: "stale-in-progress".to_string(),
                description: "should reconcile to done".to_string(),
                task_type: Some(TaskType::Feature),
                priority: Some(Priority::Medium),
                created_by: Some("test".to_string()),
                tags: Vec::new(),
                linked_requirements: Vec::new(),
                linked_architecture_entities: Vec::new(),
            })
            .await
            .expect("task should be created");

        let workflow = hub
            .workflows()
            .run(WorkflowRunInput {
                task_id: task.id.clone(),
                pipeline_id: None,
            })
            .await
            .expect("workflow should start");

        let mut workflow_state = workflow;
        for _ in 0..12 {
            if workflow_state.status == WorkflowStatus::Completed {
                break;
            }
            workflow_state = hub
                .workflows()
                .complete_current_phase(&workflow_state.id)
                .await
                .expect("phase completion should succeed");
        }
        assert_eq!(workflow_state.status, WorkflowStatus::Completed);

        hub.tasks()
            .set_status(&task.id, TaskStatus::InProgress)
            .await
            .expect("task should become in-progress");

        let reconciled = reconcile_stale_in_progress_tasks_for_project(
            hub.clone() as Arc<dyn ServiceHub>,
            "/tmp/ao-test-stale-reconcile",
        )
        .await
        .expect("stale reconciliation should succeed");
        assert_eq!(reconciled, 1);

        let task_state = hub.tasks().get(&task.id).await.expect("task should load");
        assert_eq!(task_state.status, TaskStatus::Done);
    }

    #[tokio::test]
    async fn reconcile_stale_in_progress_does_not_mark_merge_conflict_done() {
        let hub = Arc::new(InMemoryServiceHub::new());
        let task = hub
            .tasks()
            .create(TaskCreateInput {
                title: "merge-conflict-stale-in-progress".to_string(),
                description: "should remain non-terminal while merge conflict is unresolved"
                    .to_string(),
                task_type: Some(TaskType::Feature),
                priority: Some(Priority::Medium),
                created_by: Some("test".to_string()),
                tags: Vec::new(),
                linked_requirements: Vec::new(),
                linked_architecture_entities: Vec::new(),
            })
            .await
            .expect("task should be created");

        let workflow = hub
            .workflows()
            .run(WorkflowRunInput {
                task_id: task.id.clone(),
                pipeline_id: None,
            })
            .await
            .expect("workflow should start");

        let mut workflow_state = workflow;
        for _ in 0..12 {
            if workflow_state.status == WorkflowStatus::Completed {
                break;
            }
            workflow_state = hub
                .workflows()
                .complete_current_phase(&workflow_state.id)
                .await
                .expect("phase completion should succeed");
        }
        assert_eq!(workflow_state.status, WorkflowStatus::Completed);
        assert!(workflow_state.completed_at.is_some());

        let conflicted = hub
            .workflows()
            .mark_merge_conflict(
                &workflow_state.id,
                "failed to merge source branch into target branch".to_string(),
            )
            .await
            .expect("workflow should enter merge-conflict state");
        assert_eq!(
            conflicted.machine_state,
            orchestrator_core::WorkflowMachineState::MergeConflict
        );
        assert!(conflicted.completed_at.is_none());

        hub.tasks()
            .set_status(&task.id, TaskStatus::InProgress)
            .await
            .expect("task should become in-progress");

        let reconciled = reconcile_stale_in_progress_tasks_for_project(
            hub.clone() as Arc<dyn ServiceHub>,
            "/tmp/ao-test-stale-reconcile-merge-conflict",
        )
        .await
        .expect("stale reconciliation should succeed");
        assert_eq!(reconciled, 0);

        let task_state = hub.tasks().get(&task.id).await.expect("task should load");
        assert_eq!(task_state.status, TaskStatus::InProgress);
    }

    #[tokio::test]
    async fn ensure_task_execution_cwd_provisions_worktree_and_updates_task_metadata() {
        let temp = TempDir::new().expect("temp dir");
        init_git_repo(&temp);
        let project_root = temp.path().to_string_lossy().to_string();
        let hub = Arc::new(FileServiceHub::new(&project_root).expect("file service hub"));

        let task = hub
            .tasks()
            .create(TaskCreateInput {
                title: "worktree task".to_string(),
                description: "needs isolated execution cwd".to_string(),
                task_type: Some(TaskType::Feature),
                priority: Some(Priority::High),
                created_by: Some("test".to_string()),
                tags: Vec::new(),
                linked_requirements: Vec::new(),
                linked_architecture_entities: Vec::new(),
            })
            .await
            .expect("task should be created");

        let execution_cwd =
            ensure_task_execution_cwd(hub.clone() as Arc<dyn ServiceHub>, &project_root, &task)
                .await
                .expect("execution cwd should be provisioned");

        assert!(execution_cwd.contains("/.ao/"));
        assert!(execution_cwd.contains("/worktrees/"));
        assert!(Path::new(&execution_cwd).exists());

        let updated = hub.tasks().get(&task.id).await.expect("task should load");
        assert_eq!(
            updated.worktree_path.as_deref(),
            Some(execution_cwd.as_str())
        );
        assert!(updated
            .branch_name
            .as_deref()
            .map(|value| !value.trim().is_empty())
            .unwrap_or(false));
    }

    #[test]
    fn is_branch_merged_reports_false_for_unmerged_feature_branch() {
        let temp = TempDir::new().expect("temp dir");
        init_git_repo(&temp);
        let project_root = temp.path().to_string_lossy().to_string();

        let checkout = ProcessCommand::new("git")
            .args(["checkout", "-b", "feature/unmerged"])
            .current_dir(temp.path())
            .status()
            .expect("checkout should run");
        assert!(checkout.success(), "feature branch should be created");

        std::fs::write(temp.path().join("feature.txt"), "feature change\n")
            .expect("feature file should be written");
        let add = ProcessCommand::new("git")
            .args(["add", "feature.txt"])
            .current_dir(temp.path())
            .status()
            .expect("git add should run");
        assert!(add.success(), "git add should succeed");
        let commit = ProcessCommand::new("git")
            .args(["commit", "-m", "feature change"])
            .current_dir(temp.path())
            .status()
            .expect("git commit should run");
        assert!(commit.success(), "feature commit should succeed");

        let checkout_main = ProcessCommand::new("git")
            .args(["checkout", "main"])
            .current_dir(temp.path())
            .status()
            .expect("checkout should run");
        assert!(checkout_main.success(), "checkout main should succeed");

        let merged = is_branch_merged(&project_root, "feature/unmerged")
            .expect("branch merge check should succeed");
        assert_eq!(merged, Some(false));
    }

    #[test]
    fn is_branch_merged_reports_true_after_merge_commit() {
        let temp = TempDir::new().expect("temp dir");
        init_git_repo(&temp);
        let project_root = temp.path().to_string_lossy().to_string();

        let checkout = ProcessCommand::new("git")
            .args(["checkout", "-b", "feature/merged"])
            .current_dir(temp.path())
            .status()
            .expect("checkout should run");
        assert!(checkout.success(), "feature branch should be created");

        std::fs::write(temp.path().join("merged.txt"), "merged change\n")
            .expect("feature file should be written");
        let add = ProcessCommand::new("git")
            .args(["add", "merged.txt"])
            .current_dir(temp.path())
            .status()
            .expect("git add should run");
        assert!(add.success(), "git add should succeed");
        let commit = ProcessCommand::new("git")
            .args(["commit", "-m", "merged feature change"])
            .current_dir(temp.path())
            .status()
            .expect("git commit should run");
        assert!(commit.success(), "feature commit should succeed");

        let checkout_main = ProcessCommand::new("git")
            .args(["checkout", "main"])
            .current_dir(temp.path())
            .status()
            .expect("checkout should run");
        assert!(checkout_main.success(), "checkout main should succeed");

        let merge = ProcessCommand::new("git")
            .args(["merge", "--no-ff", "feature/merged", "-m", "merge feature"])
            .current_dir(temp.path())
            .status()
            .expect("git merge should run");
        assert!(merge.success(), "merge should succeed");

        let merged = is_branch_merged(&project_root, "feature/merged")
            .expect("branch merge check should succeed");
        assert_eq!(merged, Some(true));
    }

    #[tokio::test]
    async fn project_tick_reconciles_stale_completed_workflow_tasks() {
        let temp = TempDir::new().expect("temp dir");
        let project_root = temp.path().to_string_lossy().to_string();
        let hub = Arc::new(FileServiceHub::new(&project_root).expect("file service hub"));

        let task = hub
            .tasks()
            .create(TaskCreateInput {
                title: "stale tick task".to_string(),
                description: "should reconcile through project tick".to_string(),
                task_type: Some(TaskType::Feature),
                priority: Some(Priority::Medium),
                created_by: Some("test".to_string()),
                tags: Vec::new(),
                linked_requirements: Vec::new(),
                linked_architecture_entities: Vec::new(),
            })
            .await
            .expect("task should be created");

        let workflow = hub
            .workflows()
            .run(WorkflowRunInput {
                task_id: task.id.clone(),
                pipeline_id: None,
            })
            .await
            .expect("workflow should start");
        let mut state = workflow;
        for _ in 0..12 {
            if state.status == WorkflowStatus::Completed {
                break;
            }
            state = hub
                .workflows()
                .complete_current_phase(&state.id)
                .await
                .expect("phase completion should succeed");
        }
        assert_eq!(state.status, WorkflowStatus::Completed);

        hub.tasks()
            .set_status(&task.id, TaskStatus::InProgress)
            .await
            .expect("task should be marked stale");

        // Avoid runner bootstrap in this test: project_tick should reconcile state without
        // depending on an external agent-runner process.
        let state_path = Path::new(&project_root).join(".ao").join("core-state.json");
        let mut state_json: Value = serde_json::from_str(
            &fs::read_to_string(&state_path).expect("core state should be readable"),
        )
        .expect("core state json should parse");
        state_json["daemon_status"] = Value::String("running".to_string());
        fs::write(
            &state_path,
            serde_json::to_string_pretty(&state_json).expect("core state should serialize"),
        )
        .expect("core state should be writable");

        let args = DaemonRunArgs {
            interval_secs: 1,
            include_registry: false,
            ai_task_generation: false,
            auto_run_ready: false,
            auto_merge: None,
            auto_pr: None,
            auto_commit_before_merge: None,
            auto_prune_worktrees_after_merge: None,
            startup_cleanup: false,
            resume_interrupted: false,
            reconcile_stale: true,
            stale_threshold_hours: 24,
            max_tasks_per_tick: 2,
            phase_timeout_secs: None,
            idle_timeout_secs: None,
            once: true,
        };
        let summary = project_tick(&project_root, &args)
            .await
            .expect("project tick should succeed");
        assert!(summary.reconciled_stale_tasks >= 1);
        assert!(summary.task_state_transitions.iter().any(|transition| {
            transition.task_id == task.id
                && transition.from_status == "in-progress"
                && transition.to_status == "done"
        }));

        let refreshed_hub = Arc::new(FileServiceHub::new(&project_root).expect("refreshed hub"));
        let updated = refreshed_hub
            .tasks()
            .get(&task.id)
            .await
            .expect("task should load");
        assert_eq!(updated.status, TaskStatus::Done);
    }

    #[tokio::test]
    async fn project_tick_reports_requirement_lifecycle_transitions() {
        let _lock = env_lock().lock().expect("env lock should be available");
        let _skip_runner = EnvVarGuard::set("AO_SKIP_RUNNER_START", Some("1"));

        let temp = TempDir::new().expect("temp dir");
        let project_root = temp.path().to_string_lossy().to_string();
        let hub = Arc::new(FileServiceHub::new(&project_root).expect("file service hub"));

        hub.planning()
            .draft_vision(VisionDraftInput {
                project_name: Some("Lifecycle Events".to_string()),
                problem_statement: "Need requirement lifecycle events".to_string(),
                target_users: vec!["Product teams".to_string()],
                goals: vec!["Draft and execute requirements through review gates".to_string()],
                constraints: vec![],
                value_proposition: Some(
                    "Visibility into requirement state transitions".to_string(),
                ),
                complexity_assessment: None,
            })
            .await
            .expect("vision should be drafted");

        let now = chrono::Utc::now();
        hub.planning()
            .upsert_requirement(RequirementItem {
                id: String::new(),
                title: "Investigate provider tradeoffs for image generation".to_string(),
                description: "Investigate options and choose one.".to_string(),
                body: None,
                legacy_id: None,
                category: None,
                requirement_type: None,
                acceptance_criteria: vec!["Decision documented".to_string()],
                priority: RequirementPriority::Should,
                status: RequirementStatus::Draft,
                source: "manual".to_string(),
                tags: vec![],
                links: orchestrator_core::RequirementLinks::default(),
                comments: vec![],
                relative_path: None,
                linked_task_ids: vec![],
                created_at: now,
                updated_at: now,
            })
            .await
            .expect("requirement should be stored");

        let args = DaemonRunArgs {
            interval_secs: 1,
            include_registry: false,
            ai_task_generation: false,
            auto_run_ready: false,
            auto_merge: None,
            auto_pr: None,
            auto_commit_before_merge: None,
            auto_prune_worktrees_after_merge: None,
            startup_cleanup: false,
            resume_interrupted: false,
            reconcile_stale: false,
            stale_threshold_hours: 24,
            max_tasks_per_tick: 2,
            phase_timeout_secs: None,
            idle_timeout_secs: None,
            once: true,
        };
        let summary = project_tick(&project_root, &args)
            .await
            .expect("project tick should succeed");

        assert!(!summary.requirement_lifecycle_transitions.is_empty());
        let phases: HashSet<String> = summary
            .requirement_lifecycle_transitions
            .iter()
            .map(|transition| transition.phase.clone())
            .collect();
        assert!(phases.contains("refine"));
        assert!(phases.contains("po-review"));
        assert!(phases.contains("em-review"));
        assert!(phases.contains("approved"));
        assert!(phases.contains("rework") || phases.contains("research"));
    }
}

fn enforce_frontend_phase_gate(
    project_root: &str,
    workflow_id: &str,
    phase_id: &str,
    task: &orchestrator_core::OrchestratorTask,
) -> Result<()> {
    frontend_phase_gate::enforce_frontend_phase_gate(project_root, workflow_id, phase_id, task)
}

fn pipeline_for_task(task: &orchestrator_core::OrchestratorTask) -> String {
    if task.is_frontend_related() {
        orchestrator_core::UI_UX_PIPELINE_ID.to_string()
    } else {
        orchestrator_core::STANDARD_PIPELINE_ID.to_string()
    }
}

fn task_requires_research(task: &orchestrator_core::OrchestratorTask) -> bool {
    if task.workflow_metadata.requires_architecture {
        return true;
    }

    if task.tags.iter().any(|tag| {
        matches!(
            tag.trim().to_ascii_lowercase().as_str(),
            "needs-research" | "research" | "discovery" | "investigation" | "spike"
        )
    }) {
        return true;
    }

    let haystack = format!("{} {}", task.title, task.description).to_ascii_lowercase();
    [
        "research",
        "investigate",
        "evaluate",
        "compare",
        "benchmark",
        "unknown",
        "spike",
        "decision record",
        "validate approach",
    ]
    .iter()
    .any(|needle| haystack.contains(needle))
}

fn workflow_has_completed_research(workflow: &orchestrator_core::OrchestratorWorkflow) -> bool {
    workflow.phases.iter().any(|phase| {
        phase.phase_id == "research"
            && phase.status == orchestrator_core::WorkflowPhaseStatus::Success
    })
}

fn workflow_has_active_research(workflow: &orchestrator_core::OrchestratorWorkflow) -> bool {
    workflow.phases.iter().any(|phase| {
        phase.phase_id == "research"
            && matches!(
                phase.status,
                orchestrator_core::WorkflowPhaseStatus::Pending
                    | orchestrator_core::WorkflowPhaseStatus::Ready
                    | orchestrator_core::WorkflowPhaseStatus::Running
            )
    })
}

const DEPENDENCY_GATE_PREFIX: &str = "dependency gate:";
const MERGE_GATE_PREFIX: &str = "merge gate:";

fn commit_implementation_changes(cwd: &str, commit_message: &str) -> Result<()> {
    git_ops::commit_implementation_changes(cwd, commit_message)
}

async fn post_success_merge_push_and_cleanup(
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
    task: &orchestrator_core::OrchestratorTask,
) -> Result<git_ops::PostMergeOutcome> {
    git_ops::post_success_merge_push_and_cleanup(hub, project_root, task).await
}

async fn finalize_merge_conflict_resolution(
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
    task: &orchestrator_core::OrchestratorTask,
    context: &git_ops::MergeConflictContext,
) -> Result<()> {
    git_ops::finalize_merge_conflict_resolution(hub, project_root, task, context).await
}

fn cleanup_merge_conflict_worktree(project_root: &str, context: &git_ops::MergeConflictContext) {
    git_ops::cleanup_merge_conflict_worktree(project_root, context)
}

fn flush_git_integration_outbox(project_root: &str) -> Result<()> {
    git_ops::flush_git_integration_outbox(project_root)
}

async fn refresh_runtime_binaries_if_main_advanced(
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
    trigger: git_ops::RuntimeBinaryRefreshTrigger,
) -> git_ops::RuntimeBinaryRefreshOutcome {
    git_ops::refresh_runtime_binaries_if_main_advanced(hub, project_root, trigger).await
}

fn task_status_label(status: TaskStatus) -> &'static str {
    git_ops::task_status_label(status)
}

async fn ensure_task_execution_cwd(
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
    task: &orchestrator_core::OrchestratorTask,
) -> Result<String> {
    git_ops::ensure_task_execution_cwd(hub, project_root, task).await
}

fn is_branch_merged(project_root: &str, branch_name: &str) -> Result<Option<bool>> {
    git_ops::is_branch_merged(project_root, branch_name)
}

fn daemon_repo_runtime_root(project_root: &str) -> Result<PathBuf> {
    git_ops::daemon_repo_runtime_root(project_root)
}

fn dependency_blocked_reason(issues: &[String]) -> String {
    format!("{DEPENDENCY_GATE_PREFIX} {}", issues.join("; "))
}

fn merge_blocked_reason(branch_name: &str) -> String {
    format!("{MERGE_GATE_PREFIX} branch `{branch_name}` is not merged into default branch")
}

fn is_dependency_gate_block(task: &orchestrator_core::OrchestratorTask) -> bool {
    task.blocked_reason
        .as_deref()
        .map(|reason| reason.starts_with(DEPENDENCY_GATE_PREFIX))
        .unwrap_or(false)
}

fn is_merge_gate_block(task: &orchestrator_core::OrchestratorTask) -> bool {
    task.blocked_reason
        .as_deref()
        .map(|reason| reason.starts_with(MERGE_GATE_PREFIX))
        .unwrap_or(false)
}

#[cfg(test)]
async fn bootstrap_from_vision_if_needed(
    hub: Arc<dyn ServiceHub>,
    include_codebase_scan: bool,
    ai_task_generation: bool,
) -> Result<()> {
    project_tick_ops::bootstrap_from_vision_if_needed(
        hub,
        include_codebase_scan,
        ai_task_generation,
    )
    .await
}

#[cfg(test)]
async fn reconcile_dependency_gate_tasks_for_project(
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
) -> Result<usize> {
    project_tick_ops::reconcile_dependency_gate_tasks_for_project(hub, project_root).await
}

#[cfg(test)]
async fn reconcile_stale_in_progress_tasks_for_project(
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
) -> Result<usize> {
    project_tick_ops::reconcile_stale_in_progress_tasks_for_project(hub, project_root).await
}

#[cfg(test)]
async fn run_ready_task_workflows_for_project(
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
    max_tasks_per_tick: usize,
) -> Result<usize> {
    project_tick_ops::run_ready_task_workflows_for_project(hub, project_root, max_tasks_per_tick)
        .await
        .map(|summary| summary.started)
}

pub(super) fn subscribe_phase_completion_wake() -> tokio::sync::broadcast::Receiver<String> {
    project_tick_ops::subscribe_phase_completion_wake()
}

pub(super) fn resume_running_workflow_phase_spawns(project_root: &str) {
    project_tick_ops::resume_running_workflow_phase_spawns(project_root);
}

pub(super) fn pause_running_workflow_phase_spawns(project_root: &str) {
    project_tick_ops::pause_running_workflow_phase_spawns(project_root);
}

pub(super) fn clear_running_workflow_phase_pool(project_root: &str) {
    project_tick_ops::clear_running_workflow_phase_pool(project_root);
}

pub(super) async fn drain_running_workflow_phases_for_project(
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
    max_phases_per_tick: usize,
) -> Result<(usize, usize, Vec<PhaseExecutionEvent>)> {
    project_tick_ops::drain_running_workflow_phases_for_project(
        hub,
        project_root,
        max_phases_per_tick,
    )
    .await
}

pub(super) async fn project_tick(root: &str, args: &DaemonRunArgs) -> Result<ProjectTickSummary> {
    project_tick_ops::project_tick(root, args).await
}
