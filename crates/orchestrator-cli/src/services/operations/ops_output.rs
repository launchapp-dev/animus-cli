use crate::cli_types::OutputCommand;
use crate::{ensure_safe_run_id, not_found_error, print_value, run_dir};
use anyhow::{Context, Result};
use protocol::RunId;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;
use workflow_runner_v2::phase_output::{phase_output_dir, PersistedPhaseOutput};

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
pub(crate) struct RunJsonlEntryCli {
    pub(crate) source_file: String,
    pub(crate) line: String,
    #[serde(default)]
    pub(crate) timestamp_hint: Option<String>,
}

fn run_dir_candidates(project_root: &str, run_id: &str) -> Vec<PathBuf> {
    vec![
        run_dir(project_root, &RunId(run_id.to_string()), None),
        Path::new(project_root).join(".ao").join("runs").join(run_id),
        Path::new(project_root).join(".ao").join("state").join("runs").join(run_id),
    ]
}

pub(crate) fn resolve_run_dir_for_lookup(project_root: &str, run_id: &str) -> Result<Option<PathBuf>> {
    ensure_safe_run_id(run_id)?;
    Ok(run_dir_candidates(project_root, run_id).into_iter().find(|path| path.exists()))
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

pub(crate) fn get_run_jsonl_entries(project_root: &str, run_id: &str) -> Result<Vec<RunJsonlEntryCli>> {
    let mut rows = Vec::new();
    let Some(run_dir) = resolve_run_dir_for_lookup(project_root, run_id)? else {
        return Ok(rows);
    };
    for file_name in
        ["json-output.jsonl", "stdout.jsonl", "stderr.jsonl", "system.jsonl", "signals.jsonl", "events.jsonl"]
    {
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
    Path::new(project_root).join(".ao").join("artifacts").join(execution_id)
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
        let file_name = path.file_name().and_then(|value| value.to_str()).unwrap_or("artifact").to_string();
        let artifact_type = path.extension().and_then(|value| value.to_str()).unwrap_or("file").to_string();
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

fn ensure_safe_workflow_id(workflow_id: &str) -> Result<()> {
    if workflow_id.is_empty() || workflow_id.contains('/') || workflow_id.contains('\\') || workflow_id.contains("..") {
        anyhow::bail!("workflow id contains unsafe path segments");
    }
    Ok(())
}

fn run_ids_for_workflow(project_root: &str, workflow_id: &str) -> Result<BTreeSet<String>> {
    let mut run_ids = BTreeSet::new();
    let prefix = format!("wf-{workflow_id}-");
    for runs_root in runs_root_candidates(project_root) {
        if !runs_root.exists() {
            continue;
        }
        for entry in
            fs::read_dir(&runs_root).with_context(|| format!("failed to read run directory {}", runs_root.display()))?
        {
            let entry = entry?;
            if !entry.path().is_dir() {
                continue;
            }
            let Some(name) = entry.file_name().to_str().map(ToOwned::to_owned) else {
                continue;
            };
            if !name.starts_with(prefix.as_str()) {
                continue;
            }
            if ensure_safe_run_id(name.as_str()).is_err() {
                continue;
            }
            run_ids.insert(name);
        }
    }
    Ok(run_ids)
}

fn runs_root_candidates(project_root: &str) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(scoped_parent) = run_dir(project_root, &RunId("workflow-monitor-root-probe".to_string()), None).parent() {
        candidates.push(scoped_parent.to_path_buf());
    }
    candidates.push(Path::new(project_root).join(".ao").join("runs"));
    candidates.push(Path::new(project_root).join(".ao").join("state").join("runs"));

    let mut deduped = Vec::new();
    for candidate in candidates {
        if deduped.iter().all(|existing| existing != &candidate) {
            deduped.push(candidate);
        }
    }
    deduped
}

fn path_modified_millis(path: &Path) -> u128 {
    fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_millis())
        .unwrap_or(0)
}

fn resolve_latest_run_for_workflow(project_root: &str, workflow_id: &str) -> Result<Option<String>> {
    let run_ids = run_ids_for_workflow(project_root, workflow_id)?;
    let mut candidates = Vec::new();
    for run_id in run_ids {
        let Some(run_dir) = resolve_run_dir_for_lookup(project_root, run_id.as_str())? else {
            continue;
        };
        let events_path = run_dir.join("events.jsonl");
        let has_events = events_path.exists();
        let modified_millis = if has_events {
            path_modified_millis(events_path.as_path())
        } else {
            path_modified_millis(run_dir.as_path())
        };
        candidates.push((run_id, has_events, modified_millis));
    }

    candidates.sort_by(|left, right| {
        right.1.cmp(&left.1).then_with(|| right.2.cmp(&left.2)).then_with(|| left.0.cmp(&right.0))
    });

    Ok(candidates.into_iter().next().map(|(run_id, _, _)| run_id))
}

pub(crate) fn get_phase_outputs(
    project_root: &str,
    workflow_id: &str,
    phase_id: Option<&str>,
) -> Result<Vec<PersistedPhaseOutput>> {
    ensure_safe_workflow_id(workflow_id)?;
    if let Some(phase_id) = phase_id {
        ensure_safe_workflow_id(phase_id)?;
    }

    let dir = phase_output_dir(project_root, workflow_id);
    if !dir.exists() {
        return Ok(Vec::new());
    }

    let mut outputs = Vec::new();
    if let Some(phase_id) = phase_id {
        let file_path = dir.join(format!("{phase_id}.json"));
        if !file_path.exists() {
            return Ok(outputs);
        }
        let content = fs::read_to_string(&file_path)?;
        outputs.push(serde_json::from_str::<PersistedPhaseOutput>(&content)?);
        return Ok(outputs);
    }

    for entry in fs::read_dir(&dir)? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_file() || path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let content = fs::read_to_string(&path)?;
        outputs.push(serde_json::from_str::<PersistedPhaseOutput>(&content)?);
    }
    outputs.sort_by(|left, right| {
        left.completed_at.cmp(&right.completed_at).then_with(|| left.phase_id.cmp(&right.phase_id))
    });
    Ok(outputs)
}

pub(crate) async fn handle_output(command: OutputCommand, project_root: &str, json: bool) -> Result<()> {
    match command {
        OutputCommand::Run(args) => {
            let run_dir = resolve_run_dir_for_lookup(project_root, &args.run_id)?
                .ok_or_else(|| not_found_error(format!("run directory not found for {}", args.run_id)))?;
            let events_path = run_dir.join("events.jsonl");
            if !events_path.exists() {
                return print_value(Vec::<Value>::new(), json);
            }
            let content = fs::read_to_string(events_path)?;
            let events: Vec<Value> =
                content.lines().filter_map(|line| serde_json::from_str::<Value>(line).ok()).collect();
            print_value(events, json)
        }
        OutputCommand::PhaseOutputs(args) => print_value(
            serde_json::json!({
                "workflow_id": args.workflow_id,
                "phase_id": args.phase_id,
                "outputs": get_phase_outputs(project_root, &args.workflow_id, args.phase_id.as_deref())?,
            }),
            json,
        ),
        OutputCommand::Artifacts(args) => print_value(list_artifact_infos(project_root, &args.execution_id)?, json),
        OutputCommand::Download(args) => {
            let path = artifact_dir(project_root, &args.execution_id).join(&args.artifact_id);
            let bytes = fs::read(&path).with_context(|| format!("failed to read artifact at {}", path.display()))?;
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
            let resolved_run_id = if let Some(workflow_id) = args.workflow_id.as_deref() {
                ensure_safe_workflow_id(workflow_id)?;
                resolve_latest_run_for_workflow(project_root, workflow_id)?
                    .ok_or_else(|| not_found_error(format!("no runs found for workflow {}", workflow_id)))?
            } else if let Some(run_id) = args.run_id.as_deref() {
                run_id.to_string()
            } else {
                return Err(anyhow::anyhow!("provide either --run-id or --workflow-id"));
            };

            let entries = get_run_jsonl_entries(project_root, &resolved_run_id)?;
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

            let mut result = serde_json::json!({
                "events": events,
            });

            if let Some(workflow_id) = args.workflow_id.as_deref() {
                let phase_outputs = get_phase_outputs(project_root, workflow_id, None)?;
                if let Some(last_output) = phase_outputs.last() {
                    let summary = serde_json::json!({
                        "phase_id": last_output.phase_id,
                        "verdict": last_output.verdict,
                        "reason": last_output.reason,
                        "completed_at": last_output.completed_at,
                    });
                    result["phase_summary"] = summary;
                }
            }

            print_value(result, json)
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

#[cfg(test)]
mod tests {
    use super::*;

    use protocol::test_utils::EnvVarGuard;

    #[test]
    fn run_dir_candidates_prioritize_scoped_canonical_path() {
        let _lock = crate::shared::test_env_lock().lock().expect("env lock should be available");
        let temp = tempfile::tempdir().expect("tempdir should be created");
        let _home = EnvVarGuard::set("HOME", Some(temp.path().to_string_lossy().as_ref()));
        let project_root = temp.path().join("project");
        std::fs::create_dir_all(&project_root).expect("project dir should be created");
        let run_id = "trace-output-run";

        let candidates = run_dir_candidates(project_root.to_string_lossy().as_ref(), run_id);
        assert_eq!(candidates.len(), 3);
        assert_eq!(candidates[0], run_dir(project_root.to_string_lossy().as_ref(), &RunId(run_id.to_string()), None));
        assert_eq!(candidates[1], project_root.join(".ao").join("runs").join(run_id));
        assert_eq!(candidates[2], project_root.join(".ao").join("state").join("runs").join(run_id));

        for candidate in &candidates {
            std::fs::create_dir_all(candidate).expect("candidate run dir should be created");
        }
        let selected = resolve_run_dir_for_lookup(project_root.to_string_lossy().as_ref(), run_id)
            .expect("run dir lookup should succeed")
            .expect("a run dir should be selected");
        assert_eq!(selected, candidates[0]);
    }

    #[test]
    fn run_dir_candidates_fall_back_to_legacy_paths() {
        let _lock = crate::shared::test_env_lock().lock().expect("env lock should be available");
        let temp = tempfile::tempdir().expect("tempdir should be created");
        let _home = EnvVarGuard::set("HOME", Some(temp.path().to_string_lossy().as_ref()));
        let project_root = temp.path().join("project");
        std::fs::create_dir_all(&project_root).expect("project dir should be created");
        let run_id = "trace-output-legacy";
        let candidates = run_dir_candidates(project_root.to_string_lossy().as_ref(), run_id);

        std::fs::create_dir_all(&candidates[1]).expect("legacy .ao/runs dir should be created");
        let selected_legacy = resolve_run_dir_for_lookup(project_root.to_string_lossy().as_ref(), run_id)
            .expect("run dir lookup should succeed")
            .expect("legacy run dir should be selected");
        assert_eq!(selected_legacy, candidates[1]);

        std::fs::remove_dir_all(&candidates[1]).expect("legacy .ao/runs dir should be removed");
        std::fs::create_dir_all(&candidates[2]).expect("legacy .ao/state/runs dir should exist");
        let selected_state = resolve_run_dir_for_lookup(project_root.to_string_lossy().as_ref(), run_id)
            .expect("run dir lookup should succeed")
            .expect("legacy state run dir should be selected");
        assert_eq!(selected_state, candidates[2]);
    }

    #[test]
    fn get_run_jsonl_entries_prefer_canonical_path_over_legacy_fallbacks() {
        let _lock = crate::shared::test_env_lock().lock().expect("env lock should be available");
        let temp = tempfile::tempdir().expect("tempdir should be created");
        let _home = EnvVarGuard::set("HOME", Some(temp.path().to_string_lossy().as_ref()));
        let project_root = temp.path().join("project");
        std::fs::create_dir_all(&project_root).expect("project dir should be created");
        let run_id = "trace-jsonl-canonical-precedence";
        let canonical_dir = run_dir(project_root.to_string_lossy().as_ref(), &RunId(run_id.to_string()), None);
        let legacy_dir = project_root.join(".ao").join("runs").join(run_id);
        let legacy_state_dir = project_root.join(".ao").join("state").join("runs").join(run_id);
        std::fs::create_dir_all(&canonical_dir).expect("canonical run dir should be created");
        std::fs::create_dir_all(&legacy_dir).expect("legacy run dir should be created");
        std::fs::create_dir_all(&legacy_state_dir).expect("legacy state run dir should be created");
        std::fs::write(
            canonical_dir.join("events.jsonl"),
            "{\"timestamp\":\"2024-01-01T00:00:00Z\",\"kind\":\"canonical\"}\n",
        )
        .expect("canonical events should be written");
        std::fs::write(
            legacy_dir.join("events.jsonl"),
            "{\"timestamp\":\"2024-01-02T00:00:00Z\",\"kind\":\"legacy\"}\n",
        )
        .expect("legacy events should be written");
        std::fs::write(
            legacy_state_dir.join("events.jsonl"),
            "{\"timestamp\":\"2024-01-03T00:00:00Z\",\"kind\":\"legacy-state\"}\n",
        )
        .expect("legacy state events should be written");

        let entries = get_run_jsonl_entries(project_root.to_string_lossy().as_ref(), run_id)
            .expect("jsonl entries should load from canonical path");
        assert_eq!(entries.len(), 1);
        assert!(entries[0].line.contains("\"canonical\""));
    }

    #[test]
    fn get_run_jsonl_entries_keep_lookup_repo_scoped_under_global_runner_scope() {
        let _lock = crate::shared::test_env_lock().lock().expect("env lock should be available");
        let temp = tempfile::tempdir().expect("tempdir should be created");
        let _home = EnvVarGuard::set("HOME", Some(temp.path().to_string_lossy().as_ref()));
        let _scope = EnvVarGuard::set("AO_RUNNER_SCOPE", Some("global"));
        let override_dir = temp.path().join("override-config");
        let _ao_config = EnvVarGuard::set("AO_CONFIG_DIR", Some(override_dir.to_string_lossy().as_ref()));
        let project_root = temp.path().join("project");
        std::fs::create_dir_all(&project_root).expect("project dir should be created");
        let run_id = "trace-jsonl-global-scope-lookup";
        let canonical_dir = run_dir(project_root.to_string_lossy().as_ref(), &RunId(run_id.to_string()), None);
        let override_run_dir = override_dir.join("runs").join(run_id);
        std::fs::create_dir_all(&canonical_dir).expect("canonical run dir should be created");
        std::fs::create_dir_all(&override_run_dir).expect("override run dir should be created");
        std::fs::write(
            canonical_dir.join("events.jsonl"),
            "{\"timestamp\":\"2024-01-01T00:00:00Z\",\"kind\":\"canonical\"}\n",
        )
        .expect("canonical events should be written");
        std::fs::write(
            override_run_dir.join("events.jsonl"),
            "{\"timestamp\":\"2024-01-02T00:00:00Z\",\"kind\":\"override\"}\n",
        )
        .expect("override events should be written");

        let entries = get_run_jsonl_entries(project_root.to_string_lossy().as_ref(), run_id)
            .expect("jsonl entries should load from scoped path");
        assert_eq!(entries.len(), 1);
        assert!(entries[0].line.contains("\"canonical\""));
        assert!(canonical_dir.starts_with(temp.path().join(".ao")));
        assert!(!canonical_dir.starts_with(&override_dir));
    }

    #[test]
    fn get_run_jsonl_entries_merges_deterministically_with_source_metadata() {
        let _lock = crate::shared::test_env_lock().lock().expect("env lock should be available");
        let temp = tempfile::tempdir().expect("tempdir should be created");
        let _home = EnvVarGuard::set("HOME", Some(temp.path().to_string_lossy().as_ref()));
        let project_root = temp.path().join("project");
        std::fs::create_dir_all(&project_root).expect("project dir should be created");
        let run_id = "trace-jsonl-order";
        let run_dir = run_dir(project_root.to_string_lossy().as_ref(), &RunId(run_id.to_string()), None);
        std::fs::create_dir_all(&run_dir).expect("canonical run dir should be created");
        std::fs::write(
            run_dir.join("json-output.jsonl"),
            "{\"created_at\":\"2024-01-01T00:00:00Z\",\"kind\":\"json\"}\n",
        )
        .expect("json output should be written");
        std::fs::write(run_dir.join("events.jsonl"), "{\"timestamp\":\"2024-01-02T00:00:00Z\",\"kind\":\"event\"}\n")
            .expect("events output should be written");

        let entries =
            get_run_jsonl_entries(project_root.to_string_lossy().as_ref(), run_id).expect("jsonl entries should load");
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].source_file, "json-output.jsonl");
        assert_eq!(entries[0].timestamp_hint.as_deref(), Some("2024-01-01T00:00:00Z"));
        assert_eq!(entries[1].source_file, "events.jsonl");
        assert_eq!(entries[1].timestamp_hint.as_deref(), Some("2024-01-02T00:00:00Z"));
    }

    #[test]
    fn get_run_jsonl_entries_reads_events_persisted_via_runner_helpers() {
        let _lock = crate::shared::test_env_lock().lock().expect("env lock should be available");
        let temp = tempfile::tempdir().expect("tempdir should be created");
        let _home = EnvVarGuard::set("HOME", Some(temp.path().to_string_lossy().as_ref()));
        let project_root = temp.path().join("project");
        std::fs::create_dir_all(&project_root).expect("project dir should be created");

        let run_id = RunId("trace-jsonl-persist".to_string());
        let canonical_run_dir = run_dir(project_root.to_string_lossy().as_ref(), &run_id, None);

        crate::persist_agent_event(
            &canonical_run_dir,
            &protocol::AgentRunEvent::Started { run_id: run_id.clone(), timestamp: protocol::Timestamp::now() },
        )
        .expect("started event should persist");
        crate::persist_agent_event(
            &canonical_run_dir,
            &protocol::AgentRunEvent::Finished { run_id: run_id.clone(), exit_code: Some(0), duration_ms: 12 },
        )
        .expect("finished event should persist");

        let entries = get_run_jsonl_entries(project_root.to_string_lossy().as_ref(), &run_id.0)
            .expect("jsonl entries should include persisted events");
        assert_eq!(entries.len(), 2);
        assert!(entries.iter().all(|entry| entry.source_file == "events.jsonl"));
        for entry in entries {
            let parsed = serde_json::from_str::<protocol::AgentRunEvent>(&entry.line)
                .expect("persisted event lines should parse");
            assert!(crate::event_matches_run(&parsed, &run_id));
        }
    }

    #[test]
    fn get_run_jsonl_entries_supports_legacy_lookup_paths() {
        let _lock = crate::shared::test_env_lock().lock().expect("env lock should be available");
        let temp = tempfile::tempdir().expect("tempdir should be created");
        let _home = EnvVarGuard::set("HOME", Some(temp.path().to_string_lossy().as_ref()));
        let project_root = temp.path().join("project");
        std::fs::create_dir_all(&project_root).expect("project dir should be created");
        let run_id = "trace-jsonl-legacy";
        let legacy_run_dir = project_root.join(".ao").join("runs").join(run_id);
        std::fs::create_dir_all(&legacy_run_dir).expect("legacy run dir should be created");
        std::fs::write(
            legacy_run_dir.join("events.jsonl"),
            "{\"timestamp\":\"2024-01-03T00:00:00Z\",\"kind\":\"legacy\"}\n",
        )
        .expect("legacy events should be written");
        let legacy_state_run_dir = project_root.join(".ao").join("state").join("runs").join(run_id);
        std::fs::create_dir_all(&legacy_state_run_dir).expect("legacy state run dir should exist");
        std::fs::write(
            legacy_state_run_dir.join("events.jsonl"),
            "{\"timestamp\":\"2024-01-04T00:00:00Z\",\"kind\":\"legacy-state\"}\n",
        )
        .expect("legacy state events should be written");

        let legacy_entries = get_run_jsonl_entries(project_root.to_string_lossy().as_ref(), run_id)
            .expect("jsonl entries should load from legacy path");
        assert_eq!(legacy_entries.len(), 1);
        assert!(legacy_entries[0].line.contains("\"legacy\""));
        assert_eq!(legacy_entries[0].timestamp_hint.as_deref(), Some("2024-01-03T00:00:00Z"));

        std::fs::remove_dir_all(&legacy_run_dir).expect("legacy run dir should be removed");
        let state_entries = get_run_jsonl_entries(project_root.to_string_lossy().as_ref(), run_id)
            .expect("jsonl entries should load from legacy state path");
        assert_eq!(state_entries.len(), 1);
        assert!(state_entries[0].line.contains("\"legacy-state\""));
        assert_eq!(state_entries[0].timestamp_hint.as_deref(), Some("2024-01-04T00:00:00Z"));
    }

    #[test]
    fn get_run_jsonl_entries_rejects_unsafe_run_ids() {
        let err = get_run_jsonl_entries("/tmp/project", "../escape").expect_err("unsafe run id should be rejected");
        assert!(err.to_string().contains("invalid run_id"));
    }

    #[test]
    fn get_phase_outputs_reads_persisted_payloads() {
        let _lock = crate::shared::test_env_lock().lock().expect("env lock should be available");
        let temp = tempfile::tempdir().expect("tempdir should be created");
        let _home = EnvVarGuard::set("HOME", Some(temp.path().to_string_lossy().as_ref()));
        let project_root = temp.path().join("project");
        std::fs::create_dir_all(&project_root).expect("project dir should be created");
        let workflow_id = "wf-phase-output-test";
        let output_dir = phase_output_dir(project_root.to_string_lossy().as_ref(), workflow_id);
        std::fs::create_dir_all(&output_dir).expect("phase output dir should exist");

        let implementation = PersistedPhaseOutput {
            phase_id: "implementation".to_string(),
            completed_at: "2026-03-10T00:00:00Z".to_string(),
            verdict: Some("advance".to_string()),
            confidence: Some(0.9),
            reason: Some("Implemented".to_string()),
            commit_message: Some("feat: implement contract".to_string()),
            evidence: Vec::new(),
            guardrail_violations: Vec::new(),
            payload: Some(serde_json::json!({
                "kind": "implementation_result",
                "verdict": "advance",
                "changed_files": ["src/lib.rs"]
            })),
        };
        let unit_test = PersistedPhaseOutput {
            phase_id: "unit-test".to_string(),
            completed_at: "2026-03-10T00:05:00Z".to_string(),
            verdict: Some("rework".to_string()),
            confidence: Some(1.0),
            reason: Some("Tests failed".to_string()),
            commit_message: None,
            evidence: Vec::new(),
            guardrail_violations: Vec::new(),
            payload: Some(serde_json::json!({
                "kind": "phase_result",
                "verdict": "rework",
                "failure_category": "tests_failed"
            })),
        };
        std::fs::write(
            output_dir.join("implementation.json"),
            serde_json::to_string_pretty(&implementation).expect("serialize output"),
        )
        .expect("implementation output should be written");
        std::fs::write(
            output_dir.join("unit-test.json"),
            serde_json::to_string_pretty(&unit_test).expect("serialize output"),
        )
        .expect("unit-test output should be written");

        let all_outputs = get_phase_outputs(project_root.to_string_lossy().as_ref(), workflow_id, None)
            .expect("phase outputs should load");
        assert_eq!(all_outputs.len(), 2);
        assert_eq!(all_outputs[0].phase_id, "implementation");
        assert_eq!(all_outputs[1].phase_id, "unit-test");

        let unit_test_only = get_phase_outputs(project_root.to_string_lossy().as_ref(), workflow_id, Some("unit-test"))
            .expect("single phase output should load");
        assert_eq!(unit_test_only.len(), 1);
        assert_eq!(unit_test_only[0].phase_id, "unit-test");
        assert_eq!(
            unit_test_only[0].payload.as_ref().and_then(|value| value.get("failure_category")).and_then(Value::as_str),
            Some("tests_failed")
        );
    }
}
