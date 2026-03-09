use std::path::Path;

use orchestrator_core::{routing_complexity_for_task, OrchestratorTask, OrchestratorWorkflow};
use orchestrator_daemon_runtime::workflow_current_phase_id;
use workflow_runner::phase_targets::PhaseTargetPlanner;

pub(crate) fn daemon_workflow_assignment(
    project_root: &str,
    workflow: &OrchestratorWorkflow,
    task: &OrchestratorTask,
) -> (String, Option<String>) {
    let phase_id = workflow_current_phase_id(workflow).unwrap_or_else(|| "unknown".to_string());
    let runtime_config =
        orchestrator_core::load_agent_runtime_config_or_default(Path::new(project_root));
    let role = runtime_config
        .phase_agent_id(&phase_id)
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| phase_id.clone());

    let fallback_models = runtime_config.phase_fallback_models(&phase_id);
    let caps = runtime_config.phase_capabilities(&phase_id);
    let execution_targets = PhaseTargetPlanner::build_phase_execution_targets(
        &phase_id,
        runtime_config.phase_model_override(&phase_id),
        runtime_config.phase_tool_override(&phase_id),
        fallback_models.as_slice(),
        routing_complexity_for_task(task),
        Some(project_root),
        &caps,
    );
    let model = execution_targets.first().map(|(_, model)| model.clone());
    (role, model)
}
