use std::sync::Arc;

use anyhow::{anyhow, Result};
use orchestrator_core::services::ServiceHub;
use protocol::{
    AgentControlRequest, AgentControlResponse, AgentStatusRequest, AgentStatusResponse, ModelId,
    ModelStatusRequest, ModelStatusResponse, RunId, RunnerStatusRequest, RunnerStatusResponse,
};
use tokio::io::{AsyncBufReadExt, BufReader};

use crate::{
    default_model_status_targets, print_model_status, print_value, read_agent_status,
    write_json_line, AgentControlArgs, AgentModelStatusArgs, AgentRunnerStatusArgs,
    AgentStatusArgs,
};

use super::connection::connect_runner_for_agent_command;

pub(super) async fn handle_agent_control(
    args: AgentControlArgs,
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
    json: bool,
) -> Result<()> {
    let stream = connect_runner_for_agent_command(&hub, project_root, args.start_runner).await?;
    let (read_half, mut write_half) = tokio::io::split(stream);

    let request = AgentControlRequest {
        run_id: RunId(args.run_id),
        action: args.action.into(),
    };
    write_json_line(&mut write_half, &request).await?;

    let mut lines = BufReader::new(read_half).lines();
    while let Some(line) = lines.next_line().await? {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        if let Ok(response) = serde_json::from_str::<AgentControlResponse>(line) {
            if response.success {
                return print_value(response, json);
            }
            return Err(anyhow!(
                "{}",
                response
                    .message
                    .unwrap_or_else(|| "agent control request failed".to_string())
            ));
        }

        if serde_json::from_str::<RunnerStatusResponse>(line).is_ok() {
            return Err(anyhow!(
                "runner returned status payload while waiting for control response; ensure agent-runner is up to date"
            ));
        }
    }

    Err(anyhow!("no control response received from runner"))
}

pub(super) async fn handle_agent_model_status(
    args: AgentModelStatusArgs,
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
    json: bool,
) -> Result<()> {
    let stream = connect_runner_for_agent_command(&hub, project_root, args.start_runner).await?;
    let (read_half, mut write_half) = tokio::io::split(stream);

    let models = if args.models.is_empty() {
        default_model_status_targets()
            .into_iter()
            .map(ModelId)
            .collect()
    } else {
        args.models.into_iter().map(ModelId).collect()
    };

    write_json_line(&mut write_half, &ModelStatusRequest { models }).await?;

    let mut lines = BufReader::new(read_half).lines();
    while let Some(line) = lines.next_line().await? {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        if let Ok(response) = serde_json::from_str::<ModelStatusResponse>(line) {
            if json {
                return print_value(response, true);
            }
            for status in response.statuses {
                print_model_status(status);
            }
            return Ok(());
        }
    }

    Err(anyhow!("no model status response received from runner"))
}

pub(super) async fn handle_agent_runner_status(
    args: AgentRunnerStatusArgs,
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
    json: bool,
) -> Result<()> {
    let stream = connect_runner_for_agent_command(&hub, project_root, args.start_runner).await?;
    let (read_half, mut write_half) = tokio::io::split(stream);

    write_json_line(&mut write_half, &RunnerStatusRequest::default()).await?;

    let mut lines = BufReader::new(read_half).lines();
    while let Some(line) = lines.next_line().await? {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        if let Ok(response) = serde_json::from_str::<RunnerStatusResponse>(line) {
            return print_value(response, json);
        }
    }

    Err(anyhow!("no runner status response received from runner"))
}

pub(super) async fn handle_agent_status(
    args: AgentStatusArgs,
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
    json: bool,
) -> Result<()> {
    match query_agent_status_from_runner(&hub, project_root, &args.run_id, args.start_runner).await
    {
        Ok(status) => print_value(status, json),
        Err(_) => {
            let status = read_agent_status(project_root, &args.run_id, args.jsonl_dir.as_deref())?;
            print_value(status, json)
        }
    }
}

async fn query_agent_status_from_runner(
    hub: &Arc<dyn ServiceHub>,
    project_root: &str,
    run_id: &str,
    start_runner: bool,
) -> Result<AgentStatusResponse> {
    let stream = connect_runner_for_agent_command(hub, project_root, start_runner).await?;
    let (read_half, mut write_half) = tokio::io::split(stream);

    let request = AgentStatusRequest {
        run_id: RunId(run_id.to_string()),
    };
    write_json_line(&mut write_half, &request).await?;

    let mut lines = BufReader::new(read_half).lines();
    while let Some(line) = lines.next_line().await? {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        if let Ok(response) = serde_json::from_str::<AgentStatusResponse>(line) {
            return Ok(response);
        }
    }

    Err(anyhow!("no agent status response received from runner"))
}
