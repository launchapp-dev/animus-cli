use super::exec_errors::{batch_item_error_from_result, build_tool_error_payload, extract_cli_success_data};
use super::{AoMcpServer, BatchItemExec, OnError, BATCH_RESULT_SCHEMA};
use animus_actor::Actor;
use anyhow::Result;
use orchestrator_daemon_runtime::{Audit, AuditActor, AuditEvent, AuditEventKind};
use rmcp::{model::CallToolResult, ErrorData as McpError};
use serde_json::{json, Value};

impl AoMcpServer {
    /// The transport-asserted caller identity this server is bound to (from
    /// `animus mcp serve --actor-json`, relayed by the workflow runner from the
    /// authenticated run), or `None` for a global-scope / local server.
    ///
    /// Every tool call routes through [`Self::run_tool`] / [`Self::run_list_tool`],
    /// which surface this for per-user audit. Actor-aware child CLI commands
    /// receive the same identity via their existing `--actor-json` boundary.
    /// Commands whose protocols do not yet carry Actor remain unchanged rather
    /// than pretending to enforce a scope they cannot represent.
    pub(super) fn pinned_actor(&self) -> Option<&Actor> {
        self.pinned_actor.as_ref()
    }

    pub(super) fn audit_actor_tool_invocation(&self, tool_name: &str, requested_args: &[String]) {
        let command_actor_aware = super::ao_exec::command_accepts_actor(requested_args);
        self.audit_actor_tool_decision(
            tool_name,
            command_actor_aware,
            if command_actor_aware { "forward" } else { "deny" },
        );
    }

    pub(super) fn audit_actor_tool_decision(&self, tool_name: &str, command_actor_aware: bool, decision: &str) {
        let Some(actor) = self.pinned_actor() else {
            return;
        };
        Audit::at_scoped_root(&self.scoped_state_root()).log_event(AuditEvent::new(
            AuditActor::Principal { id: actor.user_id.clone(), kind: "user" },
            AuditEventKind::McpToolInvocation,
            json!({
                "tool": tool_name,
                "tenant_id": actor.tenant_id,
                "command_actor_aware": command_actor_aware,
                "decision": decision,
            }),
        ));
    }

    pub(super) async fn run_tool(
        &self,
        tool_name: &str,
        requested_args: Vec<String>,
        project_root_override: Option<String>,
    ) -> Result<CallToolResult, McpError> {
        // Per-user audit: record which actor (if any) this tool call runs as.
        // The actor reaches this server per-agent-spawn via `--actor-json`; this
        // is the single choke point every typed tool routes through.
        if let Some(actor) = self.pinned_actor() {
            tracing::debug!(
                tool = tool_name,
                actor_user = %actor.user_id,
                actor_tenant = ?actor.tenant_id,
                "MCP tool invoked for actor"
            );
        }
        self.audit_actor_tool_invocation(tool_name, &requested_args);
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

    pub(super) async fn run_batch_tool(
        &self,
        tool_name: &str,
        items: Vec<BatchItemExec>,
        on_error: &OnError,
        project_root_override: Option<String>,
    ) -> Result<CallToolResult, McpError> {
        let result = run_batch_items(tool_name, items, on_error, |args| {
            self.audit_actor_tool_invocation(tool_name, &args);
            self.execute_ao(args, project_root_override.clone())
        })
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
