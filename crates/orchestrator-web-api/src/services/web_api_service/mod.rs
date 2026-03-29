mod daemon_handlers;
mod event_stream;
mod now_handlers;
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
use orchestrator_core::{ListPageRequest, RequirementQuery, TaskQuery, WorkflowQuery};
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

    #[allow(clippy::too_many_arguments)]
    pub fn build_task_query(
        &self,
        task_type: Option<String>,
        status: Option<String>,
        priority: Option<String>,
        risk: Option<String>,
        assignee_type: Option<String>,
        tags: Vec<String>,
        linked_requirement: Option<String>,
        linked_architecture_entity: Option<String>,
        search: Option<String>,
        page: ListPageRequest,
        sort: Option<String>,
    ) -> Result<TaskQuery, WebApiError> {
        Ok(TaskQuery {
            filter: parsing::build_task_filter(
                task_type,
                status,
                priority,
                risk,
                assignee_type,
                tags,
                linked_requirement,
                linked_architecture_entity,
                search,
            )?,
            page,
            sort: parsing::parse_task_query_sort_opt(sort.as_deref())?.unwrap_or_default(),
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn build_requirement_query(
        &self,
        status: Option<String>,
        priority: Option<String>,
        category: Option<String>,
        requirement_type: Option<String>,
        tags: Vec<String>,
        linked_task_id: Option<String>,
        search: Option<String>,
        page: ListPageRequest,
        sort: Option<String>,
    ) -> Result<RequirementQuery, WebApiError> {
        Ok(RequirementQuery {
            filter: parsing::build_requirement_filter(
                status,
                priority,
                category,
                requirement_type,
                tags,
                linked_task_id,
                search,
            )?,
            page,
            sort: parsing::parse_requirement_query_sort_opt(sort.as_deref())?.unwrap_or_default(),
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn build_workflow_query(
        &self,
        status: Option<String>,
        workflow_ref: Option<String>,
        task_id: Option<String>,
        phase_id: Option<String>,
        search: Option<String>,
        page: ListPageRequest,
        sort: Option<String>,
    ) -> Result<WorkflowQuery, WebApiError> {
        Ok(WorkflowQuery {
            filter: parsing::build_workflow_filter(status, workflow_ref, task_id, phase_id, search)?,
            page,
            sort: parsing::parse_workflow_query_sort_opt(sort.as_deref())?.unwrap_or_default(),
        })
    }
}
