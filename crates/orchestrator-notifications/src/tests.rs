#![allow(clippy::await_holding_lock)]

use super::*;
use std::env;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Mutex, OnceLock};
use std::thread;
use tempfile::TempDir;

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
                let (mut stream, _) = listener.accept().expect("connection accept should succeed");
                let mut buffer = [0_u8; 4096];
                let _ = stream.read(&mut buffer);
                let response =
                    format!("HTTP/1.1 {} Test\r\nContent-Length: 0\r\nConnection: close\r\n\r\n", status_code);
                stream.write_all(response.as_bytes()).expect("response should be written");
                let _ = stream.flush();
            }
        });
        Self { url, join_handle: Some(join_handle) }
    }
}

fn test_env_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

struct EnvVarGuard {
    key: &'static str,
    original: Option<String>,
}

impl EnvVarGuard {
    fn set(key: &'static str, value: Option<&str>) -> Self {
        let original = env::var(key).ok();
        match value {
            Some(value) => env::set_var(key, value),
            None => env::remove_var(key),
        }
        Self { key, original }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        match self.original.as_deref() {
            Some(value) => env::set_var(self.key, value),
            None => env::remove_var(self.key),
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
    fs::write(path, format!("{}\n", serde_json::to_string_pretty(&value).unwrap()))
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

    let policy = NotificationRetryPolicy { max_attempts: 5, base_delay_secs: 2, max_delay_secs: 16 };
    assert_eq!(retry_delay_secs(1, &policy), 2);
    assert_eq!(retry_delay_secs(2, &policy), 4);
    assert_eq!(retry_delay_secs(3, &policy), 8);
    assert_eq!(retry_delay_secs(4, &policy), 16);
    assert_eq!(retry_delay_secs(8, &policy), 16);
}

#[tokio::test]
async fn flush_budget_limits_processing_per_tick() {
    let _guard = test_env_lock().lock().expect("env lock should be available");
    let temp_home = TempDir::new().expect("temp home dir");
    let temp_project = TempDir::new().expect("temp project dir");
    let project_root = temp_project.path().to_string_lossy().to_string();
    let server = TestHttpServer::start(vec![200, 200]);

    let _home_guard = EnvVarGuard::set("HOME", Some(temp_home.path().to_string_lossy().as_ref()));
    let _url_guard = EnvVarGuard::set("AO_NOTIFY_WEBHOOK_URL", Some(server.url.as_str()));
    let _token_guard = EnvVarGuard::set("AO_NOTIFY_BEARER_TOKEN", Some("super-secret-token"));

    let mut config = sample_config("AO_NOTIFY_WEBHOOK_URL");
    config["notification_config"]["max_deliveries_per_tick"] = json!(1);
    write_pm_config(project_root.as_str(), config);

    let mut runtime = DaemonNotificationRuntime::new(project_root.as_str()).expect("runtime should initialize");
    runtime
        .enqueue_for_event(&sample_event(project_root.as_str(), "workflow-phase-started"))
        .expect("first enqueue should succeed");
    runtime
        .enqueue_for_event(&sample_event(project_root.as_str(), "workflow-phase-completed"))
        .expect("second enqueue should succeed");

    let first_flush = runtime.flush_due_deliveries().await.expect("first flush should succeed");
    assert_eq!(first_flush.iter().filter(|event| event.event_type == "notification-delivery-sent").count(), 1);
    let outbox_after_first = load_notification_outbox(project_root.as_str()).expect("outbox should load");
    assert_eq!(outbox_after_first.len(), 1);

    let second_flush = runtime.flush_due_deliveries().await.expect("second flush should succeed");
    assert_eq!(second_flush.iter().filter(|event| event.event_type == "notification-delivery-sent").count(), 1);
    let outbox_after_second = load_notification_outbox(project_root.as_str()).expect("outbox should load");
    assert!(outbox_after_second.is_empty());
}

#[tokio::test]
async fn enqueue_retry_then_success_clears_outbox() {
    let _guard = test_env_lock().lock().expect("env lock should be available");
    let temp_home = TempDir::new().expect("temp home dir");
    let temp_project = TempDir::new().expect("temp project dir");
    let project_root = temp_project.path().to_string_lossy().to_string();
    let server = TestHttpServer::start(vec![500, 200]);

    let _home_guard = EnvVarGuard::set("HOME", Some(temp_home.path().to_string_lossy().as_ref()));
    let _url_guard = EnvVarGuard::set("AO_NOTIFY_WEBHOOK_URL", Some(server.url.as_str()));
    let _token_guard = EnvVarGuard::set("AO_NOTIFY_BEARER_TOKEN", Some("super-secret-token"));

    write_pm_config(project_root.as_str(), sample_config("AO_NOTIFY_WEBHOOK_URL"));

    let mut runtime = DaemonNotificationRuntime::new(project_root.as_str()).expect("runtime should initialize");
    let event = sample_event(project_root.as_str(), "workflow-phase-started");

    let enqueued = runtime.enqueue_for_event(&event).expect("enqueue should succeed");
    assert_eq!(enqueued.len(), 1);

    let failed = runtime.flush_due_deliveries().await.expect("flush should process first attempt");
    assert!(failed.iter().any(|event| event.event_type == "notification-delivery-failed"));

    let mut outbox = load_notification_outbox(project_root.as_str()).expect("outbox should be readable");
    assert_eq!(outbox.len(), 1);
    outbox[0].next_attempt_unix_secs = Utc::now().timestamp() - 1;
    save_notification_outbox(project_root.as_str(), &outbox).expect("outbox should be rewritten");

    let sent = runtime.flush_due_deliveries().await.expect("second flush should succeed");
    assert!(sent.iter().any(|event| event.event_type == "notification-delivery-sent"));

    let outbox_after = load_notification_outbox(project_root.as_str()).expect("outbox should load after success");
    assert!(outbox_after.is_empty());
}

#[tokio::test]
async fn permanent_failure_moves_delivery_to_dead_letter() {
    let _guard = test_env_lock().lock().expect("env lock should be available");
    let temp_home = TempDir::new().expect("temp home dir");
    let temp_project = TempDir::new().expect("temp project dir");
    let project_root = temp_project.path().to_string_lossy().to_string();
    let server = TestHttpServer::start(vec![400]);

    let _home_guard = EnvVarGuard::set("HOME", Some(temp_home.path().to_string_lossy().as_ref()));
    let _url_guard = EnvVarGuard::set("AO_NOTIFY_WEBHOOK_URL", Some(server.url.as_str()));
    let _token_guard = EnvVarGuard::set("AO_NOTIFY_BEARER_TOKEN", Some("secret-token"));

    write_pm_config(project_root.as_str(), sample_config("AO_NOTIFY_WEBHOOK_URL"));

    let mut runtime = DaemonNotificationRuntime::new(project_root.as_str()).expect("runtime should initialize");
    let event = sample_event(project_root.as_str(), "workflow-phase-started");

    runtime.enqueue_for_event(&event).expect("enqueue should succeed");
    let events = runtime.flush_due_deliveries().await.expect("flush should process permanent failure");

    assert!(events.iter().any(|event| event.event_type == "notification-delivery-dead-lettered"));

    let outbox = load_notification_outbox(project_root.as_str()).expect("outbox should load");
    assert!(outbox.is_empty());

    let dead_letters = load_dead_letter_entries(project_root.as_str()).expect("dead-letter should load");
    assert_eq!(dead_letters.len(), 1);
    assert!(dead_letters[0].last_error.contains("HTTP 400"));
    assert!(!dead_letters[0].last_error.contains("secret-token"));
}

#[tokio::test]
async fn missing_credentials_are_redacted_and_dead_lettered() {
    let _guard = test_env_lock().lock().expect("env lock should be available");
    let temp_home = TempDir::new().expect("temp home dir");
    let temp_project = TempDir::new().expect("temp project dir");
    let project_root = temp_project.path().to_string_lossy().to_string();

    let _home_guard = EnvVarGuard::set("HOME", Some(temp_home.path().to_string_lossy().as_ref()));
    let _token_guard = EnvVarGuard::set("AO_NOTIFY_BEARER_TOKEN", Some("top-secret-value"));
    let _unset_url_guard = EnvVarGuard::set("AO_NOTIFY_WEBHOOK_URL", None);

    write_pm_config(project_root.as_str(), sample_config("AO_NOTIFY_WEBHOOK_URL"));

    let mut runtime = DaemonNotificationRuntime::new(project_root.as_str()).expect("runtime should initialize");
    runtime
        .enqueue_for_event(&sample_event(project_root.as_str(), "workflow-phase-started"))
        .expect("enqueue should succeed");
    runtime.flush_due_deliveries().await.expect("flush should handle missing credential");

    let dead_letters = load_dead_letter_entries(project_root.as_str()).expect("dead-letter should load");
    assert_eq!(dead_letters.len(), 1);
    assert!(dead_letters[0].last_error.contains("missing credential env var 'AO_NOTIFY_WEBHOOK_URL'"));
    assert!(!dead_letters[0].last_error.contains("top-secret-value"));
}

#[test]
fn enqueue_global_event_uses_runtime_project_root_and_context() {
    let _guard = test_env_lock().lock().expect("env lock should be available");
    let temp_home = TempDir::new().expect("temp home dir");
    let temp_project = TempDir::new().expect("temp project dir");
    let project_root = temp_project.path().to_string_lossy().to_string();

    let _home_guard = EnvVarGuard::set("HOME", Some(temp_home.path().to_string_lossy().as_ref()));
    write_pm_config(project_root.as_str(), sample_config("AO_NOTIFY_WEBHOOK_URL"));

    let mut runtime = DaemonNotificationRuntime::new(project_root.as_str()).expect("runtime should initialize");
    let mut event = sample_event(project_root.as_str(), "workflow-phase-started");
    event.project_root = None;
    let canonical_project_root = canonicalize_lossy(project_root.as_str());

    let enqueued = runtime.enqueue_for_event(&event).expect("enqueue should succeed");
    assert_eq!(enqueued.len(), 1);
    assert_eq!(enqueued[0].project_root.as_deref(), Some(canonical_project_root.as_str()));
    assert_eq!(enqueued[0].data.get("workflow_id").and_then(Value::as_str), Some("WF-001"));
    assert_eq!(enqueued[0].data.get("task_id").and_then(Value::as_str), Some("TASK-001"));

    let outbox = load_notification_outbox(project_root.as_str()).expect("outbox should load");
    assert_eq!(outbox.len(), 1);
    assert_eq!(outbox[0].event_project_root.as_deref(), Some(canonical_project_root.as_str()));
    assert_eq!(outbox[0].event_workflow_id.as_deref(), Some("WF-001"));
    assert_eq!(outbox[0].event_task_id.as_deref(), Some("TASK-001"));
}

#[tokio::test]
async fn pre_exhausted_entries_are_dead_lettered_without_another_attempt() {
    let _guard = test_env_lock().lock().expect("env lock should be available");
    let temp_home = TempDir::new().expect("temp home dir");
    let temp_project = TempDir::new().expect("temp project dir");
    let project_root = temp_project.path().to_string_lossy().to_string();

    let _home_guard = EnvVarGuard::set("HOME", Some(temp_home.path().to_string_lossy().as_ref()));
    write_pm_config(project_root.as_str(), sample_config("AO_NOTIFY_WEBHOOK_URL"));

    let mut runtime = DaemonNotificationRuntime::new(project_root.as_str()).expect("runtime should initialize");
    runtime
        .enqueue_for_event(&sample_event(project_root.as_str(), "workflow-phase-started"))
        .expect("enqueue should succeed");

    let mut outbox = load_notification_outbox(project_root.as_str()).expect("outbox should load");
    assert_eq!(outbox.len(), 1);
    outbox[0].attempts = 3;
    outbox[0].last_error = Some("notification endpoint returned HTTP 503".to_string());
    outbox[0].next_attempt_unix_secs = Utc::now().timestamp() - 1;
    save_notification_outbox(project_root.as_str(), &outbox).expect("outbox should be rewritten");

    let lifecycle_events = runtime.flush_due_deliveries().await.expect("flush should handle exhausted delivery");
    assert!(lifecycle_events.iter().any(|event| event.event_type == "notification-delivery-dead-lettered"));
    assert!(!lifecycle_events.iter().any(|event| event.event_type == "notification-delivery-failed"));

    let outbox_after = load_notification_outbox(project_root.as_str()).expect("outbox should load after flush");
    assert!(outbox_after.is_empty());

    let dead_letters = load_dead_letter_entries(project_root.as_str()).expect("dead-letter should load");
    assert_eq!(dead_letters.len(), 1);
    assert_eq!(dead_letters[0].attempts, 3);
    assert!(dead_letters[0].last_error.contains("HTTP 503"));
}

#[test]
fn redact_error_message_masks_configured_secret_values() {
    let _guard = test_env_lock().lock().expect("env lock should be available");
    let _url_guard =
        EnvVarGuard::set("AO_NOTIFY_TEST_WEBHOOK_URL", Some("https://hooks.example.invalid/secret-hook-token"));
    let _token_guard = EnvVarGuard::set("AO_NOTIFY_TEST_AUTH", Some("Bearer super-secret-auth-token"));

    let mut headers_env = BTreeMap::new();
    headers_env.insert("Authorization".to_string(), "AO_NOTIFY_TEST_AUTH".to_string());
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
