use super::*;
use orchestrator_core::services::ServiceHub;
use orchestrator_daemon_runtime::recover_orphaned_running_workflows;
use std::collections::HashSet;

pub async fn recover_orphaned_running_workflows_on_startup(
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
) -> usize {
    recover_orphaned_running_workflows(hub, project_root, &HashSet::new()).await
}
