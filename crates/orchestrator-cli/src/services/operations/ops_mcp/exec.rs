use super::exec_errors::{batch_item_error_from_result, build_tool_error_payload, extract_cli_success_data};
use super::{build_guarded_list_result, AoMcpServer, BatchItemExec, ListGuardInput, OnError, BATCH_RESULT_SCHEMA};
use anyhow::Result;
use rmcp::{model::CallToolResult, ErrorData as McpError};
use serde_json::{json, Value};

impl AoMcpServer {
    pub(super) async fn run_tool(
        &self,
        tool_name: &str,
        requested_args: Vec<String>,
        project_root_override: Option<String>,
    ) -> Result<CallToolResult, McpError> {
        match self.execute_ao(requested_args, project_root_override).await {
            Ok(result) => {
                if result.success {
                    let data = extract_cli_success_data(result.stdout_json);

                    Ok(CallToolResult::structured(json!({
                        "tool": tool_name,
                        "result": data,
                    })))
                } else {
                    Ok(CallToolResult::structured_error(build_tool_error_payload(tool_name, &result)))
                }
            }
            Err(err) => Ok(CallToolResult::structured_error(json!({
                "tool": tool_name,
                "error": err.to_string(),
            }))),
        }
    }

    pub(super) async fn run_list_tool(
        &self,
        tool_name: &str,
        requested_args: Vec<String>,
        project_root_override: Option<String>,
        guard: ListGuardInput,
    ) -> Result<CallToolResult, McpError> {
        match self.execute_ao(requested_args, project_root_override).await {
            Ok(result) => {
                if result.success {
                    let data = extract_cli_success_data(result.stdout_json);
                    match build_guarded_list_result(tool_name, data, guard) {
                        Ok(shaped) => Ok(CallToolResult::structured(json!({
                            "tool": tool_name,
                            "result": shaped,
                        }))),
                        Err(error) => Ok(CallToolResult::structured_error(json!({
                            "tool": tool_name,
                            "error": error.to_string(),
                        }))),
                    }
                } else {
                    Ok(CallToolResult::structured_error(build_tool_error_payload(tool_name, &result)))
                }
            }
            Err(err) => Ok(CallToolResult::structured_error(json!({
                "tool": tool_name,
                "error": err.to_string(),
            }))),
        }
    }

    pub(super) async fn run_batch_tool(
        &self,
        tool_name: &str,
        items: Vec<BatchItemExec>,
        on_error: &OnError,
        project_root_override: Option<String>,
    ) -> Result<CallToolResult, McpError> {
        let result =
            run_batch_items(tool_name, items, on_error, |args| self.execute_ao(args, project_root_override.clone()))
                .await;
        Ok(CallToolResult::structured(result))
    }
}

/// Execute batch items one at a time through `exec`, honoring `on_error`
/// semantics (stop = remaining items are marked skipped and never executed;
/// continue = every item runs), and assemble the
/// `animus.mcp.batch.result.v1` envelope. Extracted from `run_batch_tool`
/// so the dispatch loop is unit-testable without spawning CLI subprocesses.
pub(super) async fn run_batch_items<F, Fut>(
    tool_name: &str,
    items: Vec<BatchItemExec>,
    on_error: &OnError,
    mut exec: F,
) -> Value
where
    F: FnMut(Vec<String>) -> Fut,
    Fut: std::future::Future<Output = Result<super::CliExecutionResult>>,
{
    let requested = items.len();
    let mut outcomes: Vec<Value> = Vec::with_capacity(requested);
    let mut stopped = false;

    for (index, item) in items.into_iter().enumerate() {
        if stopped {
            outcomes.push(json!({
                "index": index,
                "status": "skipped",
                "target_id": item.target_id,
                "command": item.command,
                "result": null,
                "error": null,
                "exit_code": null,
                "reason": "stopped after earlier failure",
            }));
            continue;
        }

        match exec(item.args).await {
            Ok(exec_result) => {
                if exec_result.success {
                    let data = extract_cli_success_data(exec_result.stdout_json);
                    outcomes.push(json!({
                        "index": index,
                        "status": "success",
                        "target_id": item.target_id,
                        "command": item.command,
                        "result": data,
                        "exit_code": exec_result.exit_code,
                    }));
                } else {
                    let error = batch_item_error_from_result(&exec_result);
                    outcomes.push(json!({
                        "index": index,
                        "status": "failed",
                        "target_id": item.target_id,
                        "command": item.command,
                        "result": null,
                        "error": error,
                        "exit_code": exec_result.exit_code,
                    }));
                    if *on_error == OnError::Stop {
                        stopped = true;
                    }
                }
            }
            Err(err) => {
                outcomes.push(json!({
                    "index": index,
                    "status": "failed",
                    "target_id": item.target_id,
                    "command": item.command,
                    "result": null,
                    "error": { "error": err.to_string() },
                    "exit_code": null,
                }));
                if *on_error == OnError::Stop {
                    stopped = true;
                }
            }
        }
    }

    let executed = outcomes.iter().filter(|o| o["status"] != "skipped").count();
    let succeeded = outcomes.iter().filter(|o| o["status"] == "success").count();
    let failed = outcomes.iter().filter(|o| o["status"] == "failed").count();
    let skipped = outcomes.iter().filter(|o| o["status"] == "skipped").count();

    json!({
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
        "results": outcomes,
    })
}
