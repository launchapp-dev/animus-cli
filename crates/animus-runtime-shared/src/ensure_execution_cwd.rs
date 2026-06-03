use anyhow::Result;
use orchestrator_core::{services::ServiceHub, subject_adapter::SubjectContext, SubjectRef};
use std::sync::Arc;

pub async fn ensure_execution_cwd(
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
    subject: &SubjectRef,
    subject_context: &SubjectContext,
) -> Result<String> {
    hub.project_adapter().ensure_execution_cwd(project_root, subject, subject_context).await
}
