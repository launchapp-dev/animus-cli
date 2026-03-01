use super::canonicalize_lossy;
use super::daemon_events::DaemonEventRecord;
use anyhow::{anyhow, Context, Result};
use chrono::Utc;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, CONTENT_TYPE};
use reqwest::{Client, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;
use uuid::Uuid;

pub(super) const NOTIFICATION_CONFIG_SCHEMA: &str = "ao.daemon-notification-config.v1";
const NOTIFICATION_CONFIG_VERSION: u32 = 1;
const NOTIFICATION_CONFIG_PM_KEY: &str = "notification_config";
const DEFAULT_CONNECTOR_TIMEOUT_SECS: u64 = 10;
const DEFAULT_MAX_DELIVERIES_PER_TICK: usize = 8;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct NotificationLifecycleEvent {
    pub(super) event_type: String,
    pub(super) project_root: Option<String>,
    pub(super) data: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct NotificationConfig {
    #[serde(default = "default_notification_config_schema")]
    schema: String,
    #[serde(default = "default_notification_config_version")]
    version: u32,
    #[serde(default)]
    connectors: Vec<NotificationConnectorConfig>,
    #[serde(default)]
    subscriptions: Vec<NotificationSubscription>,
    #[serde(default)]
    retry_policy: NotificationRetryPolicy,
    #[serde(default = "default_max_deliveries_per_tick")]
    max_deliveries_per_tick: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(super) enum NotificationConnectorConfig {
    Webhook(WebhookConnectorConfig),
    SlackWebhook(SlackWebhookConnectorConfig),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct WebhookConnectorConfig {
    id: String,
    #[serde(default = "default_connector_enabled")]
    enabled: bool,
    url_env: String,
    #[serde(default)]
    headers_env: BTreeMap<String, String>,
    #[serde(default)]
    timeout_secs: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct SlackWebhookConnectorConfig {
    id: String,
    #[serde(default = "default_connector_enabled")]
    enabled: bool,
    webhook_url_env: String,
    #[serde(default)]
    headers_env: BTreeMap<String, String>,
    #[serde(default)]
    timeout_secs: Option<u64>,
    #[serde(default)]
    username: Option<String>,
    #[serde(default)]
    channel: Option<String>,
    #[serde(default)]
    icon_emoji: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct NotificationSubscription {
    id: String,
    #[serde(default = "default_connector_enabled")]
    enabled: bool,
    connector_id: String,
    #[serde(default = "default_event_type_wildcard")]
    event_types: Vec<String>,
    #[serde(default)]
    project_root: Option<String>,
    #[serde(default)]
    workflow_id: Option<String>,
    #[serde(default)]
    task_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct NotificationRetryPolicy {
    #[serde(default = "default_retry_max_attempts")]
    max_attempts: u32,
    #[serde(default = "default_retry_base_delay_secs")]
    base_delay_secs: u64,
    #[serde(default = "default_retry_max_delay_secs")]
    max_delay_secs: u64,
}

impl Default for NotificationRetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: default_retry_max_attempts(),
            base_delay_secs: default_retry_base_delay_secs(),
            max_delay_secs: default_retry_max_delay_secs(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct NotificationOutboxEntry {
    delivery_id: String,
    delivery_key: String,
    event_id: String,
    event_type: String,
    event_project_root: Option<String>,
    event_workflow_id: Option<String>,
    event_task_id: Option<String>,
    connector_id: String,
    subscription_id: String,
    attempts: u32,
    next_attempt_unix_secs: i64,
    last_error: Option<String>,
    payload: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct NotificationDeadLetterEntry {
    delivery_id: String,
    delivery_key: String,
    event_id: String,
    event_type: String,
    event_project_root: Option<String>,
    event_workflow_id: Option<String>,
    event_task_id: Option<String>,
    connector_id: String,
    subscription_id: String,
    attempts: u32,
    failed_at: String,
    last_error: String,
    payload: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeliveryFailureClass {
    Transient,
    Permanent,
    Misconfigured,
}

#[derive(Debug, Clone)]
struct DeliveryFailure {
    class: DeliveryFailureClass,
    message: String,
}

impl DeliveryFailure {
    fn transient(message: impl Into<String>) -> Self {
        Self {
            class: DeliveryFailureClass::Transient,
            message: message.into(),
        }
    }

    fn permanent(message: impl Into<String>) -> Self {
        Self {
            class: DeliveryFailureClass::Permanent,
            message: message.into(),
        }
    }

    fn misconfigured(message: impl Into<String>) -> Self {
        Self {
            class: DeliveryFailureClass::Misconfigured,
            message: message.into(),
        }
    }
}

#[derive(Debug)]
struct EventContext<'a> {
    workflow_id: Option<&'a str>,
    task_id: Option<&'a str>,
    source_connector_id: Option<&'a str>,
}

pub(super) struct DaemonNotificationRuntime {
    project_root: String,
    config: NotificationConfig,
    client: Client,
}

impl NotificationConnectorConfig {
    fn id(&self) -> &str {
        match self {
            NotificationConnectorConfig::Webhook(config) => config.id.as_str(),
            NotificationConnectorConfig::SlackWebhook(config) => config.id.as_str(),
        }
    }

    fn enabled(&self) -> bool {
        match self {
            NotificationConnectorConfig::Webhook(config) => config.enabled,
            NotificationConnectorConfig::SlackWebhook(config) => config.enabled,
        }
    }
}

impl NotificationSubscription {
    fn matches(&self, event: &DaemonEventRecord, context: &EventContext<'_>) -> bool {
        if !self.enabled {
            return false;
        }

        if event.event_type.starts_with("notification-delivery-")
            && context.source_connector_id == Some(self.connector_id.as_str())
        {
            return false;
        }

        let matches_event_type = self
            .event_types
            .iter()
            .any(|pattern| event_type_matches(pattern, &event.event_type));
        if !matches_event_type {
            return false;
        }

        if let Some(project_root) = self
            .project_root
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            if event.project_root.as_deref() != Some(project_root) {
                return false;
            }
        }

        if let Some(workflow_id) = self
            .workflow_id
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            if context.workflow_id != Some(workflow_id) {
                return false;
            }
        }

        if let Some(task_id) = self
            .task_id
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            if context.task_id != Some(task_id) {
                return false;
            }
        }

        true
    }
}

impl DaemonNotificationRuntime {
    pub(super) fn new(project_root: &str) -> Result<Self> {
        let project_root = canonicalize_lossy(project_root);
        let config = load_notification_config(&project_root)?;
        let client = Client::builder()
            .user_agent("ao-daemon-notifier/1")
            .build()
            .context("failed to construct notification HTTP client")?;
        Ok(Self {
            project_root,
            config,
            client,
        })
    }

    fn refresh_config(&mut self) -> Result<()> {
        self.config = load_notification_config(&self.project_root)?;
        Ok(())
    }

    pub(super) fn enqueue_for_event(
        &mut self,
        event: &DaemonEventRecord,
    ) -> Result<Vec<NotificationLifecycleEvent>> {
        self.refresh_config()?;

        if self.config.connectors.is_empty() || self.config.subscriptions.is_empty() {
            return Ok(Vec::new());
        }

        let mut outbox_entries = load_notification_outbox(&self.project_root)?;
        let mut known_keys: HashSet<String> = outbox_entries
            .iter()
            .map(|entry| entry.delivery_key.clone())
            .collect();
        let connector_lookup: HashMap<&str, &NotificationConnectorConfig> = self
            .config
            .connectors
            .iter()
            .map(|connector| (connector.id(), connector))
            .collect();

        let context = event_context(event);
        let event_project_root = event
            .project_root
            .clone()
            .or_else(|| Some(self.project_root.clone()));
        let now = Utc::now().timestamp();
        let mut lifecycle_events = Vec::new();

        for subscription in self
            .config
            .subscriptions
            .iter()
            .filter(|subscription| subscription.matches(event, &context))
        {
            if !connector_lookup.contains_key(subscription.connector_id.as_str()) {
                continue;
            }

            let delivery_key =
                delivery_key(&event.id, &subscription.connector_id, &subscription.id);
            if known_keys.contains(&delivery_key) {
                continue;
            }

            let delivery_id = Uuid::new_v4().to_string();
            let outbox_entry = NotificationOutboxEntry {
                delivery_id: delivery_id.clone(),
                delivery_key: delivery_key.clone(),
                event_id: event.id.clone(),
                event_type: event.event_type.clone(),
                event_project_root: event_project_root.clone(),
                event_workflow_id: context.workflow_id.map(ToOwned::to_owned),
                event_task_id: context.task_id.map(ToOwned::to_owned),
                connector_id: subscription.connector_id.clone(),
                subscription_id: subscription.id.clone(),
                attempts: 0,
                next_attempt_unix_secs: now,
                last_error: None,
                payload: build_delivery_payload(
                    event,
                    &delivery_id,
                    &subscription.connector_id,
                    &subscription.id,
                ),
            };

            known_keys.insert(delivery_key);
            lifecycle_events.push(NotificationLifecycleEvent {
                event_type: "notification-delivery-enqueued".to_string(),
                project_root: outbox_entry.event_project_root.clone(),
                data: json!({
                    "delivery_id": outbox_entry.delivery_id,
                    "event_id": outbox_entry.event_id,
                    "connector_id": outbox_entry.connector_id,
                    "subscription_id": outbox_entry.subscription_id,
                    "workflow_id": outbox_entry.event_workflow_id.clone(),
                    "task_id": outbox_entry.event_task_id.clone(),
                }),
            });
            outbox_entries.push(outbox_entry);
        }

        save_notification_outbox(&self.project_root, &outbox_entries)?;
        Ok(lifecycle_events)
    }

    pub(super) async fn flush_due_deliveries(&mut self) -> Result<Vec<NotificationLifecycleEvent>> {
        self.refresh_config()?;

        let entries = load_notification_outbox(&self.project_root)?;
        if entries.is_empty() {
            return Ok(Vec::new());
        }

        let mut remaining_entries = Vec::new();
        let mut lifecycle_events = Vec::new();
        let mut dead_letter_entries = load_dead_letter_entries(&self.project_root)?;

        let now = Utc::now().timestamp();
        let max_attempts = self.config.retry_policy.max_attempts.max(1);
        let flush_budget = self.config.max_deliveries_per_tick.max(1);
        let connector_lookup: HashMap<&str, &NotificationConnectorConfig> = self
            .config
            .connectors
            .iter()
            .map(|connector| (connector.id(), connector))
            .collect();

        let mut processed = 0usize;
        for mut entry in entries {
            if entry.next_attempt_unix_secs > now || processed >= flush_budget {
                remaining_entries.push(entry);
                continue;
            }
            processed = processed.saturating_add(1);

            let connector = connector_lookup.get(entry.connector_id.as_str()).copied();
            if entry.attempts >= max_attempts {
                let terminal_error = entry.last_error.clone().unwrap_or_else(|| {
                    format!("delivery reached retry attempt limit ({max_attempts})")
                });
                let redacted_error = redact_error_message(terminal_error.as_str(), connector);
                push_dead_letter(
                    &mut dead_letter_entries,
                    &mut lifecycle_events,
                    &entry,
                    redacted_error,
                );
                continue;
            }

            let result = self.send_delivery(connector, &entry).await;
            match result {
                Ok(()) => {
                    lifecycle_events.push(NotificationLifecycleEvent {
                        event_type: "notification-delivery-sent".to_string(),
                        project_root: entry.event_project_root.clone(),
                        data: json!({
                            "delivery_id": entry.delivery_id,
                            "event_id": entry.event_id,
                            "connector_id": entry.connector_id,
                            "subscription_id": entry.subscription_id,
                            "attempts": entry.attempts.saturating_add(1),
                            "workflow_id": entry.event_workflow_id.clone(),
                            "task_id": entry.event_task_id.clone(),
                        }),
                    });
                }
                Err(error) => {
                    entry.attempts = entry.attempts.saturating_add(1);
                    let redacted_error = redact_error_message(&error.message, connector);
                    let should_retry = error.class == DeliveryFailureClass::Transient
                        && entry.attempts < max_attempts;

                    lifecycle_events.push(NotificationLifecycleEvent {
                        event_type: "notification-delivery-failed".to_string(),
                        project_root: entry.event_project_root.clone(),
                        data: json!({
                            "delivery_id": entry.delivery_id,
                            "event_id": entry.event_id,
                            "connector_id": entry.connector_id,
                            "subscription_id": entry.subscription_id,
                            "attempts": entry.attempts,
                            "retriable": should_retry,
                            "last_error": redacted_error,
                            "workflow_id": entry.event_workflow_id.clone(),
                            "task_id": entry.event_task_id.clone(),
                        }),
                    });

                    if should_retry {
                        entry.last_error = Some(redacted_error);
                        entry.next_attempt_unix_secs = now.saturating_add(retry_delay_secs(
                            entry.attempts,
                            &self.config.retry_policy,
                        ));
                        remaining_entries.push(entry);
                    } else {
                        push_dead_letter(
                            &mut dead_letter_entries,
                            &mut lifecycle_events,
                            &entry,
                            redacted_error,
                        );
                    }
                }
            }
        }

        save_notification_outbox(&self.project_root, &remaining_entries)?;
        save_dead_letter_entries(&self.project_root, &dead_letter_entries)?;
        Ok(lifecycle_events)
    }

    async fn send_delivery(
        &self,
        connector: Option<&NotificationConnectorConfig>,
        entry: &NotificationOutboxEntry,
    ) -> std::result::Result<(), DeliveryFailure> {
        let connector = connector.ok_or_else(|| {
            DeliveryFailure::misconfigured(format!(
                "connector '{}' not found for subscription '{}'",
                entry.connector_id, entry.subscription_id
            ))
        })?;

        if !connector.enabled() {
            return Err(DeliveryFailure::misconfigured(format!(
                "connector '{}' is disabled",
                connector.id()
            )));
        }

        match connector {
            NotificationConnectorConfig::Webhook(config) => {
                self.send_webhook_delivery(config, entry).await
            }
            NotificationConnectorConfig::SlackWebhook(config) => {
                self.send_slack_delivery(config, entry).await
            }
        }
    }

    async fn send_webhook_delivery(
        &self,
        config: &WebhookConnectorConfig,
        entry: &NotificationOutboxEntry,
    ) -> std::result::Result<(), DeliveryFailure> {
        let url = resolve_env_var(config.url_env.as_str())?;
        let headers = resolve_headers(&config.headers_env)?;
        let timeout = Duration::from_secs(
            config
                .timeout_secs
                .unwrap_or(DEFAULT_CONNECTOR_TIMEOUT_SECS),
        );

        let response = self
            .client
            .post(url)
            .timeout(timeout)
            .header(CONTENT_TYPE, "application/json")
            .headers(headers)
            .json(&entry.payload)
            .send()
            .await
            .map_err(classify_request_error)?;

        classify_response_status(response.status())
    }

    async fn send_slack_delivery(
        &self,
        config: &SlackWebhookConnectorConfig,
        entry: &NotificationOutboxEntry,
    ) -> std::result::Result<(), DeliveryFailure> {
        let url = resolve_env_var(config.webhook_url_env.as_str())?;
        let headers = resolve_headers(&config.headers_env)?;
        let timeout = Duration::from_secs(
            config
                .timeout_secs
                .unwrap_or(DEFAULT_CONNECTOR_TIMEOUT_SECS),
        );

        let text = format!(
            "AO daemon event '{}' (delivery: {}, event: {})",
            entry.event_type, entry.delivery_id, entry.event_id
        );

        let mut payload = json!({
            "text": text,
        });
        if let Some(username) = config
            .username
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            payload["username"] = Value::String(username.to_string());
        }
        if let Some(channel) = config
            .channel
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            payload["channel"] = Value::String(channel.to_string());
        }
        if let Some(icon_emoji) = config
            .icon_emoji
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            payload["icon_emoji"] = Value::String(icon_emoji.to_string());
        }

        let response = self
            .client
            .post(url)
            .timeout(timeout)
            .header(CONTENT_TYPE, "application/json")
            .headers(headers)
            .json(&payload)
            .send()
            .await
            .map_err(classify_request_error)?;

        classify_response_status(response.status())
    }
}

pub(super) fn read_notification_config_from_pm_config(
    pm_config: &Value,
) -> Result<NotificationConfig> {
    let Some(config_value) = pm_config.get(NOTIFICATION_CONFIG_PM_KEY) else {
        return Ok(NotificationConfig::default());
    };
    parse_notification_config_value(config_value)
}

pub(super) fn parse_notification_config_value(value: &Value) -> Result<NotificationConfig> {
    let parsed: NotificationConfig = serde_json::from_value(value.clone())
        .context("invalid daemon notification config payload")?;
    normalize_notification_config(parsed)
}

pub(super) fn serialize_notification_config(config: &NotificationConfig) -> Result<Value> {
    Ok(serde_json::to_value(config).context("failed to serialize daemon notification config")?)
}

pub(super) fn clear_notification_config(pm_config: &mut Value) {
    if let Some(config_obj) = pm_config.as_object_mut() {
        config_obj.remove(NOTIFICATION_CONFIG_PM_KEY);
    }
}

impl Default for NotificationConfig {
    fn default() -> Self {
        Self {
            schema: default_notification_config_schema(),
            version: default_notification_config_version(),
            connectors: Vec::new(),
            subscriptions: Vec::new(),
            retry_policy: NotificationRetryPolicy::default(),
            max_deliveries_per_tick: default_max_deliveries_per_tick(),
        }
    }
}

fn default_notification_config_schema() -> String {
    NOTIFICATION_CONFIG_SCHEMA.to_string()
}

fn default_notification_config_version() -> u32 {
    NOTIFICATION_CONFIG_VERSION
}

fn default_retry_max_attempts() -> u32 {
    5
}

fn default_retry_base_delay_secs() -> u64 {
    2
}

fn default_retry_max_delay_secs() -> u64 {
    300
}

fn default_max_deliveries_per_tick() -> usize {
    DEFAULT_MAX_DELIVERIES_PER_TICK
}

fn default_connector_enabled() -> bool {
    true
}

fn default_event_type_wildcard() -> Vec<String> {
    vec!["*".to_string()]
}

fn normalize_notification_config(mut config: NotificationConfig) -> Result<NotificationConfig> {
    if config.schema.trim() != NOTIFICATION_CONFIG_SCHEMA {
        anyhow::bail!(
            "daemon notification config schema must be '{}'",
            NOTIFICATION_CONFIG_SCHEMA
        );
    }
    if config.version != NOTIFICATION_CONFIG_VERSION {
        anyhow::bail!(
            "daemon notification config version must be {}",
            NOTIFICATION_CONFIG_VERSION
        );
    }

    let mut connector_ids = HashSet::new();
    for connector in &mut config.connectors {
        match connector {
            NotificationConnectorConfig::Webhook(webhook) => {
                webhook.id = normalize_nonempty_identifier(&webhook.id, "connector id")?;
                webhook.url_env = normalize_env_ref(&webhook.url_env, "webhook.url_env")?;
                validate_header_env_refs(&webhook.headers_env, webhook.id.as_str())?;
            }
            NotificationConnectorConfig::SlackWebhook(slack) => {
                slack.id = normalize_nonempty_identifier(&slack.id, "connector id")?;
                slack.webhook_url_env =
                    normalize_env_ref(&slack.webhook_url_env, "slack_webhook.webhook_url_env")?;
                validate_header_env_refs(&slack.headers_env, slack.id.as_str())?;
            }
        }

        if !connector_ids.insert(connector.id().to_string()) {
            anyhow::bail!("duplicate connector id '{}'", connector.id());
        }
    }

    let mut subscription_ids = HashSet::new();
    for subscription in &mut config.subscriptions {
        subscription.id = normalize_nonempty_identifier(&subscription.id, "subscription id")?;
        subscription.connector_id =
            normalize_nonempty_identifier(&subscription.connector_id, "subscription connector_id")?;

        if subscription.event_types.is_empty() {
            subscription.event_types = default_event_type_wildcard();
        }

        for event_type in &mut subscription.event_types {
            *event_type = normalize_nonempty_identifier(event_type, "subscription event type")?;
        }

        if !subscription_ids.insert(subscription.id.clone()) {
            anyhow::bail!("duplicate subscription id '{}'", subscription.id);
        }
        if !connector_ids.contains(subscription.connector_id.as_str()) {
            anyhow::bail!(
                "subscription '{}' references unknown connector '{}'",
                subscription.id,
                subscription.connector_id
            );
        }

        subscription.project_root = normalize_optional_identifier(subscription.project_root.take());
        subscription.workflow_id = normalize_optional_identifier(subscription.workflow_id.take());
        subscription.task_id = normalize_optional_identifier(subscription.task_id.take());
    }

    config.retry_policy.max_attempts = config.retry_policy.max_attempts.clamp(1, 20);
    config.retry_policy.base_delay_secs = config.retry_policy.base_delay_secs.clamp(1, 3600);
    config.retry_policy.max_delay_secs = config
        .retry_policy
        .max_delay_secs
        .clamp(config.retry_policy.base_delay_secs, 86_400);
    config.max_deliveries_per_tick = config.max_deliveries_per_tick.clamp(1, 128);

    config
        .connectors
        .sort_by(|left, right| left.id().cmp(right.id()));
    config
        .subscriptions
        .sort_by(|left, right| left.id.cmp(&right.id));

    Ok(config)
}

fn normalize_nonempty_identifier(value: &str, label: &str) -> Result<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        anyhow::bail!("{} cannot be empty", label);
    }
    Ok(trimmed.to_string())
}

fn normalize_optional_identifier(value: Option<String>) -> Option<String> {
    value
        .as_deref()
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(ToOwned::to_owned)
}

fn normalize_env_ref(value: &str, label: &str) -> Result<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        anyhow::bail!("{} must reference a non-empty environment variable", label);
    }
    Ok(trimmed.to_string())
}

fn validate_header_env_refs(
    headers_env: &BTreeMap<String, String>,
    connector_id: &str,
) -> Result<()> {
    for (header_name, env_ref) in headers_env {
        let normalized_name = header_name.trim();
        if normalized_name.is_empty() {
            anyhow::bail!("connector '{}' contains an empty header name", connector_id);
        }
        HeaderName::from_bytes(normalized_name.as_bytes()).with_context(|| {
            format!(
                "connector '{}' contains invalid header name '{}'",
                connector_id, normalized_name
            )
        })?;

        let env_ref = env_ref.trim();
        if env_ref.is_empty() {
            anyhow::bail!(
                "connector '{}' contains empty env var reference for header '{}'",
                connector_id,
                normalized_name
            );
        }
    }

    Ok(())
}

fn event_type_matches(pattern: &str, event_type: &str) -> bool {
    wildcard_match(pattern.trim(), event_type.trim())
}

fn wildcard_match(pattern: &str, value: &str) -> bool {
    if pattern == "*" {
        return true;
    }

    let pattern_chars: Vec<char> = pattern.chars().collect();
    let value_chars: Vec<char> = value.chars().collect();
    let mut matrix = vec![vec![false; value_chars.len() + 1]; pattern_chars.len() + 1];
    matrix[0][0] = true;

    for i in 1..=pattern_chars.len() {
        if pattern_chars[i - 1] == '*' {
            matrix[i][0] = matrix[i - 1][0];
        }
        for j in 1..=value_chars.len() {
            if pattern_chars[i - 1] == '*' {
                matrix[i][j] = matrix[i - 1][j] || matrix[i][j - 1];
            } else if pattern_chars[i - 1] == value_chars[j - 1] {
                matrix[i][j] = matrix[i - 1][j - 1];
            }
        }
    }

    matrix[pattern_chars.len()][value_chars.len()]
}

fn event_context(event: &DaemonEventRecord) -> EventContext<'_> {
    EventContext {
        workflow_id: string_field(&event.data, "workflow_id"),
        task_id: string_field(&event.data, "task_id"),
        source_connector_id: string_field(&event.data, "connector_id"),
    }
}

fn string_field<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|item| !item.is_empty())
}

fn build_delivery_payload(
    event: &DaemonEventRecord,
    delivery_id: &str,
    connector_id: &str,
    subscription_id: &str,
) -> Value {
    json!({
        "schema": "ao.daemon.notification-delivery.v1",
        "delivery_id": delivery_id,
        "connector_id": connector_id,
        "subscription_id": subscription_id,
        "event": {
            "id": event.id,
            "seq": event.seq,
            "timestamp": event.timestamp,
            "event_type": event.event_type,
            "project_root": event.project_root,
            "data": event.data,
        },
    })
}

fn delivery_key(event_id: &str, connector_id: &str, subscription_id: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(event_id.as_bytes());
    hasher.update(&[0]);
    hasher.update(connector_id.as_bytes());
    hasher.update(&[0]);
    hasher.update(subscription_id.as_bytes());
    hex_from_digest(hasher.finalize())
}

fn resolve_env_var(env_ref: &str) -> std::result::Result<String, DeliveryFailure> {
    let env_ref = env_ref.trim();
    if env_ref.is_empty() {
        return Err(DeliveryFailure::misconfigured(
            "connector credential env var reference is empty",
        ));
    }

    let value = std::env::var(env_ref).map_err(|_| {
        DeliveryFailure::misconfigured(format!("missing credential env var '{}'", env_ref))
    })?;

    if value.trim().is_empty() {
        return Err(DeliveryFailure::misconfigured(format!(
            "credential env var '{}' is empty",
            env_ref
        )));
    }

    Ok(value)
}

fn resolve_headers(
    headers_env: &BTreeMap<String, String>,
) -> std::result::Result<HeaderMap, DeliveryFailure> {
    let mut headers = HeaderMap::new();
    for (header_name_raw, env_ref) in headers_env {
        let header_name =
            HeaderName::from_bytes(header_name_raw.trim().as_bytes()).map_err(|_| {
                DeliveryFailure::misconfigured(format!(
                    "invalid configured header name '{}'",
                    header_name_raw.trim()
                ))
            })?;

        let header_value = resolve_env_var(env_ref.as_str())?;
        let header_value = HeaderValue::from_str(header_value.as_str()).map_err(|_| {
            DeliveryFailure::misconfigured(format!(
                "header '{}' resolved to invalid value",
                header_name.as_str()
            ))
        })?;

        headers.insert(header_name, header_value);
    }

    Ok(headers)
}

fn classify_request_error(error: reqwest::Error) -> DeliveryFailure {
    if error.is_timeout() {
        return DeliveryFailure::transient("notification request timed out");
    }
    if error.is_connect() {
        return DeliveryFailure::transient("notification connector is unreachable");
    }
    if error.is_builder() {
        return DeliveryFailure::misconfigured(
            "invalid notification connector request configuration",
        );
    }
    if error.is_request() {
        return DeliveryFailure::transient("notification request failed before response");
    }

    DeliveryFailure::transient("notification delivery failed")
}

fn classify_response_status(status: StatusCode) -> std::result::Result<(), DeliveryFailure> {
    if status.is_success() {
        return Ok(());
    }

    let class = classify_http_status(status.as_u16());
    let message = format!("notification endpoint returned HTTP {}", status.as_u16());
    Err(match class {
        DeliveryFailureClass::Transient => DeliveryFailure::transient(message),
        DeliveryFailureClass::Permanent => DeliveryFailure::permanent(message),
        DeliveryFailureClass::Misconfigured => DeliveryFailure::misconfigured(message),
    })
}

fn classify_http_status(status: u16) -> DeliveryFailureClass {
    match status {
        408 | 425 | 429 => DeliveryFailureClass::Transient,
        500..=599 => DeliveryFailureClass::Transient,
        400..=499 => DeliveryFailureClass::Permanent,
        _ => DeliveryFailureClass::Permanent,
    }
}

fn retry_delay_secs(attempts: u32, policy: &NotificationRetryPolicy) -> i64 {
    let base = i64::try_from(policy.base_delay_secs.max(1)).unwrap_or(1);
    let max =
        i64::try_from(policy.max_delay_secs.max(policy.base_delay_secs.max(1))).unwrap_or(base);
    let exponent = attempts.saturating_sub(1).min(16);
    let growth = 1_i64.checked_shl(exponent).unwrap_or(i64::MAX);
    base.saturating_mul(growth).clamp(base, max)
}

fn connector_credential_env_refs(connector: &NotificationConnectorConfig) -> Vec<&str> {
    match connector {
        NotificationConnectorConfig::Webhook(config) => {
            let mut refs = Vec::with_capacity(1 + config.headers_env.len());
            refs.push(config.url_env.as_str());
            refs.extend(config.headers_env.values().map(String::as_str));
            refs
        }
        NotificationConnectorConfig::SlackWebhook(config) => {
            let mut refs = Vec::with_capacity(1 + config.headers_env.len());
            refs.push(config.webhook_url_env.as_str());
            refs.extend(config.headers_env.values().map(String::as_str));
            refs
        }
    }
}

fn redact_error_message(message: &str, connector: Option<&NotificationConnectorConfig>) -> String {
    let mut redacted = message.replace('\n', " ").replace('\r', " ");

    if let Some(connector) = connector {
        let mut seen_refs = HashSet::new();
        for env_ref in connector_credential_env_refs(connector) {
            if !seen_refs.insert(env_ref) {
                continue;
            }
            let Ok(secret) = std::env::var(env_ref) else {
                continue;
            };
            let secret = secret.trim();
            if secret.is_empty() {
                continue;
            }
            redacted = redacted.replace(secret, "<redacted>");
        }
    }

    redacted.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn push_dead_letter(
    dead_letter_entries: &mut Vec<NotificationDeadLetterEntry>,
    lifecycle_events: &mut Vec<NotificationLifecycleEvent>,
    entry: &NotificationOutboxEntry,
    last_error: String,
) {
    dead_letter_entries.push(NotificationDeadLetterEntry {
        delivery_id: entry.delivery_id.clone(),
        delivery_key: entry.delivery_key.clone(),
        event_id: entry.event_id.clone(),
        event_type: entry.event_type.clone(),
        event_project_root: entry.event_project_root.clone(),
        event_workflow_id: entry.event_workflow_id.clone(),
        event_task_id: entry.event_task_id.clone(),
        connector_id: entry.connector_id.clone(),
        subscription_id: entry.subscription_id.clone(),
        attempts: entry.attempts,
        failed_at: Utc::now().to_rfc3339(),
        last_error: last_error.clone(),
        payload: entry.payload.clone(),
    });
    lifecycle_events.push(NotificationLifecycleEvent {
        event_type: "notification-delivery-dead-lettered".to_string(),
        project_root: entry.event_project_root.clone(),
        data: json!({
            "delivery_id": entry.delivery_id,
            "event_id": entry.event_id,
            "connector_id": entry.connector_id,
            "subscription_id": entry.subscription_id,
            "attempts": entry.attempts,
            "last_error": last_error,
            "workflow_id": entry.event_workflow_id.clone(),
            "task_id": entry.event_task_id.clone(),
        }),
    });
}

fn load_notification_config(project_root: &str) -> Result<NotificationConfig> {
    let pm_config_path = pm_config_path(project_root);
    if !pm_config_path.exists() {
        return Ok(NotificationConfig::default());
    }

    let content = fs::read_to_string(&pm_config_path).with_context(|| {
        format!(
            "failed to read daemon config at {}",
            pm_config_path.display()
        )
    })?;
    if content.trim().is_empty() {
        return Ok(NotificationConfig::default());
    }

    let pm_config: Value = serde_json::from_str(content.as_str())
        .with_context(|| format!("invalid daemon config JSON at {}", pm_config_path.display()))?;
    read_notification_config_from_pm_config(&pm_config)
}

fn pm_config_path(project_root: &str) -> PathBuf {
    PathBuf::from(canonicalize_lossy(project_root))
        .join(".ao")
        .join("pm-config.json")
}

fn load_jsonl_entries<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<Vec<T>> {
    if !path.exists() {
        return Ok(Vec::new());
    }

    let content = fs::read_to_string(path)
        .with_context(|| format!("failed to read notification state at {}", path.display()))?;
    let mut entries = Vec::new();
    for line in content
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        if let Ok(entry) = serde_json::from_str::<T>(line) {
            entries.push(entry);
        }
    }

    Ok(entries)
}

fn save_jsonl_entries<T: Serialize>(path: &Path, entries: &[T]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create directory {}", parent.display()))?;
    }

    if entries.is_empty() {
        if path.exists() {
            fs::remove_file(path).with_context(|| {
                format!("failed to clear notification state {}", path.display())
            })?;
        }
        return Ok(());
    }

    let mut payload = String::new();
    for entry in entries {
        payload.push_str(&serde_json::to_string(entry)?);
        payload.push('\n');
    }

    let tmp_path = path.with_file_name(format!(
        "{}.{}.tmp",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("notifications"),
        Uuid::new_v4()
    ));

    fs::write(&tmp_path, payload).with_context(|| {
        format!(
            "failed to write notification temp file {}",
            tmp_path.display()
        )
    })?;
    fs::rename(&tmp_path, path).with_context(|| {
        format!(
            "failed to atomically rename notification file {}",
            path.display()
        )
    })?;
    Ok(())
}

fn load_notification_outbox(project_root: &str) -> Result<Vec<NotificationOutboxEntry>> {
    let path = notification_outbox_path(project_root)?;
    load_jsonl_entries(path.as_path())
}

fn save_notification_outbox(project_root: &str, entries: &[NotificationOutboxEntry]) -> Result<()> {
    let path = notification_outbox_path(project_root)?;
    save_jsonl_entries(path.as_path(), entries)
}

fn load_dead_letter_entries(project_root: &str) -> Result<Vec<NotificationDeadLetterEntry>> {
    let path = notification_dead_letter_path(project_root)?;
    load_jsonl_entries(path.as_path())
}

fn save_dead_letter_entries(
    project_root: &str,
    entries: &[NotificationDeadLetterEntry],
) -> Result<()> {
    let path = notification_dead_letter_path(project_root)?;
    save_jsonl_entries(path.as_path(), entries)
}

fn notification_outbox_path(project_root: &str) -> Result<PathBuf> {
    Ok(notification_state_root(project_root)?.join("outbox.jsonl"))
}

fn notification_dead_letter_path(project_root: &str) -> Result<PathBuf> {
    Ok(notification_state_root(project_root)?.join("dead-letter.jsonl"))
}

fn notification_state_root(project_root: &str) -> Result<PathBuf> {
    Ok(repo_scope_root(project_root)?.join("notifications"))
}

fn repo_scope_root(project_root: &str) -> Result<PathBuf> {
    Ok(ao_root_dir()?.join(repo_scope(project_root)))
}

fn ao_root_dir() -> Result<PathBuf> {
    let home =
        dirs::home_dir().ok_or_else(|| anyhow!("failed to resolve home directory for ~/.ao"))?;
    Ok(home.join(".ao"))
}

fn repo_scope(project_root: &str) -> String {
    protocol::repository_scope_for_path(Path::new(project_root))
}

fn hex_from_digest(digest: impl AsRef<[u8]>) -> String {
    let bytes = digest.as_ref();
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        text.push_str(format!("{:02x}", byte).as_str());
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::{Mutex, OnceLock};
    use std::thread;
    use tempfile::TempDir;

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    struct EnvVarGuard {
        key: &'static str,
        previous: Option<String>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: Option<&str>) -> Self {
            let previous = std::env::var(key).ok();
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
            Self { key, previous }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            if let Some(previous) = &self.previous {
                std::env::set_var(self.key, previous);
            } else {
                std::env::remove_var(self.key);
            }
        }
    }

    #[derive(Debug)]
    struct TestHttpServer {
        url: String,
        join_handle: Option<thread::JoinHandle<()>>,
    }

    impl TestHttpServer {
        fn start(status_codes: Vec<u16>) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").expect("listener should bind");
            let address = listener.local_addr().expect("listener local addr");
            let url = format!("http://{}/notify", address);
            let join_handle = thread::spawn(move || {
                for status_code in status_codes {
                    let (mut stream, _) =
                        listener.accept().expect("connection accept should succeed");
                    let mut buffer = [0_u8; 4096];
                    let _ = stream.read(&mut buffer);
                    let response = format!(
                        "HTTP/1.1 {} Test\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                        status_code
                    );
                    stream
                        .write_all(response.as_bytes())
                        .expect("response should be written");
                    let _ = stream.flush();
                }
            });
            Self {
                url,
                join_handle: Some(join_handle),
            }
        }
    }

    impl Drop for TestHttpServer {
        fn drop(&mut self) {
            if let Some(handle) = self.join_handle.take() {
                let _ = handle.join();
            }
        }
    }

    fn write_pm_config(project_root: &str, value: Value) {
        let path = pm_config_path(project_root);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("pm-config parent should exist");
        }
        fs::write(
            path,
            format!("{}\n", serde_json::to_string_pretty(&value).unwrap()),
        )
        .expect("pm-config should be written");
    }

    fn sample_event(project_root: &str, event_type: &str) -> DaemonEventRecord {
        DaemonEventRecord {
            schema: "ao.daemon.event.v1".to_string(),
            id: Uuid::new_v4().to_string(),
            seq: 1,
            timestamp: Utc::now().to_rfc3339(),
            event_type: event_type.to_string(),
            project_root: Some(project_root.to_string()),
            data: json!({
                "workflow_id": "WF-001",
                "task_id": "TASK-001"
            }),
        }
    }

    fn sample_config(url_env: &str) -> Value {
        json!({
            "notification_config": {
                "schema": "ao.daemon-notification-config.v1",
                "version": 1,
                "connectors": [
                    {
                        "type": "webhook",
                        "id": "ops-webhook",
                        "enabled": true,
                        "url_env": url_env,
                        "headers_env": {
                            "Authorization": "AO_NOTIFY_BEARER_TOKEN"
                        },
                        "timeout_secs": 2
                    }
                ],
                "subscriptions": [
                    {
                        "id": "all-events",
                        "enabled": true,
                        "connector_id": "ops-webhook",
                        "event_types": ["workflow*"]
                    }
                ],
                "retry_policy": {
                    "max_attempts": 3,
                    "base_delay_secs": 1,
                    "max_delay_secs": 4
                },
                "max_deliveries_per_tick": 8
            }
        })
    }

    #[test]
    fn subscription_matching_supports_wildcards_and_filters() {
        let subscription = NotificationSubscription {
            id: "sub-1".to_string(),
            enabled: true,
            connector_id: "connector-1".to_string(),
            event_types: vec!["workflow-phase-*".to_string()],
            project_root: Some("/tmp/project".to_string()),
            workflow_id: Some("WF-123".to_string()),
            task_id: Some("TASK-123".to_string()),
        };

        let event = DaemonEventRecord {
            schema: "ao.daemon.event.v1".to_string(),
            id: "evt-1".to_string(),
            seq: 1,
            timestamp: Utc::now().to_rfc3339(),
            event_type: "workflow-phase-started".to_string(),
            project_root: Some("/tmp/project".to_string()),
            data: json!({
                "workflow_id": "WF-123",
                "task_id": "TASK-123"
            }),
        };

        assert!(subscription.matches(&event, &event_context(&event)));

        let mismatched = DaemonEventRecord {
            event_type: "workflow-phase-started".to_string(),
            data: json!({
                "workflow_id": "WF-999",
                "task_id": "TASK-123"
            }),
            ..event
        };
        assert!(!subscription.matches(&mismatched, &event_context(&mismatched)));
    }

    #[test]
    fn delivery_events_do_not_reenqueue_same_connector() {
        let subscription = NotificationSubscription {
            id: "sub-1".to_string(),
            enabled: true,
            connector_id: "ops-webhook".to_string(),
            event_types: vec!["notification-delivery-*".to_string()],
            project_root: None,
            workflow_id: None,
            task_id: None,
        };

        let event = DaemonEventRecord {
            schema: "ao.daemon.event.v1".to_string(),
            id: "evt-1".to_string(),
            seq: 1,
            timestamp: Utc::now().to_rfc3339(),
            event_type: "notification-delivery-failed".to_string(),
            project_root: None,
            data: json!({
                "connector_id": "ops-webhook"
            }),
        };
        assert!(!subscription.matches(&event, &event_context(&event)));

        let forwarded_event = DaemonEventRecord {
            data: json!({
                "connector_id": "audit-webhook"
            }),
            ..event
        };
        assert!(subscription.matches(&forwarded_event, &event_context(&forwarded_event)));
    }

    #[test]
    fn retry_classification_and_backoff_are_deterministic() {
        assert_eq!(classify_http_status(500), DeliveryFailureClass::Transient);
        assert_eq!(classify_http_status(429), DeliveryFailureClass::Transient);
        assert_eq!(classify_http_status(400), DeliveryFailureClass::Permanent);

        let policy = NotificationRetryPolicy {
            max_attempts: 5,
            base_delay_secs: 2,
            max_delay_secs: 16,
        };
        assert_eq!(retry_delay_secs(1, &policy), 2);
        assert_eq!(retry_delay_secs(2, &policy), 4);
        assert_eq!(retry_delay_secs(3, &policy), 8);
        assert_eq!(retry_delay_secs(4, &policy), 16);
        assert_eq!(retry_delay_secs(8, &policy), 16);
    }

    #[tokio::test]
    async fn flush_budget_limits_processing_per_tick() {
        let _guard = env_lock().lock().expect("env lock should be available");
        let temp_home = TempDir::new().expect("temp home dir");
        let temp_project = TempDir::new().expect("temp project dir");
        let project_root = temp_project.path().to_string_lossy().to_string();
        let server = TestHttpServer::start(vec![200, 200]);

        let _home_guard =
            EnvVarGuard::set("HOME", Some(temp_home.path().to_string_lossy().as_ref()));
        let _url_guard = EnvVarGuard::set("AO_NOTIFY_WEBHOOK_URL", Some(server.url.as_str()));
        let _token_guard = EnvVarGuard::set("AO_NOTIFY_BEARER_TOKEN", Some("super-secret-token"));

        let mut config = sample_config("AO_NOTIFY_WEBHOOK_URL");
        config["notification_config"]["max_deliveries_per_tick"] = json!(1);
        write_pm_config(project_root.as_str(), config);

        let mut runtime = DaemonNotificationRuntime::new(project_root.as_str())
            .expect("runtime should initialize");
        runtime
            .enqueue_for_event(&sample_event(
                project_root.as_str(),
                "workflow-phase-started",
            ))
            .expect("first enqueue should succeed");
        runtime
            .enqueue_for_event(&sample_event(
                project_root.as_str(),
                "workflow-phase-completed",
            ))
            .expect("second enqueue should succeed");

        let first_flush = runtime
            .flush_due_deliveries()
            .await
            .expect("first flush should succeed");
        assert_eq!(
            first_flush
                .iter()
                .filter(|event| event.event_type == "notification-delivery-sent")
                .count(),
            1
        );
        let outbox_after_first =
            load_notification_outbox(project_root.as_str()).expect("outbox should load");
        assert_eq!(outbox_after_first.len(), 1);

        let second_flush = runtime
            .flush_due_deliveries()
            .await
            .expect("second flush should succeed");
        assert_eq!(
            second_flush
                .iter()
                .filter(|event| event.event_type == "notification-delivery-sent")
                .count(),
            1
        );
        let outbox_after_second =
            load_notification_outbox(project_root.as_str()).expect("outbox should load");
        assert!(outbox_after_second.is_empty());
    }

    #[tokio::test]
    async fn enqueue_retry_then_success_clears_outbox() {
        let _guard = env_lock().lock().expect("env lock should be available");
        let temp_home = TempDir::new().expect("temp home dir");
        let temp_project = TempDir::new().expect("temp project dir");
        let project_root = temp_project.path().to_string_lossy().to_string();
        let server = TestHttpServer::start(vec![500, 200]);

        let _home_guard =
            EnvVarGuard::set("HOME", Some(temp_home.path().to_string_lossy().as_ref()));
        let _url_guard = EnvVarGuard::set("AO_NOTIFY_WEBHOOK_URL", Some(server.url.as_str()));
        let _token_guard = EnvVarGuard::set("AO_NOTIFY_BEARER_TOKEN", Some("super-secret-token"));

        write_pm_config(
            project_root.as_str(),
            sample_config("AO_NOTIFY_WEBHOOK_URL"),
        );

        let mut runtime = DaemonNotificationRuntime::new(project_root.as_str())
            .expect("runtime should initialize");
        let event = sample_event(project_root.as_str(), "workflow-phase-started");

        let enqueued = runtime
            .enqueue_for_event(&event)
            .expect("enqueue should succeed");
        assert_eq!(enqueued.len(), 1);

        let failed = runtime
            .flush_due_deliveries()
            .await
            .expect("flush should process first attempt");
        assert!(failed
            .iter()
            .any(|event| event.event_type == "notification-delivery-failed"));

        let mut outbox =
            load_notification_outbox(project_root.as_str()).expect("outbox should be readable");
        assert_eq!(outbox.len(), 1);
        outbox[0].next_attempt_unix_secs = Utc::now().timestamp() - 1;
        save_notification_outbox(project_root.as_str(), &outbox)
            .expect("outbox should be rewritten");

        let sent = runtime
            .flush_due_deliveries()
            .await
            .expect("second flush should succeed");
        assert!(sent
            .iter()
            .any(|event| event.event_type == "notification-delivery-sent"));

        let outbox_after = load_notification_outbox(project_root.as_str())
            .expect("outbox should load after success");
        assert!(outbox_after.is_empty());
    }

    #[tokio::test]
    async fn permanent_failure_moves_delivery_to_dead_letter() {
        let _guard = env_lock().lock().expect("env lock should be available");
        let temp_home = TempDir::new().expect("temp home dir");
        let temp_project = TempDir::new().expect("temp project dir");
        let project_root = temp_project.path().to_string_lossy().to_string();
        let server = TestHttpServer::start(vec![400]);

        let _home_guard =
            EnvVarGuard::set("HOME", Some(temp_home.path().to_string_lossy().as_ref()));
        let _url_guard = EnvVarGuard::set("AO_NOTIFY_WEBHOOK_URL", Some(server.url.as_str()));
        let _token_guard = EnvVarGuard::set("AO_NOTIFY_BEARER_TOKEN", Some("secret-token"));

        write_pm_config(
            project_root.as_str(),
            sample_config("AO_NOTIFY_WEBHOOK_URL"),
        );

        let mut runtime = DaemonNotificationRuntime::new(project_root.as_str())
            .expect("runtime should initialize");
        let event = sample_event(project_root.as_str(), "workflow-phase-started");

        runtime
            .enqueue_for_event(&event)
            .expect("enqueue should succeed");
        let events = runtime
            .flush_due_deliveries()
            .await
            .expect("flush should process permanent failure");

        assert!(events
            .iter()
            .any(|event| event.event_type == "notification-delivery-dead-lettered"));

        let outbox = load_notification_outbox(project_root.as_str()).expect("outbox should load");
        assert!(outbox.is_empty());

        let dead_letters =
            load_dead_letter_entries(project_root.as_str()).expect("dead-letter should load");
        assert_eq!(dead_letters.len(), 1);
        assert!(dead_letters[0].last_error.contains("HTTP 400"));
        assert!(!dead_letters[0].last_error.contains("secret-token"));
    }

    #[tokio::test]
    async fn missing_credentials_are_redacted_and_dead_lettered() {
        let _guard = env_lock().lock().expect("env lock should be available");
        let temp_home = TempDir::new().expect("temp home dir");
        let temp_project = TempDir::new().expect("temp project dir");
        let project_root = temp_project.path().to_string_lossy().to_string();

        let _home_guard =
            EnvVarGuard::set("HOME", Some(temp_home.path().to_string_lossy().as_ref()));
        let _token_guard = EnvVarGuard::set("AO_NOTIFY_BEARER_TOKEN", Some("top-secret-value"));
        let _unset_url_guard = EnvVarGuard::set("AO_NOTIFY_WEBHOOK_URL", None);

        write_pm_config(
            project_root.as_str(),
            sample_config("AO_NOTIFY_WEBHOOK_URL"),
        );

        let mut runtime = DaemonNotificationRuntime::new(project_root.as_str())
            .expect("runtime should initialize");
        runtime
            .enqueue_for_event(&sample_event(
                project_root.as_str(),
                "workflow-phase-started",
            ))
            .expect("enqueue should succeed");
        runtime
            .flush_due_deliveries()
            .await
            .expect("flush should handle missing credential");

        let dead_letters =
            load_dead_letter_entries(project_root.as_str()).expect("dead-letter should load");
        assert_eq!(dead_letters.len(), 1);
        assert!(dead_letters[0]
            .last_error
            .contains("missing credential env var 'AO_NOTIFY_WEBHOOK_URL'"));
        assert!(!dead_letters[0].last_error.contains("top-secret-value"));
    }

    #[test]
    fn enqueue_global_event_uses_runtime_project_root_and_context() {
        let _guard = env_lock().lock().expect("env lock should be available");
        let temp_home = TempDir::new().expect("temp home dir");
        let temp_project = TempDir::new().expect("temp project dir");
        let project_root = temp_project.path().to_string_lossy().to_string();

        let _home_guard =
            EnvVarGuard::set("HOME", Some(temp_home.path().to_string_lossy().as_ref()));
        write_pm_config(
            project_root.as_str(),
            sample_config("AO_NOTIFY_WEBHOOK_URL"),
        );

        let mut runtime = DaemonNotificationRuntime::new(project_root.as_str())
            .expect("runtime should initialize");
        let mut event = sample_event(project_root.as_str(), "workflow-phase-started");
        event.project_root = None;
        let canonical_project_root = canonicalize_lossy(project_root.as_str());

        let enqueued = runtime
            .enqueue_for_event(&event)
            .expect("enqueue should succeed");
        assert_eq!(enqueued.len(), 1);
        assert_eq!(
            enqueued[0].project_root.as_deref(),
            Some(canonical_project_root.as_str())
        );
        assert_eq!(
            enqueued[0].data.get("workflow_id").and_then(Value::as_str),
            Some("WF-001")
        );
        assert_eq!(
            enqueued[0].data.get("task_id").and_then(Value::as_str),
            Some("TASK-001")
        );

        let outbox = load_notification_outbox(project_root.as_str()).expect("outbox should load");
        assert_eq!(outbox.len(), 1);
        assert_eq!(
            outbox[0].event_project_root.as_deref(),
            Some(canonical_project_root.as_str())
        );
        assert_eq!(outbox[0].event_workflow_id.as_deref(), Some("WF-001"));
        assert_eq!(outbox[0].event_task_id.as_deref(), Some("TASK-001"));
    }

    #[tokio::test]
    async fn pre_exhausted_entries_are_dead_lettered_without_another_attempt() {
        let _guard = env_lock().lock().expect("env lock should be available");
        let temp_home = TempDir::new().expect("temp home dir");
        let temp_project = TempDir::new().expect("temp project dir");
        let project_root = temp_project.path().to_string_lossy().to_string();

        let _home_guard =
            EnvVarGuard::set("HOME", Some(temp_home.path().to_string_lossy().as_ref()));
        write_pm_config(
            project_root.as_str(),
            sample_config("AO_NOTIFY_WEBHOOK_URL"),
        );

        let mut runtime = DaemonNotificationRuntime::new(project_root.as_str())
            .expect("runtime should initialize");
        runtime
            .enqueue_for_event(&sample_event(
                project_root.as_str(),
                "workflow-phase-started",
            ))
            .expect("enqueue should succeed");

        let mut outbox =
            load_notification_outbox(project_root.as_str()).expect("outbox should load");
        assert_eq!(outbox.len(), 1);
        outbox[0].attempts = 3;
        outbox[0].last_error = Some("notification endpoint returned HTTP 503".to_string());
        outbox[0].next_attempt_unix_secs = Utc::now().timestamp() - 1;
        save_notification_outbox(project_root.as_str(), &outbox)
            .expect("outbox should be rewritten");

        let lifecycle_events = runtime
            .flush_due_deliveries()
            .await
            .expect("flush should handle exhausted delivery");
        assert!(lifecycle_events
            .iter()
            .any(|event| event.event_type == "notification-delivery-dead-lettered"));
        assert!(!lifecycle_events
            .iter()
            .any(|event| event.event_type == "notification-delivery-failed"));

        let outbox_after = load_notification_outbox(project_root.as_str())
            .expect("outbox should load after flush");
        assert!(outbox_after.is_empty());

        let dead_letters =
            load_dead_letter_entries(project_root.as_str()).expect("dead-letter should load");
        assert_eq!(dead_letters.len(), 1);
        assert_eq!(dead_letters[0].attempts, 3);
        assert!(dead_letters[0].last_error.contains("HTTP 503"));
    }

    #[test]
    fn redact_error_message_masks_configured_secret_values() {
        let _guard = env_lock().lock().expect("env lock should be available");
        let _url_guard = EnvVarGuard::set(
            "AO_NOTIFY_TEST_WEBHOOK_URL",
            Some("https://hooks.example.invalid/secret-hook-token"),
        );
        let _token_guard = EnvVarGuard::set(
            "AO_NOTIFY_TEST_AUTH",
            Some("Bearer super-secret-auth-token"),
        );

        let mut headers_env = BTreeMap::new();
        headers_env.insert(
            "Authorization".to_string(),
            "AO_NOTIFY_TEST_AUTH".to_string(),
        );
        let connector = NotificationConnectorConfig::Webhook(WebhookConnectorConfig {
            id: "ops-webhook".to_string(),
            enabled: true,
            url_env: "AO_NOTIFY_TEST_WEBHOOK_URL".to_string(),
            headers_env,
            timeout_secs: Some(2),
        });

        let message = "request to https://hooks.example.invalid/secret-hook-token failed: invalid token Bearer super-secret-auth-token";
        let redacted = redact_error_message(message, Some(&connector));
        assert!(!redacted.contains("secret-hook-token"));
        assert!(!redacted.contains("super-secret-auth-token"));
        assert!(redacted.contains("<redacted>"));
    }
}
