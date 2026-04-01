use chrono::{DateTime, Utc};
use orchestrator_core::{load_trigger_state, load_workflow_config_or_default, save_trigger_state, WebhookEvent};
use serde_json::Value;
use uuid::Uuid;

use crate::models::WebApiError;

use super::WebApiService;

impl WebApiService {
    /// Return the project root path for this service instance.
    pub fn project_root(&self) -> &str {
        &self.context.project_root
    }

    /// Enqueue a webhook event for the given trigger, enforcing rate limits.
    ///
    /// The event is appended to the trigger's `pending_events` queue in
    /// `TriggerState`.  The daemon tick will drain the queue on the next
    /// iteration and spawn pipelines for each event.
    ///
    /// Returns `Ok(())` on success, or a `WebApiError` with:
    /// - `"not_found"` if the trigger does not exist or is not a webhook type.
    /// - `"rate_limited"` if the rolling-minute limit has been exceeded.
    pub fn trigger_webhook_enqueue(
        &self,
        trigger_id: &str,
        payload: Value,
        now: DateTime<Utc>,
    ) -> Result<Value, WebApiError> {
        let project_root = std::path::Path::new(&self.context.project_root);
        let config = load_workflow_config_or_default(project_root);

        // Look up the trigger.
        let trigger = config
            .config
            .triggers
            .iter()
            .find(|t| t.id.eq_ignore_ascii_case(trigger_id))
            .ok_or_else(|| WebApiError::new("not_found", format!("trigger '{}' not found", trigger_id), 3))?;

        // Only webhook types can receive HTTP deliveries.
        if !matches!(
            trigger.trigger_type,
            orchestrator_core::workflow_config::TriggerType::Webhook
                | orchestrator_core::workflow_config::TriggerType::GithubWebhook
        ) {
            return Err(WebApiError::new("not_found", format!("trigger '{}' is not a webhook trigger", trigger_id), 3));
        }

        if !trigger.enabled {
            return Err(WebApiError::new("not_found", format!("trigger '{}' is disabled", trigger_id), 3));
        }

        // Parse webhook-specific config for rate limit.
        let wh_config = orchestrator_core::workflow_config::WebhookTriggerConfig::from_value(&trigger.config);
        let max_per_minute = wh_config.max_triggers_per_minute;

        // Load (or default) trigger state and apply rate limiting.
        let mut state = load_trigger_state(project_root).unwrap_or_default();
        let run_state = state.triggers.entry(trigger_id.to_string()).or_default();

        // Advance / reset the rolling-minute window.
        let window_elapsed =
            run_state.rate_window_start.map(|start| now.signed_duration_since(start).num_seconds()).unwrap_or(61); // No window yet → treat as expired.

        if window_elapsed >= 60 {
            // Start a new window.
            run_state.rate_window_start = Some(now);
            run_state.rate_window_count = 0;
        }

        if run_state.rate_window_count >= max_per_minute {
            return Err(WebApiError::new(
                "rate_limited",
                format!("trigger '{}' has exceeded {} requests/minute", trigger_id, max_per_minute),
                8,
            ));
        }

        // Accept the event.
        run_state.rate_window_count += 1;
        run_state.pending_events.push(WebhookEvent { event_id: Uuid::new_v4().to_string(), received_at: now, payload });

        // Persist state (fail-fast: the caller should know if we couldn't queue).
        save_trigger_state(project_root, &state)
            .map_err(|e| WebApiError::new("internal", format!("failed to save trigger state: {e}"), 1))?;

        Ok(serde_json::json!({
            "trigger_id": trigger_id,
            "status": "queued",
            "message": "event queued; will be dispatched on the next daemon tick",
        }))
    }
}
