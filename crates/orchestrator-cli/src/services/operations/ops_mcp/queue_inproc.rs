use super::daemon_inproc::resolve_project_root;
use super::exec_errors::build_inproc_tool_error_payload;
use super::{AoMcpServer, QueueEnqueueInput};
use crate::services::operations::{
    queue_bulk_application, queue_enqueue_application, queue_list_application, queue_reorder_application,
    queue_stats_application, QueueBulkVerb, QueueEnqueueRequest,
};
use anyhow::Result;
use rmcp::model::CallToolResult;
use serde_json::{json, Value};

impl AoMcpServer {
    async fn run_queue_application<F>(&self, tool_name: &str, project_root: Option<String>, call: F) -> CallToolResult
    where
        F: AsyncFnOnce(String) -> Result<Value>,
    {
        let project_root = resolve_project_root(&self.default_project_root, project_root);
        match call(project_root).await {
            Ok(result) => CallToolResult::structured(json!({ "tool": tool_name, "result": result })),
            Err(err) => CallToolResult::structured_error(build_inproc_tool_error_payload(tool_name, &err)),
        }
    }

    pub(super) async fn queue_list_inproc(
        &self,
        project_root: Option<String>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        Ok(self
            .run_queue_application("animus.queue.list", project_root, async |root| {
                Ok(serde_json::to_value(queue_list_application(&root).await?)?)
            })
            .await)
    }

    pub(super) async fn queue_stats_inproc(
        &self,
        project_root: Option<String>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        Ok(self
            .run_queue_application("animus.queue.stats", project_root, async |root| {
                Ok(serde_json::to_value(queue_stats_application(&root).await?)?)
            })
            .await)
    }

    pub(super) async fn queue_enqueue_inproc(
        &self,
        input: QueueEnqueueInput,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let project_root = input.project_root.clone();
        Ok(self
            .run_queue_application("animus.queue.enqueue", project_root, async |root| {
                queue_enqueue_application(queue_enqueue_request(input)?, &root).await
            })
            .await)
    }

    pub(super) async fn queue_reorder_inproc(
        &self,
        input: super::QueueReorderInput,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let project_root = input.project_root.clone();
        Ok(self
            .run_queue_application("animus.queue.reorder", project_root, async |root| {
                queue_reorder_application(input.subject_ids, &root).await
            })
            .await)
    }

    pub(super) async fn queue_bulk_inproc(
        &self,
        tool_name: &'static str,
        verb: QueueBulkVerb,
        input: super::QueueSubjectInput,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let project_root = input.project_root.clone();
        Ok(self
            .run_queue_application(tool_name, project_root, async |root| {
                let mut subject_ids = input.subject_ids;
                if let Some(subject_id) = input.subject_id {
                    subject_ids.insert(0, subject_id);
                }
                if subject_ids.is_empty() {
                    return Err(crate::invalid_input_error("at least one subject_id is required"));
                }
                let response = queue_bulk_application(verb, subject_ids, false, &root).await?;
                if let Some(error) = response.failure_error()? {
                    return Err(error);
                }
                response.payload()
            })
            .await)
    }
}

fn queue_enqueue_request(input: QueueEnqueueInput) -> Result<QueueEnqueueRequest> {
    if input.title.is_some() && input.subject_id.is_some() {
        return Err(crate::invalid_input_error("title and subject_id are mutually exclusive"));
    }
    if input.input.is_some() && input.input_json.is_some() {
        return Err(crate::invalid_input_error("input and input_json are mutually exclusive"));
    }
    if input.expire_after.is_some() && input.run_at.is_none() {
        return Err(crate::invalid_input_error("expire_after requires run_at"));
    }
    let typed_input = match (input.input, input.input_json) {
        (Some(value), None) => Some(value),
        (None, Some(raw)) => {
            Some(serde_json::from_str(&raw).map_err(|err| crate::invalid_input_error(format!("input_json: {err}")))?)
        }
        (None, None) => None,
        (Some(_), Some(_)) => unreachable!("conflict validated above"),
    };
    let expire_after_secs = input
        .expire_after
        .as_deref()
        .map(crate::cli_types::parse_duration_secs_default_seconds)
        .transpose()
        .map_err(crate::invalid_input_error)?;
    Ok(QueueEnqueueRequest {
        title: input.title,
        subject_id: input.subject_id,
        description: input.description,
        workflow_ref: input.workflow_ref,
        input: typed_input,
        run_at: input.run_at,
        expire_after_secs,
        adhoc: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input() -> QueueEnqueueInput {
        QueueEnqueueInput {
            title: None,
            subject_id: Some("task:TASK-971".to_string()),
            description: None,
            workflow_ref: Some("coding".to_string()),
            input_json: None,
            input: None,
            run_at: None,
            expire_after: None,
            project_root: None,
        }
    }

    #[test]
    fn typed_enqueue_preserves_nested_input_without_argv_round_trip() {
        let mut input = input();
        input.input = Some(json!({ "nested": { "count": 7 }, "items": [true, null, "x"] }));
        let request = queue_enqueue_request(input).expect("typed request");
        assert_eq!(request.input.unwrap()["nested"]["count"], 7);
    }

    #[test]
    fn compatibility_json_is_parsed_once_and_conflicts_are_rejected() {
        let mut compatible = input();
        compatible.input_json = Some(r#"{"owner":{"user_id":"alice"}}"#.to_string());
        assert_eq!(queue_enqueue_request(compatible).unwrap().input.unwrap()["owner"]["user_id"], "alice");

        let mut conflicting = input();
        conflicting.input = Some(json!({}));
        conflicting.input_json = Some("{}".to_string());
        assert!(queue_enqueue_request(conflicting).unwrap_err().to_string().contains("mutually exclusive"));
    }

    #[test]
    fn expire_after_requires_run_at_and_is_converted_to_seconds() {
        let mut missing_at = input();
        missing_at.expire_after = Some("10m".to_string());
        assert!(queue_enqueue_request(missing_at).unwrap_err().to_string().contains("requires run_at"));

        let mut valid = input();
        valid.run_at = Some("30m".to_string());
        valid.expire_after = Some("10m".to_string());
        assert_eq!(queue_enqueue_request(valid).unwrap().expire_after_secs, Some(600));
    }

    #[tokio::test]
    async fn enqueue_validation_surfaces_typed_mcp_error_without_subprocess() {
        let root = tempfile::tempdir().expect("project root");
        let server = super::super::new_ao_mcp_server(root.path().to_string_lossy().as_ref());
        let mut missing_subject = input();
        missing_subject.subject_id = None;
        let result = server.queue_enqueue_inproc(missing_subject).await.expect("tool transport succeeds");

        assert_eq!(result.is_error, Some(true));
        let payload = result.structured_content.expect("typed error payload");
        assert_eq!(payload.pointer("/error/code").and_then(Value::as_str), Some("invalid_input"));
        assert!(payload.pointer("/error/message").and_then(Value::as_str).unwrap().contains("no subject specified"));
    }
}
