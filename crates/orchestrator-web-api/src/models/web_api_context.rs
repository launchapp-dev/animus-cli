use std::sync::Arc;

use orchestrator_core::ServiceHub;

#[derive(Clone)]
pub struct WebApiContext {
    pub hub: Arc<dyn ServiceHub>,
    pub project_root: String,
    pub app_version: String,
}
