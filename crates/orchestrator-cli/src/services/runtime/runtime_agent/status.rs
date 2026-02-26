use std::sync::Arc;

use anyhow::{anyhow, Result};
use orchestrator_core::services::ServiceHub;
use protocol::{
    AgentControlRequest, AgentControlResponse, AgentStatusErrorCode, AgentStatusQueryResponse,
    AgentStatusRequest, AgentStatusResponse, ModelId, ModelStatusRequest, ModelStatusResponse,
    RunId, RunnerStatusRequest, RunnerStatusResponse,
};
use tokio::io::{AsyncBufReadExt, BufReader};

use crate::{
    default_model_status_targets, print_model_status, print_value, read_agent_status,
    write_json_line, AgentControlArgs, AgentModelStatusArgs, AgentRunnerStatusArgs,
    AgentStatusArgs,
};

use super::connection::connect_runner_for_agent_command;

#[derive(Debug)]
enum AgentStatusLookup {
    Found(AgentStatusResponse),
    NotFound { message: String },
}

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
        Ok(AgentStatusLookup::Found(status)) => print_value(status, json),
        Ok(AgentStatusLookup::NotFound { message }) => Err(anyhow!(message)),
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
) -> Result<AgentStatusLookup> {
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

        if let Some(response) = parse_agent_status_query_line(line) {
            return Ok(response);
        }
    }

    Err(anyhow!("no agent status response received from runner"))
}

fn parse_agent_status_query_line(line: &str) -> Option<AgentStatusLookup> {
    if let Ok(response) = serde_json::from_str::<AgentStatusQueryResponse>(line) {
        return match response {
            AgentStatusQueryResponse::Status(status) => Some(AgentStatusLookup::Found(status)),
            AgentStatusQueryResponse::Error(error) => match error.code {
                AgentStatusErrorCode::NotFound => Some(AgentStatusLookup::NotFound {
                    message: error.message,
                }),
            },
        };
    }

    if let Ok(response) = serde_json::from_str::<AgentStatusResponse>(line) {
        return Some(AgentStatusLookup::Found(response));
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::{AgentStatus, AgentStatusErrorResponse, RunId, Timestamp};

    #[test]
    fn parse_agent_status_query_line_parses_typed_status_response() {
        let payload = AgentStatusQueryResponse::Status(AgentStatusResponse {
            run_id: RunId("run-123".to_string()),
            status: AgentStatus::Running,
            elapsed_ms: 10,
            started_at: Timestamp::now(),
            completed_at: None,
        });
        let line = serde_json::to_string(&payload).expect("serialize status payload");

        let parsed = parse_agent_status_query_line(&line).expect("status payload should parse");
        match parsed {
            AgentStatusLookup::Found(status) => {
                assert_eq!(status.run_id.0, "run-123");
                assert_eq!(status.status, AgentStatus::Running);
            }
            AgentStatusLookup::NotFound { .. } => panic!("expected found result"),
        }
    }

    #[test]
    fn parse_agent_status_query_line_parses_typed_not_found_error() {
        let payload = AgentStatusQueryResponse::Error(AgentStatusErrorResponse {
            run_id: RunId("run-missing".to_string()),
            code: AgentStatusErrorCode::NotFound,
            message: "run not found: run-missing".to_string(),
        });
        let line = serde_json::to_string(&payload).expect("serialize error payload");

        let parsed = parse_agent_status_query_line(&line).expect("error payload should parse");
        match parsed {
            AgentStatusLookup::NotFound { message } => {
                assert_eq!(message, "run not found: run-missing");
            }
            AgentStatusLookup::Found(_) => panic!("expected not_found result"),
        }
    }

    #[test]
    fn parse_agent_status_query_line_parses_legacy_status_shape() {
        let payload = AgentStatusResponse {
            run_id: RunId("run-legacy".to_string()),
            status: AgentStatus::Completed,
            elapsed_ms: 55,
            started_at: Timestamp::now(),
            completed_at: Some(Timestamp::now()),
        };
        let line = serde_json::to_string(&payload).expect("serialize legacy status payload");

        let parsed = parse_agent_status_query_line(&line).expect("legacy payload should parse");
        match parsed {
            AgentStatusLookup::Found(status) => {
                assert_eq!(status.run_id.0, "run-legacy");
                assert_eq!(status.status, AgentStatus::Completed);
            }
            AgentStatusLookup::NotFound { .. } => panic!("expected found result"),
        }
    }
}
