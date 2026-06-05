//! HTTP transport with retry/backoff. Metrics emission MUST NEVER block.

use std::collections::BTreeMap;
use std::time::Duration;

use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::events::EventName;
use super::store::PendingEvent;

pub const MAX_ATTEMPTS: u32 = 3;
const BASE_BACKOFF_MS: u64 = 250;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PayloadEvent {
    pub name: EventName,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub tags: BTreeMap<String, String>,
    pub count: u64,
    pub first_seen: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Payload {
    pub install_id: String,
    pub animus_version: String,
    pub os: String,
    pub arch: String,
    pub events: Vec<PayloadEvent>,
}

pub fn build_payload(install_id: &str, events: Vec<PendingEvent>) -> Payload {
    let mut grouped: BTreeMap<(EventName, Vec<(String, String)>), PayloadEvent> = BTreeMap::new();
    for event in events {
        let tag_key: Vec<(String, String)> = event.tags.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        let key = (event.name, tag_key);
        grouped
            .entry(key)
            .and_modify(|payload| {
                payload.count += 1;
                if event.recorded_at < payload.first_seen {
                    payload.first_seen = event.recorded_at;
                }
            })
            .or_insert(PayloadEvent {
                name: event.name,
                tags: event.tags,
                count: 1,
                first_seen: event.recorded_at,
            });
    }

    Payload {
        install_id: install_id.to_string(),
        animus_version: env!("CARGO_PKG_VERSION").to_string(),
        os: std::env::consts::OS.to_string(),
        arch: std::env::consts::ARCH.to_string(),
        events: grouped.into_values().collect(),
    }
}

#[async_trait]
pub trait MetricsTransport: Send + Sync {
    async fn send(&self, endpoint: &str, payload: &Payload) -> Result<()>;
}

pub struct HttpTransport {
    client: reqwest::Client,
}

impl HttpTransport {
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .user_agent(format!("animus/{}", env!("CARGO_PKG_VERSION")))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self { client }
    }
}

impl Default for HttpTransport {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl MetricsTransport for HttpTransport {
    async fn send(&self, endpoint: &str, payload: &Payload) -> Result<()> {
        let response = self.client.post(endpoint).json(payload).send().await?;
        let status = response.status();
        if !status.is_success() {
            anyhow::bail!("metrics endpoint returned status {status}");
        }
        Ok(())
    }
}

pub async fn send_with_retry<T: MetricsTransport + ?Sized>(
    transport: &T,
    endpoint: &str,
    payload: &Payload,
) -> Result<()> {
    let mut last_error: Option<anyhow::Error> = None;
    for attempt in 0..MAX_ATTEMPTS {
        match transport.send(endpoint, payload).await {
            Ok(()) => return Ok(()),
            Err(err) => {
                last_error = Some(err);
                if attempt + 1 < MAX_ATTEMPTS {
                    let backoff = BASE_BACKOFF_MS * (1u64 << attempt);
                    tokio::time::sleep(Duration::from_millis(backoff)).await;
                }
            }
        }
    }
    Err(last_error.unwrap_or_else(|| anyhow::anyhow!("metrics send failed without recorded error")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;

    struct FailingTransport {
        attempts: Arc<AtomicU32>,
        fail_first_n: u32,
    }

    #[async_trait]
    impl MetricsTransport for FailingTransport {
        async fn send(&self, _endpoint: &str, _payload: &Payload) -> Result<()> {
            let attempt = self.attempts.fetch_add(1, Ordering::SeqCst) + 1;
            if attempt <= self.fail_first_n {
                anyhow::bail!("synthetic failure attempt {attempt}");
            }
            Ok(())
        }
    }

    fn empty_payload() -> Payload {
        Payload {
            install_id: "test".into(),
            animus_version: "0.0.0".into(),
            os: "test".into(),
            arch: "test".into(),
            events: Vec::new(),
        }
    }

    #[tokio::test]
    async fn retries_then_succeeds_within_budget() {
        let attempts = Arc::new(AtomicU32::new(0));
        let transport = FailingTransport { attempts: attempts.clone(), fail_first_n: 2 };
        let payload = empty_payload();
        let result = send_with_retry(&transport, "http://example.test", &payload).await;
        assert!(result.is_ok(), "should succeed on third attempt");
        assert_eq!(attempts.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn gives_up_after_max_attempts() {
        let attempts = Arc::new(AtomicU32::new(0));
        let transport = FailingTransport { attempts: attempts.clone(), fail_first_n: MAX_ATTEMPTS + 5 };
        let payload = empty_payload();
        let result = send_with_retry(&transport, "http://example.test", &payload).await;
        assert!(result.is_err(), "should bubble up the final error after exhausting attempts");
        assert_eq!(attempts.load(Ordering::SeqCst), MAX_ATTEMPTS);
    }

    #[test]
    fn payload_coalesces_repeated_events() {
        use chrono::TimeZone;
        let earlier = Utc.with_ymd_and_hms(2026, 6, 1, 10, 0, 0).unwrap();
        let later = Utc.with_ymd_and_hms(2026, 6, 1, 11, 0, 0).unwrap();
        let events = vec![
            PendingEvent {
                name: EventName::WorkflowStarted,
                tags: [("workflow_kind".to_string(), "task".to_string())].into_iter().collect(),
                recorded_at: later,
            },
            PendingEvent {
                name: EventName::WorkflowStarted,
                tags: [("workflow_kind".to_string(), "task".to_string())].into_iter().collect(),
                recorded_at: earlier,
            },
            PendingEvent { name: EventName::DaemonStarted, tags: BTreeMap::new(), recorded_at: later },
        ];
        let payload = build_payload("install", events);
        assert_eq!(payload.events.len(), 2);
        let workflow_event =
            payload.events.iter().find(|event| event.name == EventName::WorkflowStarted).expect("workflow event");
        assert_eq!(workflow_event.count, 2);
        assert_eq!(workflow_event.first_seen, earlier, "first_seen should track earliest timestamp");
    }
}
