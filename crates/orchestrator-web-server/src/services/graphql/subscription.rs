use async_graphql::{SimpleObject, Subscription};
use futures_core::Stream;
use orchestrator_web_api::WebApiService;
use orchestrator_web_contracts::DaemonEventRecord;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::StreamExt;

#[derive(SimpleObject, Debug, Clone)]
pub struct GqlDaemonEvent {
    pub id: String,
    pub seq: i32,
    pub timestamp: String,
    pub event_type: String,
    pub data: String,
}

impl From<DaemonEventRecord> for GqlDaemonEvent {
    fn from(r: DaemonEventRecord) -> Self {
        Self { id: r.id, seq: r.seq as i32, timestamp: r.timestamp, event_type: r.event_type, data: r.data.to_string() }
    }
}

pub struct SubscriptionRoot;

#[Subscription]
impl SubscriptionRoot {
    async fn daemon_events<'ctx>(
        &self,
        ctx: &async_graphql::Context<'ctx>,
        event_type: Option<String>,
    ) -> async_graphql::Result<impl Stream<Item = GqlDaemonEvent>> {
        let api = ctx.data::<WebApiService>()?;
        let rx = api.subscribe_events();
        let stream = BroadcastStream::new(rx).filter_map(move |result| match result {
            Ok(record) => {
                if let Some(ref et) = event_type {
                    if record.event_type != *et {
                        return None;
                    }
                }
                Some(GqlDaemonEvent::from(record))
            }
            Err(_) => None,
        });
        Ok(stream)
    }

    async fn task_events<'ctx>(
        &self,
        ctx: &async_graphql::Context<'ctx>,
        task_id: Option<String>,
    ) -> async_graphql::Result<impl Stream<Item = GqlDaemonEvent>> {
        let api = ctx.data::<WebApiService>()?;
        let rx = api.subscribe_events();
        let stream = BroadcastStream::new(rx).filter_map(move |result| match result {
            Ok(record) => {
                let is_task_event = record.event_type.contains("task");
                if !is_task_event {
                    return None;
                }
                if let Some(ref tid) = task_id {
                    let event_task_id = record.data.get("task_id").and_then(|v| v.as_str()).unwrap_or("");
                    if event_task_id != tid.as_str() {
                        return None;
                    }
                }
                Some(GqlDaemonEvent::from(record))
            }
            Err(_) => None,
        });
        Ok(stream)
    }

    async fn workflow_events<'ctx>(
        &self,
        ctx: &async_graphql::Context<'ctx>,
        workflow_id: Option<String>,
    ) -> async_graphql::Result<impl Stream<Item = GqlDaemonEvent>> {
        let api = ctx.data::<WebApiService>()?;
        let rx = api.subscribe_events();
        let stream = BroadcastStream::new(rx).filter_map(move |result| match result {
            Ok(record) => {
                let is_workflow_event = record.event_type.contains("workflow") || record.event_type.contains("phase");
                if !is_workflow_event {
                    return None;
                }
                if let Some(ref wid) = workflow_id {
                    let event_wf_id = record.data.get("workflow_id").and_then(|v| v.as_str()).unwrap_or("");
                    if event_wf_id != wid.as_str() {
                        return None;
                    }
                }
                Some(GqlDaemonEvent::from(record))
            }
            Err(_) => None,
        });
        Ok(stream)
    }
}
