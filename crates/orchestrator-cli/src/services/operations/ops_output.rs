use crate::cli_types::OutputCommand;
use crate::print_value;
use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ArtifactInfoCli {
    artifact_id: String,
    artifact_type: String,
    #[serde(default)]
    file_path: Option<String>,
    #[serde(default)]
    size_bytes: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RunJsonlEntryCli {
    source_file: String,
    line: String,
    #[serde(default)]
    timestamp_hint: Option<String>,
}

fn run_dir_candidates(project_root: &str, run_id: &str) -> Vec<PathBuf> {
    vec![
        Path::new(project_root)
            .join(".ao")
            .join("runs")
            .join(run_id),
        Path::new(project_root)
            .join(".ao")
            .join("state")
            .join("runs")
            .join(run_id),
    ]
}

fn extract_timestamp_hint(line: &str) -> Option<String> {
    let parsed = serde_json::from_str::<Value>(line).ok()?;
    parsed
        .get("timestamp")
        .and_then(|value| value.as_str())
        .or_else(|| parsed.get("created_at").and_then(|value| value.as_str()))
        .or_else(|| parsed.get("time").and_then(|value| value.as_str()))
        .map(|value| value.to_string())
}

fn get_run_jsonl_entries(project_root: &str, run_id: &str) -> Result<Vec<RunJsonlEntryCli>> {
    if run_id.trim().is_empty() {
        anyhow::bail!("run_id is required");
    }
    if run_id.contains('/') || run_id.contains('\\') || run_id.contains("..") {
        anyhow::bail!("invalid run_id");
    }

    let mut rows = Vec::new();
    for run_dir in run_dir_candidates(project_root, run_id) {
        if !run_dir.exists() {
            continue;
        }
        for file_name in [
            "json-output.jsonl",
            "stdout.jsonl",
            "stderr.jsonl",
            "system.jsonl",
            "signals.jsonl",
            "events.jsonl",
        ] {
            let path = run_dir.join(file_name);
            if !path.exists() {
                continue;
            }
            let content = fs::read_to_string(&path)?;
            for line in content.lines() {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                rows.push(RunJsonlEntryCli {
                    source_file: file_name.to_string(),
                    line: line.to_string(),
                    timestamp_hint: extract_timestamp_hint(line),
                });
            }
        }
    }

    rows.sort_by(|a, b| a.timestamp_hint.cmp(&b.timestamp_hint));
    Ok(rows)
}

fn infer_cli_from_jsonl(entries: &[RunJsonlEntryCli]) -> Option<String> {
    for entry in entries {
        let lower = entry.line.to_ascii_lowercase();
        if lower.contains("claude") {
            return Some("claude".to_string());
        }
        if lower.contains("codex") || lower.contains("openai") {
            return Some("codex".to_string());
        }
        if lower.contains("gemini") {
            return Some("gemini".to_string());
        }
        if lower.contains("opencode") {
            return Some("opencode".to_string());
        }
    }
    None
}

fn artifact_dir(project_root: &str, execution_id: &str) -> PathBuf {
    Path::new(project_root)
        .join(".ao")
        .join("artifacts")
        .join(execution_id)
}

fn list_artifact_infos(project_root: &str, execution_id: &str) -> Result<Vec<ArtifactInfoCli>> {
    let artifacts_dir = artifact_dir(project_root, execution_id);
    if !artifacts_dir.exists() {
        return Ok(Vec::new());
    }
    let mut artifacts = Vec::new();
    for entry in fs::read_dir(&artifacts_dir)? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let file_name = path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("artifact")
            .to_string();
        let artifact_type = path
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or("file")
            .to_string();
        let size_bytes = fs::metadata(&path).ok().map(|metadata| metadata.len());
        artifacts.push(ArtifactInfoCli {
            artifact_id: file_name.clone(),
            artifact_type,
            file_path: Some(path.display().to_string()),
            size_bytes,
        });
    }
    Ok(artifacts)
}

pub(crate) async fn handle_output(
    command: OutputCommand,
    project_root: &str,
    json: bool,
) -> Result<()> {
    match command {
        OutputCommand::Run(args) => {
            let run_dir = run_dir_candidates(project_root, &args.run_id)
                .into_iter()
                .find(|path| path.exists())
                .ok_or_else(|| anyhow!("run directory not found for {}", args.run_id))?;
            let events_path = run_dir.join("events.jsonl");
            if !events_path.exists() {
                return print_value(Vec::<Value>::new(), json);
            }
            let content = fs::read_to_string(events_path)?;
            let events: Vec<Value> = content
                .lines()
                .filter_map(|line| serde_json::from_str::<Value>(line).ok())
                .collect();
            print_value(events, json)
        }
        OutputCommand::Artifacts(args) => {
            print_value(list_artifact_infos(project_root, &args.execution_id)?, json)
        }
        OutputCommand::Download(args) => {
            let path = artifact_dir(project_root, &args.execution_id).join(&args.artifact_id);
            let bytes = fs::read(&path)
                .with_context(|| format!("failed to read artifact at {}", path.display()))?;
            print_value(
                serde_json::json!({
                    "artifact_id": args.artifact_id,
                    "execution_id": args.execution_id,
                    "size_bytes": bytes.len(),
                    "bytes": bytes,
                }),
                json,
            )
        }
        OutputCommand::Files(args) => {
            let files: Vec<String> = list_artifact_infos(project_root, &args.execution_id)?
                .into_iter()
                .map(|artifact| artifact.artifact_id)
                .collect();
            print_value(files, json)
        }
        OutputCommand::Jsonl(args) => {
            let entries = get_run_jsonl_entries(project_root, &args.run_id)?;
            if args.entries {
                print_value(entries, json)
            } else {
                let lines: Vec<String> = entries.into_iter().map(|entry| entry.line).collect();
                print_value(lines, json)
            }
        }
        OutputCommand::Monitor(args) => {
            let entries = get_run_jsonl_entries(project_root, &args.run_id)?;
            let mut events = Vec::new();
            for entry in entries {
                let Ok(payload) = serde_json::from_str::<Value>(&entry.line) else {
                    continue;
                };
                if let Some(task_id) = args.task_id.as_deref() {
                    if payload.get("task_id").and_then(|value| value.as_str()) != Some(task_id) {
                        continue;
                    }
                }
                if let Some(phase_id) = args.phase_id.as_deref() {
                    if payload.get("phase_id").and_then(|value| value.as_str()) != Some(phase_id) {
                        continue;
                    }
                }
                events.push(payload);
            }
            print_value(events, json)
        }
        OutputCommand::Cli(args) => {
            let entries = get_run_jsonl_entries(project_root, &args.run_id)?;
            print_value(
                serde_json::json!({
                    "run_id": args.run_id,
                    "cli": infer_cli_from_jsonl(&entries),
                }),
                json,
            )
        }
    }
}
