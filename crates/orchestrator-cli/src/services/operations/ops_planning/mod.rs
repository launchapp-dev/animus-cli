use std::sync::Arc;

use anyhow::Result;
use orchestrator_core::services::ServiceHub;

use crate::{print_value, VisionCommand};

pub(crate) async fn handle_vision(
    command: VisionCommand,
    hub: Arc<dyn ServiceHub>,
    _project_root: &str,
    json: bool,
) -> Result<()> {
    let planning = hub.planning();

    match command {
        VisionCommand::Get => print_value(planning.get_vision().await?, json),
    }
}
