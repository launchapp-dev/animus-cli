use std::path::Path;

use anyhow::Result;
use chrono::Utc;
use serde::Serialize;
use uuid::Uuid;

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

#[derive(Debug, Serialize)]
struct TriggerFireResult {
    trigger_id: String,
    event_id: String,
    status: String,
    message: String,
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
        TriggerCommand::Fire { trigger_id, payload } => {
            let _ = orchestrator_core::ensure_workflow_config_compiled(Path::new(project_root));
            let config = orchestrator_core::load_workflow_config_or_default(Path::new(project_root));

            // Validate the trigger exists and is a webhook type.
            let trigger = config
                .config
                .triggers
                .iter()
                .find(|t| t.id.eq_ignore_ascii_case(&trigger_id))
                .ok_or_else(|| anyhow::anyhow!("trigger '{}' not found", trigger_id))?;

            if !matches!(
                trigger.trigger_type,
                orchestrator_core::workflow_config::TriggerType::Webhook
                    | orchestrator_core::workflow_config::TriggerType::GithubWebhook
            ) {
                anyhow::bail!(
                    "trigger '{}' is of type '{:?}' — only webhook and github_webhook triggers can be fired manually",
                    trigger_id,
                    trigger.trigger_type
                );
            }

            if !trigger.enabled {
                anyhow::bail!("trigger '{}' is disabled", trigger_id);
            }

            // Parse the payload.
            let payload_value: serde_json::Value =
                serde_json::from_str(&payload).map_err(|e| anyhow::anyhow!("invalid JSON payload: {}", e))?;

            // Directly queue the event to TriggerState (bypasses HTTP server,
            // useful for local testing without running the daemon web server).
            let mut state = orchestrator_core::load_trigger_state(Path::new(project_root)).unwrap_or_default();
            let run_state = state.triggers.entry(trigger_id.clone()).or_default();

            let event_id = Uuid::new_v4().to_string();
            run_state.pending_events.push(orchestrator_core::WebhookEvent {
                event_id: event_id.clone(),
                received_at: Utc::now(),
                payload: payload_value,
            });
            orchestrator_core::save_trigger_state(Path::new(project_root), &state)?;

            let result = TriggerFireResult {
                trigger_id: trigger_id.clone(),
                event_id,
                status: "queued".to_string(),
                message: "event queued; will be dispatched on the next daemon tick".to_string(),
            };
            print_value(result, json)
        }
    }
}
