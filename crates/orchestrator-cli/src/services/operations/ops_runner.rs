use super::{read_json_or_default, write_json_pretty};
use crate::cli_types::{RunnerCommand, RunnerOrphanCommand};
use crate::print_value;
use crate::shared::{connect_runner, runner_config_dir, write_json_line};
use anyhow::Result;
use orchestrator_core::ServiceHub;
use protocol::{kill_process, process_exists, RunnerStatusRequest, RunnerStatusResponse};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, BufReader};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct CliTrackerStateCli {
    #[serde(default)]
    processes: HashMap<String, i32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RunnerOrphanCli {
    run_id: String,
    pid: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RunnerOrphanDetectionCli {
    orphans: Vec<RunnerOrphanCli>,
    count: usize,
}

fn load_cli_tracker() -> Result<CliTrackerStateCli> {
    read_json_or_default(&protocol::cli_tracker_path())
}

fn save_cli_tracker(tracker: &CliTrackerStateCli) -> Result<()> {
    write_json_pretty(&protocol::cli_tracker_path(), tracker)
}

async fn query_runner_status_direct(project_root: &str) -> Option<RunnerStatusResponse> {
    let config_dir = runner_config_dir(Path::new(project_root));
    let stream = connect_runner(&config_dir).await.ok()?;
    let (read_half, mut write_half) = tokio::io::split(stream);
    write_json_line(&mut write_half, &RunnerStatusRequest::default())
        .await
        .ok()?;
    let mut lines = BufReader::new(read_half).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Ok(response) = serde_json::from_str::<RunnerStatusResponse>(line) {
            return Some(response);
        }
    }
    None
}

pub(crate) async fn handle_runner(
    command: RunnerCommand,
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
    json: bool,
) -> Result<()> {
    match command {
        RunnerCommand::Health => {
            let daemon_health = hub.daemon().health().await.ok();
            let runner_status = query_runner_status_direct(project_root).await;
            print_value(
                serde_json::json!({
                    "daemon_health": daemon_health,
                    "runner_status": runner_status,
                    "runner_connected": runner_status.is_some(),
                }),
                json,
            )
        }
        RunnerCommand::Orphans { command } => match command {
            RunnerOrphanCommand::Detect => {
                let tracker = load_cli_tracker()?;
                let orphans: Vec<_> = tracker
                    .processes
                    .into_iter()
                    .filter_map(|(run_id, pid)| {
                        if process_exists(pid) {
                            Some(RunnerOrphanCli { run_id, pid })
                        } else {
                            None
                        }
                    })
                    .collect();
                let detection = RunnerOrphanDetectionCli {
                    count: orphans.len(),
                    orphans,
                };
                print_value(detection, json)
            }
            RunnerOrphanCommand::Cleanup(args) => {
                let mut tracker = load_cli_tracker()?;
                let mut cleaned = Vec::new();
                for run_id in args.run_id {
                    let Some(pid) = tracker.processes.get(&run_id).copied() else {
                        continue;
                    };
                    if !process_exists(pid) || kill_process(pid) {
                        cleaned.push(run_id.clone());
                        tracker.processes.remove(&run_id);
                    }
                }
                save_cli_tracker(&tracker)?;
                print_value(serde_json::json!({ "cleaned_run_ids": cleaned }), json)
            }
        },
        RunnerCommand::RestartStats => {
            let path = crate::services::runtime::daemon_events_log_path();
            let mut starts = 0usize;
            let mut stops = 0usize;
            let mut crashes = 0usize;
            if path.exists() {
                let content = fs::read_to_string(path)?;
                for line in content.lines() {
                    let Ok(record) =
                        serde_json::from_str::<crate::services::runtime::DaemonEventRecord>(line)
                    else {
                        continue;
                    };
                    if record.event_type == "status" {
                        match record
                            .data
                            .get("status")
                            .and_then(|value| value.as_str())
                            .unwrap_or("")
                        {
                            "running" => starts = starts.saturating_add(1),
                            "stopped" => stops = stops.saturating_add(1),
                            "crashed" => crashes = crashes.saturating_add(1),
                            _ => {}
                        }
                    }
                }
            }
            print_value(
                serde_json::json!({
                    "starts": starts,
                    "stops": stops,
                    "crashes": crashes,
                }),
                json,
            )
        }
    }
}
