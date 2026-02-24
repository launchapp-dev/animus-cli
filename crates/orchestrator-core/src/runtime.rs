use std::sync::Arc;

use anyhow::Result;
use tokio::sync::{broadcast, RwLock};

use crate::config::{resolve_project_root, RuntimeConfig};
use crate::events::{OrchestratorEvent, OrchestratorEventKind};
use crate::types::DaemonStatus;

pub trait EventSink: Send + Sync {
    fn emit(&self, event: OrchestratorEvent);
}

#[derive(Clone)]
pub struct RuntimeHandle {
    status: Arc<RwLock<DaemonStatus>>,
    events_tx: broadcast::Sender<OrchestratorEvent>,
}

impl RuntimeHandle {
    pub async fn shutdown(&self) {
        self.set_status(DaemonStatus::Stopping).await;
        self.set_status(DaemonStatus::Stopped).await;
    }

    pub async fn status(&self) -> DaemonStatus {
        *self.status.read().await
    }

    pub async fn pause(&self) {
        self.set_status(DaemonStatus::Paused).await;
    }

    pub async fn resume(&self) {
        self.set_status(DaemonStatus::Running).await;
    }

    pub fn subscribe_events(&self) -> broadcast::Receiver<OrchestratorEvent> {
        self.events_tx.subscribe()
    }

    pub async fn set_status(&self, status: DaemonStatus) {
        {
            let mut lock = self.status.write().await;
            *lock = status;
        }

        let _ = self.events_tx.send(OrchestratorEvent::new(
            OrchestratorEventKind::DaemonStatusChanged,
            serde_json::json!({"status": status}),
        ));
    }
}

pub struct OrchestratorRuntime;

impl OrchestratorRuntime {
    pub async fn start(config: RuntimeConfig) -> Result<RuntimeHandle> {
        let (_root, _source) = resolve_project_root(&config);
        let (events_tx, _) = broadcast::channel(256);

        let handle = RuntimeHandle {
            status: Arc::new(RwLock::new(DaemonStatus::Starting)),
            events_tx,
        };

        handle.set_status(DaemonStatus::Running).await;
        Ok(handle)
    }

    pub async fn start_with_sink(
        config: RuntimeConfig,
        sink: Arc<dyn EventSink>,
    ) -> Result<RuntimeHandle> {
        let handle = Self::start(config).await?;
        let mut rx = handle.subscribe_events();

        tokio::spawn(async move {
            while let Ok(event) = rx.recv().await {
                sink.emit(event);
            }
        });

        Ok(handle)
    }
}
