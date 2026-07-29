use super::AoMcpServer;
use animus_actor::Actor;
use orchestrator_daemon_runtime::{Audit, AuditActor, AuditEvent, AuditEventKind};
use serde_json::json;

impl AoMcpServer {
    /// The transport-asserted caller identity this server is bound to (from
    /// `animus mcp serve --actor-json`, relayed by the workflow runner from the
    /// authenticated run), or `None` for a global-scope / local server.
    pub(super) fn pinned_actor(&self) -> Option<&Actor> {
        self.pinned_actor.as_ref()
    }

    #[allow(dead_code)]
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
}
