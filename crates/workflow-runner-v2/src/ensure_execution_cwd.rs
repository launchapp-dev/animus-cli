use anyhow::Result;
use orchestrator_core::{services::ServiceHub, OrchestratorTask};
use std::sync::Arc;

pub async fn ensure_execution_cwd(
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
    task: Option<&OrchestratorTask>,
) -> Result<String> {
    hub.project_adapter().ensure_execution_cwd(project_root, task).await
}
