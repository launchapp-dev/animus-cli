use std::path::Path;

use anyhow::Result;
use serde::Serialize;

use crate::{print_value, TriggerCommand};

#[derive(Debug, Serialize)]
struct TriggerListEntry {
    id: String,
    #[serde(rename = "type")]
    trigger_type: String,
    workflow_ref: Option<String>,
    enabled: bool,
    config: serde_json::Value,
}

pub(crate) async fn handle_trigger(command: TriggerCommand, project_root: &str, json: bool) -> Result<()> {
    match command {
        TriggerCommand::List => {
            let _ = orchestrator_core::ensure_workflow_config_compiled(Path::new(project_root));
            let config = orchestrator_core::load_workflow_config_or_default(Path::new(project_root));
            let entries: Vec<TriggerListEntry> = config
                .config
                .triggers
                .into_iter()
                .map(|t| {
                    let type_str = serde_json::to_value(&t.trigger_type)
                        .ok()
                        .and_then(|v| v.as_str().map(str::to_string))
                        .unwrap_or_else(|| format!("{:?}", t.trigger_type));
                    TriggerListEntry {
                        id: t.id,
                        trigger_type: type_str,
                        workflow_ref: t.workflow_ref,
                        enabled: t.enabled,
                        config: t.config,
                    }
                })
                .collect();
            print_value(entries, json)
        }
    }
}
