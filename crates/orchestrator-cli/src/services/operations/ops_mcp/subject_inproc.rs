use super::daemon_inproc::resolve_project_root;
use super::exec_errors::build_inproc_tool_error_payload;
use super::{AoMcpServer, OnError, SubjectBatchCreateInput, SubjectBatchUpdateInput};
use crate::services::operations::{
    subject_create_application, subject_get_application, subject_list_application, subject_next_application,
    subject_status_application, subject_update_application, SubjectCreateRequest, SubjectGetRequest,
    SubjectListRequest, SubjectNextRequest, SubjectStatusRequest, SubjectUpdateRequest,
};
use anyhow::Result;
use rmcp::model::CallToolResult;
use serde_json::{json, Value};

const BATCH_RESULT_SCHEMA: &str = "animus.mcp.batch.result.v1";

impl AoMcpServer {
    async fn run_subject_application<F>(&self, tool_name: &str, project_root: Option<String>, call: F) -> CallToolResult
    where
        F: AsyncFnOnce(String, Option<&animus_actor::Actor>) -> Result<Value>,
    {
        self.audit_actor_tool_decision(tool_name, true, "forward");
        let project_root = resolve_project_root(&self.default_project_root, project_root);
        match call(project_root, self.pinned_actor()).await {
            Ok(result) => CallToolResult::structured(json!({ "tool": tool_name, "result": result })),
            Err(err) => CallToolResult::structured_error(build_inproc_tool_error_payload(tool_name, &err)),
        }
    }

    pub(super) async fn subject_list_inproc(
        &self,
        input: super::SubjectListInput,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let project_root = input.project_root.clone();
        let request = input.into();
        Ok(self
            .run_subject_application("animus.subject.list", project_root, async |root, actor| {
                Ok(serde_json::to_value(subject_list_application(request, &root, actor).await?)?)
            })
            .await)
    }

    pub(super) async fn subject_get_inproc(
        &self,
        input: super::SubjectGetInput,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let project_root = input.project_root.clone();
        let request = input.into();
        Ok(self
            .run_subject_application("animus.subject.get", project_root, async |root, actor| {
                Ok(serde_json::to_value(subject_get_application(request, &root, actor).await?)?)
            })
            .await)
    }

    pub(super) async fn subject_create_inproc(
        &self,
        input: super::SubjectCreateInput,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let project_root = input.project_root.clone();
        let request = input.into();
        Ok(self
            .run_subject_application("animus.subject.create", project_root, async |root, actor| {
                Ok(serde_json::to_value(subject_create_application(request, &root, actor).await?)?)
            })
            .await)
    }

    pub(super) async fn subject_update_inproc(
        &self,
        input: super::SubjectUpdateInput,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let project_root = input.project_root.clone();
        let request = input.into();
        Ok(self
            .run_subject_application("animus.subject.update", project_root, async |root, actor| {
                Ok(serde_json::to_value(subject_update_application(request, &root, actor).await?)?)
            })
            .await)
    }

    pub(super) async fn subject_next_inproc(
        &self,
        input: super::SubjectNextInput,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let project_root = input.project_root.clone();
        let request = input.into();
        Ok(self
            .run_subject_application("animus.subject.next", project_root, async |root, actor| {
                Ok(serde_json::to_value(subject_next_application(request, &root, actor).await?)?)
            })
            .await)
    }

    pub(super) async fn subject_status_inproc(
        &self,
        input: super::SubjectStatusInput,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let project_root = input.project_root.clone();
        let request = input.into();
        Ok(self
            .run_subject_application("animus.subject.status", project_root, async |root, actor| {
                Ok(serde_json::to_value(subject_status_application(request, &root, actor).await?)?)
            })
            .await)
    }

    pub(super) async fn subject_batch_create_inproc(
        &self,
        input: SubjectBatchCreateInput,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let project_root = resolve_project_root(&self.default_project_root, input.project_root);
        let calls = input.items.into_iter().map(|item| SubjectBatchCall {
            target_id: item.title.clone(),
            operation: SubjectBatchOperation::Create(SubjectCreateRequest {
                kind: input.kind.clone(),
                title: item.title,
                status: item.status,
                priority: item.priority,
                labels: item.labels,
                body: item.body,
                data: item.data,
            }),
        });
        self.audit_actor_tool_decision("animus.subject.batch-create", true, "forward");
        Ok(self
            .run_subject_batch_application(
                "animus.subject.batch-create",
                calls.collect(),
                &input.on_error,
                &project_root,
            )
            .await)
    }

    pub(super) async fn subject_batch_update_inproc(
        &self,
        input: SubjectBatchUpdateInput,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let project_root = resolve_project_root(&self.default_project_root, input.project_root);
        let calls = input.items.into_iter().map(|item| SubjectBatchCall {
            target_id: item.id.clone(),
            operation: SubjectBatchOperation::Update(SubjectUpdateRequest {
                kind: input.kind.clone(),
                id: item.id,
                title: None,
                status: item.status,
                priority: item.priority,
                labels: item.labels,
                body: None,
                data: item.data,
            }),
        });
        self.audit_actor_tool_decision("animus.subject.batch-update", true, "forward");
        Ok(self
            .run_subject_batch_application(
                "animus.subject.batch-update",
                calls.collect(),
                &input.on_error,
                &project_root,
            )
            .await)
    }

    async fn run_subject_batch_application(
        &self,
        tool_name: &str,
        calls: Vec<SubjectBatchCall>,
        on_error: &OnError,
        project_root: &str,
    ) -> CallToolResult {
        let requested = calls.len();
        let mut results = Vec::with_capacity(requested);
        let mut stopped = false;
        for (index, call) in calls.into_iter().enumerate() {
            if stopped {
                results.push(json!({
                    "index": index,
                    "status": "skipped",
                    "target_id": call.target_id,
                    "command": call.operation.name(),
                    "result": null,
                    "error": null,
                    "exit_code": null,
                    "reason": "stopped after earlier failure",
                }));
                continue;
            }
            let command = call.operation.name();
            match call.operation.execute(project_root, self.pinned_actor()).await {
                Ok(result) => results.push(json!({
                    "index": index,
                    "status": "success",
                    "target_id": call.target_id,
                    "command": command,
                    "result": result,
                    "exit_code": 0,
                })),
                Err(err) => {
                    let mut error = build_inproc_tool_error_payload(tool_name, &err);
                    error.as_object_mut().map(|map| map.remove("tool"));
                    results.push(json!({
                        "index": index,
                        "status": "failed",
                        "target_id": call.target_id,
                        "command": command,
                        "result": null,
                        "exit_code": error.get("exit_code").cloned().unwrap_or(Value::Null),
                        "error": error,
                    }));
                    stopped = *on_error == OnError::Stop;
                }
            }
        }
        let executed = results.iter().filter(|item| item["status"] != "skipped").count();
        let succeeded = results.iter().filter(|item| item["status"] == "success").count();
        let failed = results.iter().filter(|item| item["status"] == "failed").count();
        let skipped = results.iter().filter(|item| item["status"] == "skipped").count();
        let payload = json!({
            "schema": BATCH_RESULT_SCHEMA,
            "tool": tool_name,
            "on_error": on_error.as_str(),
            "summary": {
                "requested": requested,
                "executed": executed,
                "succeeded": succeeded,
                "failed": failed,
                "skipped": skipped,
                "completed": failed == 0,
            },
            "results": results,
        });
        CallToolResult::structured(payload)
    }
}

struct SubjectBatchCall {
    target_id: String,
    operation: SubjectBatchOperation,
}

enum SubjectBatchOperation {
    Create(SubjectCreateRequest),
    Update(SubjectUpdateRequest),
}

impl SubjectBatchOperation {
    fn name(&self) -> &'static str {
        match self {
            Self::Create(_) => "subject/create",
            Self::Update(_) => "subject/update",
        }
    }

    async fn execute(self, project_root: &str, actor: Option<&animus_actor::Actor>) -> Result<Value> {
        match self {
            Self::Create(request) => {
                Ok(serde_json::to_value(subject_create_application(request, project_root, actor).await?)?)
            }
            Self::Update(request) => {
                Ok(serde_json::to_value(subject_update_application(request, project_root, actor).await?)?)
            }
        }
    }
}

impl From<super::SubjectListInput> for SubjectListRequest {
    fn from(input: super::SubjectListInput) -> Self {
        Self { kind: input.kind, status: input.status, limit: input.limit, cursor: input.cursor, query: input.query }
    }
}

impl From<super::SubjectGetInput> for SubjectGetRequest {
    fn from(input: super::SubjectGetInput) -> Self {
        Self { kind: input.kind, id: input.id }
    }
}

impl From<super::SubjectCreateInput> for SubjectCreateRequest {
    fn from(input: super::SubjectCreateInput) -> Self {
        Self {
            kind: input.kind,
            title: input.title,
            status: input.status,
            priority: input.priority,
            labels: input.labels,
            body: input.body,
            data: input.data,
        }
    }
}

impl From<super::SubjectUpdateInput> for SubjectUpdateRequest {
    fn from(input: super::SubjectUpdateInput) -> Self {
        Self {
            kind: input.kind,
            id: input.id,
            title: input.title,
            status: input.status,
            priority: input.priority,
            labels: input.labels,
            body: input.body,
            data: input.data,
        }
    }
}

impl From<super::SubjectNextInput> for SubjectNextRequest {
    fn from(input: super::SubjectNextInput) -> Self {
        Self { kind: input.kind }
    }
}

impl From<super::SubjectStatusInput> for SubjectStatusRequest {
    fn from(input: super::SubjectStatusInput) -> Self {
        Self { kind: input.kind, id: input.id, status: input.status }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_create_conversion_preserves_nested_json_without_argv_round_trip() {
        let data = json!({
            "nested": { "count": 7, "enabled": true },
            "items": [1, "two", null],
        });
        let request: SubjectCreateRequest = super::super::SubjectCreateInput {
            kind: "transcript".to_string(),
            title: "Weekly sync".to_string(),
            status: None,
            priority: None,
            labels: vec!["meeting".to_string()],
            body: None,
            data: Some(data.clone()),
            project_root: None,
        }
        .into();

        assert_eq!(request.data, Some(data));
        assert_eq!(request.labels, vec!["meeting"]);
    }

    #[test]
    fn typed_update_conversion_preserves_all_application_fields() {
        let request: SubjectUpdateRequest = super::super::SubjectUpdateInput {
            kind: "task".to_string(),
            id: "TASK-971".to_string(),
            title: Some("Typed application services".to_string()),
            status: Some("in_progress".to_string()),
            priority: Some("p1".to_string()),
            labels: vec!["arena".to_string(), "dry".to_string()],
            body: Some("evidence".to_string()),
            data: Some(json!({ "owner": { "user_id": "alice" } })),
            project_root: None,
        }
        .into();

        assert_eq!(request.id, "TASK-971");
        assert_eq!(request.title.as_deref(), Some("Typed application services"));
        assert_eq!(request.labels, vec!["arena", "dry"]);
        assert_eq!(request.data.unwrap()["owner"]["user_id"], "alice");
    }

    #[tokio::test]
    async fn subject_list_rejects_invalid_kind_through_typed_error_contract() {
        let root = tempfile::tempdir().expect("project root");
        let server = super::super::new_ao_mcp_server(root.path().to_string_lossy().as_ref());
        let result = server
            .subject_list_inproc(super::super::SubjectListInput {
                kind: "task/escape".to_string(),
                status: None,
                limit: None,
                cursor: None,
                query: None,
                project_root: None,
            })
            .await
            .expect("tool transport succeeds");

        assert_eq!(result.is_error, Some(true));
        let payload = result.structured_content.expect("typed error payload");
        assert_eq!(payload.pointer("/error/code").and_then(Value::as_str), Some("invalid_input"));
        assert_eq!(payload.pointer("/error/exit_code").and_then(Value::as_i64), Some(2));
        assert_eq!(payload.pointer("/remediation/kind").and_then(Value::as_str), Some("invalid_input"));
    }
}
