mod daemon_handlers;
mod event_stream;
mod parsing;
mod projects_handlers;
mod queue_handlers;
mod requests;
mod requirements_handlers;
mod reviews_handlers;
mod skills_handlers;
mod system_handlers;
mod tasks_handlers;
mod vision_handlers;
mod workflows_handlers;

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use chrono::Utc;
use orchestrator_web_contracts::DaemonEventRecord;
use serde_json::Value;
use tokio::sync::broadcast;
use uuid::Uuid;

use crate::models::{WebApiContext, WebApiError};

pub(super) const EVENT_SCHEMA: &str = "ao.daemon.event.v1";
pub(super) const DEFAULT_UPDATED_BY: &str = "ao-web";
pub(super) const DEFAULT_REQUIREMENT_SOURCE: &str = "ao-web";

#[derive(Clone)]
pub struct WebApiService {
    context: Arc<WebApiContext>,
    event_tx: broadcast::Sender<DaemonEventRecord>,
    next_seq: Arc<AtomicU64>,
}

impl WebApiService {
    pub fn new(context: Arc<WebApiContext>) -> Self {
        let (event_tx, _event_rx) = broadcast::channel(1024);
        let max_seq = event_stream::read_max_seq_for_project(&context.project_root).unwrap_or(0);

        Self { context, event_tx, next_seq: Arc::new(AtomicU64::new(max_seq)) }
    }

    pub fn subscribe_events(&self) -> broadcast::Receiver<DaemonEventRecord> {
        self.event_tx.subscribe()
    }

    pub fn read_events_since(&self, after_seq: Option<u64>) -> Result<Vec<DaemonEventRecord>, WebApiError> {
        let mut records = event_stream::read_events_for_project(&self.context.project_root)?;
        if let Some(after_seq) = after_seq {
            records.retain(|record| record.seq > after_seq);
        }
        Ok(records)
    }

    fn publish_event(&self, event_type: &str, data: Value) {
        let next_seq = self.next_seq.fetch_add(1, Ordering::SeqCst) + 1;
        let record = DaemonEventRecord {
            schema: EVENT_SCHEMA.to_string(),
            id: Uuid::new_v4().to_string(),
            seq: next_seq,
            timestamp: Utc::now().to_rfc3339(),
            event_type: event_type.to_string(),
            project_root: Some(self.context.project_root.clone()),
            data,
        };

        let _ = self.event_tx.send(record);
    }
}
