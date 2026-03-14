use crate::cli_types::DaemonRunArgs;
use anyhow::Result;
#[cfg(test)]
use orchestrator_core::is_dependency_gate_block;
#[cfg(test)]
use orchestrator_core::DependencyType;
use orchestrator_core::{services::ServiceHub, TaskStatus};
use orchestrator_daemon_runtime::DaemonRuntimeOptions;
#[cfg(test)]
use orchestrator_daemon_runtime::{ProcessManager, ProjectTickSummary};
pub(crate) use project_tick_ops::{slim_project_tick_driver, SlimProjectTickDriver};
#[cfg(test)]
use std::fs;
#[cfg(test)]
use std::path::Path;
#[cfg(test)]
use std::process::Command as ProcessCommand;
use std::sync::Arc;
#[cfg(test)]
#[path = "daemon_scheduler_mock_runner_tests.rs"]
mod mock_runner_tests;
#[path = "daemon_scheduler_project_tick.rs"]
mod project_tick_ops;

#[cfg(test)]
use ::workflow_runner_v2::phase_failover::PhaseFailureClassifier;
#[cfg(test)]
use ::workflow_runner_v2::phase_targets::PhaseTargetPlanner;
#[cfg(test)]
use ::workflow_runner_v2::runtime_support;

#[cfg(test)]
use orchestrator_core::FileServiceHub;
#[cfg(test)]
use orchestrator_core::{WorkflowRunInput, WorkflowStatus};
#[cfg(test)]
use orchestrator_git_ops as git_ops;
#[cfg(test)]
use runtime_support::WorkflowPhaseRuntimeSettings;
#[cfg(test)]
use runtime_support::WorkflowPipelineRuntimeRecord;
#[cfg(test)]
use runtime_support::WorkflowRuntimeConfigLite;
#[cfg(test)]
use serde_json::Value;
#[cfg(test)]
fn resolve_phase_runtime_settings(
    config: &WorkflowRuntimeConfigLite,
    workflow_ref: Option<&str>,
    phase_id: &str,
) -> Option<WorkflowPhaseRuntimeSettings> {
    ::workflow_runner_v2::runtime_support::resolve_phase_runtime_settings(
        config,
        workflow_ref,
        phase_id,
    )
}

#[cfg(test)]
fn phase_runner_attempts() -> usize {
    runtime_support::phase_runner_attempts()
}

#[cfg(test)]
fn phase_max_continuations() -> usize {
    runtime_support::phase_max_continuations()
}

#[cfg(test)]
fn bootstrap_max_requirements() -> usize {
    runtime_support::bootstrap_max_requirements()
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

#[cfg(test)]
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
    ::workflow_runner_v2::build_phase_prompt(&::workflow_runner_v2::PhasePromptParams {
        project_root,
        execution_cwd: project_root,
        workflow_id,
        subject_id: task_id,
        subject_title: task_title,
        subject_description: task_description,
        phase_id,
        rework_context: None,
        pipeline_vars: None,
    })
}

#[cfg(test)]
fn parse_commit_message_from_text(text: &str) -> Option<String> {
    ::workflow_runner_v2::parse_commit_message_from_text(text)
}

#[cfg(test)]
fn fallback_implementation_commit_message(phase_id: &str, subject_title: &str) -> String {
    ::workflow_runner_v2::fallback_implementation_commit_message(phase_id, subject_title)
}

#[cfg(test)]
#[path = "daemon_scheduler_tests.rs"]
mod tests;

#[cfg(test)]
pub(super) async fn slim_project_tick(
    root: &str,
    args: &DaemonRunArgs,
    process_manager: &mut ProcessManager,
    dispatch_paused: bool,
) -> Result<ProjectTickSummary> {
    project_tick_ops::slim_daemon_tick(
        root,
        &runtime_options_from_cli(args),
        process_manager,
        dispatch_paused,
    )
    .await
}

#[cfg(test)]
pub(super) async fn slim_project_tick_at(
    root: &str,
    args: &DaemonRunArgs,
    process_manager: &mut ProcessManager,
    dispatch_paused: bool,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<ProjectTickSummary> {
    project_tick_ops::slim_daemon_tick_at(
        root,
        &runtime_options_from_cli(args),
        process_manager,
        dispatch_paused,
        now,
    )
    .await
}

pub(super) fn runtime_options_from_cli(args: &DaemonRunArgs) -> DaemonRuntimeOptions {
    DaemonRuntimeOptions {
        pool_size: args.pool_size,
        max_agents: args.max_agents,
        interval_secs: args.interval_secs,
        auto_run_ready: args.auto_run_ready,
        startup_cleanup: args.startup_cleanup,
        resume_interrupted: args.resume_interrupted,
        reconcile_stale: args.reconcile_stale,
        stale_threshold_hours: args.stale_threshold_hours,
        max_tasks_per_tick: args.max_tasks_per_tick,
        phase_timeout_secs: args.phase_timeout_secs,
        idle_timeout_secs: args.idle_timeout_secs,
        once: args.once,
    }
}
