use std::sync::Arc;

use anyhow::Result;
use orchestrator_core::services::ServiceHub;

use crate::{
    print_value, read_agent_status, unavailable_error, AgentControlActionArg, AgentControlArgs, AgentStatusArgs,
};

pub(crate) fn agent_control_application(run_id: &str, action: AgentControlActionArg) -> Result<serde_json::Value> {
    let action = match action {
        AgentControlActionArg::Pause => "pause",
        AgentControlActionArg::Resume => "resume",
        AgentControlActionArg::Terminate => "terminate",
    };
    Err(unavailable_error(format!(
        "failed to connect to agent control: agent {action} is not exposed by the v0.5.3 provider-only model; \
         run_id={run_id}"
    )))
}

pub(crate) fn agent_status_application(
    project_root: &str,
    run_id: &str,
    jsonl_dir: Option<&str>,
) -> Result<serde_json::Value> {
    read_agent_status(project_root, run_id, jsonl_dir)
}

pub(super) async fn handle_agent_control(
    args: AgentControlArgs,
    _hub: Arc<dyn ServiceHub>,
    _project_root: &str,
    _json: bool,
) -> Result<()> {
    // v0.5.3: provider plugins do not yet expose a `pause/resume/terminate`
    // wire surface. The sidecar that previously implemented these was
    // removed when the agent-runner was deleted; the control-wire agent
    // surface returned `NotSupported` even before that. Surface
    // an `unavailable` error so scripted callers can detect that no
    // control endpoint is connected and degrade accordingly. Cancellation
    // can still be performed via process signal to the child PID.
    agent_control_application(&args.run_id, args.action).map(|_| ())
}

pub(super) async fn handle_agent_status(
    args: AgentStatusArgs,
    _hub: Arc<dyn ServiceHub>,
    project_root: &str,
    json: bool,
) -> Result<()> {
    // v0.5.3: there is no in-process run registry anymore. Status replies
    // come exclusively from the persisted JSONL log under the scoped
    // runs directory.
    print_value(agent_status_application(project_root, &args.run_id, args.jsonl_dir.as_deref())?, json)
}
