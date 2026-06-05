use super::{read_json_or_default, write_json_pretty};
use crate::cli_types::{RunnerCommand, RunnerOrphanCommand};
use crate::print_value;
use crate::services::runtime::runtime_agent::provider_client;
use anyhow::Result;
use fs2::FileExt;
use orchestrator_core::ServiceHub;
use protocol::{kill_process, process_exists};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::path::Path;
use std::sync::Arc;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RunnerOrphanCli {
    run_id: String,
    pid: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RunnerOrphanDetectionCli {
    orphans: Vec<RunnerOrphanCli>,
    count: usize,
}

fn load_cli_tracker() -> Result<HashMap<String, u32>> {
    read_json_or_default(&protocol::cli_tracker_path())
}

fn save_cli_tracker(tracker: &HashMap<String, u32>) -> Result<()> {
    write_json_pretty(&protocol::cli_tracker_path(), tracker)
}

/// Acquire an exclusive file lock on the CLI tracker for atomic read-modify-write.
fn acquire_tracker_lock() -> Result<std::fs::File> {
    let tracker_path = protocol::cli_tracker_path();
    if let Some(parent) = tracker_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let lock_path = tracker_path.with_extension("lock");
    let lock_file = OpenOptions::new().create(true).write(true).truncate(false).open(&lock_path)?;
    lock_file.lock_exclusive()?;
    Ok(lock_file)
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
            let providers = provider_client::health_snapshot(Path::new(project_root));
            let any_healthy = providers.iter().any(|p| p.installed);
            print_value(
                serde_json::json!({
                    "daemon_health": daemon_health,
                    "providers": providers,
                    "provider_plugins_healthy": any_healthy,
                }),
                json,
            )
        }
        RunnerCommand::Orphans { command } => match command {
            RunnerOrphanCommand::Detect => {
                let tracker = load_cli_tracker()?;
                let orphans: Vec<_> =
                    tracker
                        .into_iter()
                        .filter_map(|(run_id, pid)| {
                            if process_exists(pid as i32) {
                                Some(RunnerOrphanCli { run_id, pid })
                            } else {
                                None
                            }
                        })
                        .collect();
                let detection = RunnerOrphanDetectionCli { count: orphans.len(), orphans };
                print_value(detection, json)
            }
            RunnerOrphanCommand::Cleanup(args) => {
                let _lock = acquire_tracker_lock()?;
                let mut tracker = load_cli_tracker()?;
                let mut cleaned = Vec::new();
                for run_id in args.run_id {
                    let Some(pid) = tracker.get(&run_id).copied() else {
                        continue;
                    };
                    if !process_exists(pid as i32) || kill_process(pid as i32) {
                        cleaned.push(run_id.clone());
                        tracker.remove(&run_id);
                    }
                }
                save_cli_tracker(&tracker)?;
                print_value(serde_json::json!({ "cleaned_run_ids": cleaned }), json)
            }
        },
    }
}
