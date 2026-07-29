use crate::cli_types::OutputCommand;
use crate::{ensure_safe_run_id, not_found_error, print_value, run_dir};
use animus_runtime_shared::phase_output::{phase_output_dir, PersistedPhaseOutput, PersistedPhaseSkill};
use animus_runtime_shared::phase_session;
use animus_runtime_shared::recording::ReplaySource;
use anyhow::{Context, Result};
use protocol::RunId;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ArtifactInfoCli {
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

#[derive(Debug, Clone, Serialize)]
pub(crate) struct OutputPhaseOutputsResult {
    pub(crate) workflow_id: String,
    pub(crate) phase_id: Option<String>,
    pub(crate) outputs: Vec<PersistedPhaseOutput>,
}

fn run_dir_candidates(project_root: &str, run_id: &str) -> Vec<PathBuf> {
    vec![
        run_dir(project_root, &RunId(run_id.to_string()), None),
        Path::new(project_root).join(".animus").join("runs").join(run_id),
        Path::new(project_root).join(".animus").join("state").join("runs").join(run_id),
    ]
}

pub(crate) fn resolve_run_dir_for_lookup(project_root: &str, run_id: &str) -> Result<Option<PathBuf>> {
    ensure_safe_run_id(run_id)?;
    Ok(run_dir_candidates(project_root, run_id).into_iter().find(|path| path.exists()))
}

/// All `(run_id, started_at)` pairs recorded for a workflow via the
/// per-phase session checkpoints at `runs/<workflow_id>/phases/*.session.json`
/// (the canonical `(workflow_id, phase_id, run_id)` source — the same files
/// the cost scanner correlates on).
fn collect_workflow_run_candidates(project_root: &str, workflow_id: &str) -> Result<Vec<(String, String)>> {
    let mut candidates = Vec::new();
    let Some(workflow_run_dir) = resolve_run_dir_for_lookup(project_root, workflow_id)? else {
        return Ok(candidates);
    };
    let phases_dir = workflow_run_dir.join("phases");
    if !phases_dir.is_dir() {
        return Ok(candidates);
    }
    for entry in fs::read_dir(&phases_dir).with_context(|| format!("read phases dir {}", phases_dir.display()))? {
        let path = entry?.path();
        if !path.file_name().and_then(|name| name.to_str()).is_some_and(|name| name.ends_with(".session.json")) {
            continue;
        }
        if let Ok(Some(checkpoint)) = phase_session::read_path(&path) {
            candidates.push((checkpoint.run_id, checkpoint.started_at));
        }
    }
    Ok(candidates)
}

/// Resolve the latest run id recorded for `workflow_id`. Errors when no run
/// is recorded or when the latest start time is shared by multiple distinct
/// run ids (ambiguous — the caller must pass --run-id explicitly).
pub(crate) fn resolve_latest_run_id_for_workflow(project_root: &str, workflow_id: &str) -> Result<String> {
    let candidates = collect_workflow_run_candidates(project_root, workflow_id)?;
    if candidates.is_empty() {
        // Legacy naming fallback: runs created before session checkpoints
        // used `wf-<workflow_id>-<phase>-...` (scanned by mtime) or the bare
        // `wf-<workflow_id>` run dir name directly.
        if let Some((run_id, _)) =
            super::ops_mcp::output_tail_resolution::resolve_latest_workflow_run_dir(project_root, workflow_id)?
        {
            return Ok(run_id);
        }
        let legacy_run_id = format!("wf-{workflow_id}");
        if resolve_run_dir_for_lookup(project_root, &legacy_run_id)?.is_some() {
            return Ok(legacy_run_id);
        }
        return Err(not_found_error(format!(
            "no runs recorded for workflow {workflow_id}; pass --run-id explicitly if you know the run id"
        )));
    }
    let parse_started =
        |value: &str| chrono::DateTime::parse_from_rfc3339(value).map(|ts| ts.with_timezone(&chrono::Utc)).ok();
    let latest_key = candidates
        .iter()
        .map(|(_, started_at)| (parse_started(started_at), started_at.clone()))
        .max()
        .expect("candidates is non-empty");
    let mut latest_run_ids: Vec<&str> = candidates
        .iter()
        .filter(|(_, started_at)| (parse_started(started_at), started_at.clone()) == latest_key)
        .map(|(run_id, _)| run_id.as_str())
        .collect();
    latest_run_ids.sort_unstable();
    latest_run_ids.dedup();
    match latest_run_ids.as_slice() {
        [run_id] => Ok((*run_id).to_string()),
        many => anyhow::bail!(
            "ambiguous latest run for workflow {workflow_id}: multiple runs share started_at ({}); pass --run-id explicitly",
            many.join(", ")
        ),
    }
}

/// Best-effort variant for enrichment paths (history hydration): swallows
/// not-found/ambiguity instead of failing the whole listing.
pub(crate) fn try_latest_run_id_for_workflow(project_root: &str, workflow_id: &str) -> Option<String> {
    resolve_latest_run_id_for_workflow(project_root, workflow_id).ok()
}

fn resolve_run_id_arg(project_root: &str, run_id: Option<String>, workflow_id: Option<String>) -> Result<String> {
    match (run_id, workflow_id) {
        (Some(run_id), _) => Ok(run_id),
        (None, Some(workflow_id)) => resolve_latest_run_id_for_workflow(project_root, &workflow_id),
        (None, None) => anyhow::bail!("either --run-id or --workflow-id is required"),
    }
}

fn resolve_workflow_id_for_run(project_root: &str, run_id: &str) -> Result<Option<String>> {
    ensure_safe_run_id(run_id)?;
    let project = Path::new(project_root);
    let state_root = protocol::scoped_state_root(project).unwrap_or_else(|| project.join(".animus"));
    let runs_dir = state_root.join("runs");
    if !runs_dir.is_dir() {
        return Ok(None);
    }
    for workflow_entry in fs::read_dir(&runs_dir).with_context(|| format!("read runs dir {}", runs_dir.display()))? {
        let workflow_entry = workflow_entry?;
        if !workflow_entry.path().is_dir() {
            continue;
        }
        let phases_dir = workflow_entry.path().join("phases");
        if !phases_dir.is_dir() {
            continue;
        }
        for phase_entry in
            fs::read_dir(&phases_dir).with_context(|| format!("read phases dir {}", phases_dir.display()))?
        {
            let path = phase_entry?.path();
            if !path.file_name().and_then(|name| name.to_str()).is_some_and(|name| name.ends_with(".session.json")) {
                continue;
            }
            if let Ok(Some(checkpoint)) = phase_session::read_path(&path) {
                if checkpoint.run_id == run_id {
                    return Ok(Some(checkpoint.workflow_id));
                }
            }
        }
    }
    // A legacy root run may use the workflow id directly. Do this only after
    // scanning canonical phase checkpoints: a normal phase run also has its
    // own `runs/<run_id>` directory and must not be mistaken for a workflow.
    if orchestrator_core::WorkflowStateManager::new(project_root).load_workflow_actor(run_id).is_some() {
        return Ok(Some(run_id.to_string()));
    }
    Ok(None)
}

fn ensure_output_actor_access(
    project_root: &str,
    workflow_id: Option<&str>,
    run_id: Option<&str>,
    actor: Option<&animus_actor::Actor>,
) -> Result<()> {
    let Some(actor) = actor else {
        return Ok(());
    };
    let workflow_id = match workflow_id {
        Some(workflow_id) => Some(workflow_id.to_string()),
        None => match run_id {
            Some(run_id) => resolve_workflow_id_for_run(project_root, run_id)?,
            None => None,
        },
    }
    .ok_or_else(|| not_found_error("run not found"))?;
    super::ops_workflow::ensure_workflow_actor_access(project_root, &workflow_id, Some(actor))
}

/// Resolve the run id for a SPECIFIC `(workflow_id, phase_id)` from the phase
/// session checkpoints (`runs/<workflow_id>/phases/*.session.json`). Lets the UI
/// load one phase's transcript instead of only the workflow's latest run.
fn resolve_run_id_for_workflow_phase(project_root: &str, workflow_id: &str, phase_id: &str) -> Result<String> {
    let Some(workflow_run_dir) = resolve_run_dir_for_lookup(project_root, workflow_id)? else {
        return Err(not_found_error(format!("no run directory recorded for workflow {workflow_id}")));
    };
    let phases_dir = workflow_run_dir.join("phases");
    if phases_dir.is_dir() {
        for entry in fs::read_dir(&phases_dir).with_context(|| format!("read phases dir {}", phases_dir.display()))? {
            let path = entry?.path();
            if !path.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.ends_with(".session.json")) {
                continue;
            }
            if let Ok(Some(checkpoint)) = phase_session::read_path(&path) {
                if checkpoint.phase_id == phase_id {
                    return Ok(checkpoint.run_id);
                }
            }
        }
    }
    Err(not_found_error(format!("no run recorded for phase '{phase_id}' of workflow {workflow_id}")))
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

fn ensure_safe_id_segment(label: &str, value: &str) -> Result<()> {
    if value.is_empty() || value.contains('/') || value.contains('\\') || value.contains("..") {
        anyhow::bail!("{label} contains unsafe path segments");
    }
    Ok(())
}

fn artifact_dir_candidates(project_root: &str, execution_id: &str) -> Vec<PathBuf> {
    let scoped_root =
        protocol::scoped_state_root(Path::new(project_root)).unwrap_or_else(|| Path::new(project_root).join(".animus"));
    vec![
        scoped_root.join("artifacts").join(execution_id),
        Path::new(project_root).join(".animus").join("artifacts").join(execution_id),
    ]
}

fn resolve_artifact_dir(project_root: &str, execution_id: &str) -> Option<PathBuf> {
    artifact_dir_candidates(project_root, execution_id).into_iter().find(|path| path.exists())
}

fn list_artifact_infos(project_root: &str, execution_id: &str) -> Result<Vec<ArtifactInfoCli>> {
    ensure_safe_id_segment("execution id", execution_id)?;
    let Some(artifacts_dir) = resolve_artifact_dir(project_root, execution_id) else {
        return Ok(Vec::new());
    };
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
    ensure_safe_id_segment("workflow id", workflow_id)
}

pub(crate) fn output_read_application(
    project_root: &str,
    run_id: Option<&str>,
    workflow_id: Option<&str>,
    phase: Option<&str>,
    actor: Option<&animus_actor::Actor>,
) -> Result<Vec<Value>> {
    ensure_output_actor_access(project_root, workflow_id, run_id, actor)?;
    let resolved_run_id = match (phase, workflow_id) {
        (Some(phase), Some(workflow_id)) => resolve_run_id_for_workflow_phase(project_root, workflow_id, phase)?,
        _ => resolve_run_id_arg(project_root, run_id.map(str::to_string), workflow_id.map(str::to_string))?,
    };
    let run_dir = resolve_run_dir_for_lookup(project_root, &resolved_run_id)?
        .ok_or_else(|| not_found_error(format!("run directory not found for {resolved_run_id}")))?;
    let events_path = run_dir.join("events.jsonl");
    if !events_path.exists() {
        return Ok(Vec::new());
    }
    let content = fs::read_to_string(events_path)?;
    Ok(content.lines().filter_map(|line| serde_json::from_str::<Value>(line).ok()).collect())
}

pub(crate) fn output_phase_outputs_application(
    project_root: &str,
    workflow_id: &str,
    phase_id: Option<&str>,
    actor: Option<&animus_actor::Actor>,
) -> Result<OutputPhaseOutputsResult> {
    ensure_output_actor_access(project_root, Some(workflow_id), None, actor)?;
    Ok(OutputPhaseOutputsResult {
        workflow_id: workflow_id.to_string(),
        phase_id: phase_id.map(str::to_string),
        outputs: get_phase_outputs(project_root, workflow_id, phase_id)?,
    })
}

pub(crate) fn output_artifacts_application(project_root: &str, execution_id: &str) -> Result<Vec<ArtifactInfoCli>> {
    list_artifact_infos(project_root, execution_id)
}

pub(crate) fn output_jsonl_application(project_root: &str, run_id: &str, include_entries: bool) -> Result<Value> {
    let entries = get_run_jsonl_entries(project_root, run_id)?;
    if include_entries {
        Ok(serde_json::to_value(entries)?)
    } else {
        Ok(serde_json::to_value(entries.into_iter().map(|entry| entry.line).collect::<Vec<_>>())?)
    }
}

pub(crate) fn output_monitor_application(
    project_root: &str,
    run_id: &str,
    task_id: Option<&str>,
    phase_id: Option<&str>,
) -> Result<Vec<Value>> {
    let entries = get_run_jsonl_entries(project_root, run_id)?;
    let mut events = Vec::new();
    for entry in entries {
        let Ok(payload) = serde_json::from_str::<Value>(&entry.line) else {
            continue;
        };
        if let Some(task_id) = task_id {
            if payload.get("task_id").and_then(Value::as_str) != Some(task_id) {
                continue;
            }
        }
        if let Some(phase_id) = phase_id {
            if payload.get("phase_id").and_then(Value::as_str) != Some(phase_id) {
                continue;
            }
        }
        events.push(payload);
    }
    Ok(events)
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
        OutputCommand::Read(args) => {
            let actor = crate::shared::parse_actor_json_flag(args.actor_json.as_deref())?;
            print_value(
                output_read_application(
                    project_root,
                    args.run_id.as_deref(),
                    args.workflow_id.as_deref(),
                    args.phase.as_deref(),
                    actor.as_ref(),
                )?,
                json,
            )
        }
        OutputCommand::PhaseOutputs(args) => {
            let actor = crate::shared::parse_actor_json_flag(args.actor_json.as_deref())?;
            let result = output_phase_outputs_application(
                project_root,
                &args.workflow_id,
                args.phase_id.as_deref(),
                actor.as_ref(),
            )?;
            if json {
                print_value(result, json)
            } else {
                println!(
                    "{}",
                    render_phase_outputs_human(&result.workflow_id, result.phase_id.as_deref(), &result.outputs)
                );
                Ok(())
            }
        }
        OutputCommand::Artifacts(args) => {
            print_value(output_artifacts_application(project_root, &args.execution_id)?, json)
        }
        OutputCommand::Download(args) => {
            ensure_safe_id_segment("execution id", &args.execution_id)?;
            ensure_safe_id_segment("artifact id", &args.artifact_id)?;
            let artifacts_dir = resolve_artifact_dir(project_root, &args.execution_id)
                .unwrap_or_else(|| artifact_dir_candidates(project_root, &args.execution_id).remove(0));
            let path = artifacts_dir.join(&args.artifact_id);
            if !path.starts_with(&artifacts_dir) {
                anyhow::bail!("artifact path escapes the artifact directory");
            }
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
            print_value(output_jsonl_application(project_root, &args.run_id, args.entries)?, json)
        }
        OutputCommand::Monitor(args) => print_value(
            output_monitor_application(project_root, &args.run_id, args.task_id.as_deref(), args.phase_id.as_deref())?,
            json,
        ),
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
        OutputCommand::Decisions(args) => {
            let run_id = resolve_run_id_arg(project_root, args.run_id, args.workflow_id)?;
            print_value(decision_log_view(project_root, &run_id)?, json)
        }
    }
}

/// Human view for `animus output phase-outputs`: one block per persisted
/// phase output with verdict/reason summary plus a Skills section that
/// makes skill application verifiable — which skills were requested,
/// which applied (with source scope and contribution kinds), and which
/// never resolved (typo'd or not installed). Raw payloads stay on the
/// `--json` surface.
pub(crate) fn render_phase_outputs_human(
    workflow_id: &str,
    phase_filter: Option<&str>,
    outputs: &[PersistedPhaseOutput],
) -> String {
    let mut lines: Vec<String> = Vec::new();
    if outputs.is_empty() {
        match phase_filter {
            Some(phase_id) => {
                lines.push(format!("No persisted output for phase '{phase_id}' of workflow {workflow_id}."))
            }
            None => lines.push(format!("No persisted phase outputs for workflow {workflow_id}.")),
        }
        lines.push("Re-run with --json for the raw envelope.".to_string());
        return lines.join("\n");
    }

    lines.push(format!("Workflow: {workflow_id}"));
    for output in outputs {
        lines.push(String::new());
        lines.push(format!("Phase: {}", output.phase_id));
        lines.push(format!("  completed: {}", output.completed_at));
        if let Some(verdict) = output.verdict.as_deref() {
            match output.confidence {
                Some(confidence) => lines.push(format!("  verdict:   {verdict} (confidence {confidence:.2})")),
                None => lines.push(format!("  verdict:   {verdict}")),
            }
        }
        if let Some(reason) = output.reason.as_deref().map(str::trim).filter(|reason| !reason.is_empty()) {
            lines.push(format!("  reason:    {reason}"));
        }
        if let Some(commit) = output.commit_message.as_deref().map(str::trim).filter(|commit| !commit.is_empty()) {
            lines.push(format!("  commit:    {commit}"));
        }
        render_phase_skills_block(output, &mut lines);
    }
    lines.push(String::new());
    lines.push("Use --json for full payloads and the raw skill application record.".to_string());
    lines.join("\n")
}

fn format_skill_line(skill: &PersistedPhaseSkill) -> String {
    let mut line = format!("{} ({})", skill.name, skill.source);
    if !skill.contributions.is_empty() {
        line.push_str(&format!("  [{}]", skill.contributions.join(", ")));
    }
    line
}

fn render_phase_skills_block(output: &PersistedPhaseOutput, lines: &mut Vec<String>) {
    if output.requested_skills.is_empty() && output.resolved_skills.is_empty() && output.applied_skills.is_empty() {
        return;
    }
    lines.push("  Skills:".to_string());
    if !output.requested_skills.is_empty() {
        lines.push(format!("    requested: {}", output.requested_skills.join(", ")));
    }
    for skill in &output.applied_skills {
        lines.push(format!("    applied:   {}", format_skill_line(skill)));
    }
    // Resolved but not applied: the skill exists, but its activation did
    // not match the selected tool/model for this phase run.
    for skill in &output.resolved_skills {
        if output.applied_skills.iter().any(|applied| applied.name == skill.name) {
            continue;
        }
        lines.push(format!(
            "    resolved:  {}  (not applied — activation did not match the selected tool/model)",
            format_skill_line(skill)
        ));
    }
    // Requested but never resolved: typo'd name or skill not installed.
    for name in &output.requested_skills {
        let known = output.resolved_skills.iter().any(|skill| &skill.name == name)
            || output.applied_skills.iter().any(|skill| &skill.name == name);
        if !known {
            lines.push(format!("    missing:   {name}  (not found — check `animus skill list`)"));
        }
    }
}

/// Read `runs/<run_id>/decisions.jsonl` via the recording module's
/// [`ReplaySource`] reader and shape it for CLI output.
pub(crate) fn decision_log_view(project_root: &str, run_id: &str) -> Result<Value> {
    let run_dir = resolve_run_dir_for_lookup(project_root, run_id)?
        .ok_or_else(|| not_found_error(format!("run directory not found for {run_id}")))?;
    let decisions_path = run_dir.join("decisions.jsonl");
    if !decisions_path.exists() {
        return Err(not_found_error(format!(
            "no decision log for run {run_id} (expected {}); decision recording starts with the first agent event of a run",
            decisions_path.display()
        )));
    }
    let source = ReplaySource::open(&decisions_path)?;
    let truncated_tail = source.truncated_tail();
    let provider_id = source.provider_id().map(ToOwned::to_owned);
    let events = source.drain();
    Ok(serde_json::json!({
        "run_id": run_id,
        "path": decisions_path.display().to_string(),
        "provider_id": provider_id,
        "truncated_tail": truncated_tail,
        "event_count": events.len(),
        "events": events,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    use protocol::test_utils::EnvVarGuard;

    #[test]
    fn run_dir_candidates_prioritize_scoped_canonical_path() {
        let _lock = crate::shared::test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let temp = tempfile::tempdir().expect("tempdir should be created");
        let _home = EnvVarGuard::set("HOME", Some(temp.path().to_string_lossy().as_ref()));
        let project_root = temp.path().join("project");
        std::fs::create_dir_all(&project_root).expect("project dir should be created");
        let run_id = "trace-output-run";

        let candidates = run_dir_candidates(project_root.to_string_lossy().as_ref(), run_id);
        assert_eq!(candidates.len(), 3);
        assert_eq!(candidates[0], run_dir(project_root.to_string_lossy().as_ref(), &RunId(run_id.to_string()), None));
        assert_eq!(candidates[1], project_root.join(".animus").join("runs").join(run_id));
        assert_eq!(candidates[2], project_root.join(".animus").join("state").join("runs").join(run_id));

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
        let _lock = crate::shared::test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let temp = tempfile::tempdir().expect("tempdir should be created");
        let _home = EnvVarGuard::set("HOME", Some(temp.path().to_string_lossy().as_ref()));
        let project_root = temp.path().join("project");
        std::fs::create_dir_all(&project_root).expect("project dir should be created");
        let run_id = "trace-output-legacy";
        let candidates = run_dir_candidates(project_root.to_string_lossy().as_ref(), run_id);

        std::fs::create_dir_all(&candidates[1]).expect("legacy .animus/runs dir should be created");
        let selected_legacy = resolve_run_dir_for_lookup(project_root.to_string_lossy().as_ref(), run_id)
            .expect("run dir lookup should succeed")
            .expect("legacy run dir should be selected");
        assert_eq!(selected_legacy, candidates[1]);

        std::fs::remove_dir_all(&candidates[1]).expect("legacy .animus/runs dir should be removed");
        std::fs::create_dir_all(&candidates[2]).expect("legacy .animus/state/runs dir should exist");
        let selected_state = resolve_run_dir_for_lookup(project_root.to_string_lossy().as_ref(), run_id)
            .expect("run dir lookup should succeed")
            .expect("legacy state run dir should be selected");
        assert_eq!(selected_state, candidates[2]);
    }

    #[test]
    fn get_run_jsonl_entries_prefer_canonical_path_over_legacy_fallbacks() {
        let _lock = crate::shared::test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let temp = tempfile::tempdir().expect("tempdir should be created");
        let _home = EnvVarGuard::set("HOME", Some(temp.path().to_string_lossy().as_ref()));
        let project_root = temp.path().join("project");
        std::fs::create_dir_all(&project_root).expect("project dir should be created");
        let run_id = "trace-jsonl-canonical-precedence";
        let canonical_dir = run_dir(project_root.to_string_lossy().as_ref(), &RunId(run_id.to_string()), None);
        let legacy_dir = project_root.join(".animus").join("runs").join(run_id);
        let legacy_state_dir = project_root.join(".animus").join("state").join("runs").join(run_id);
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
    fn get_run_jsonl_entries_keep_lookup_repo_scoped_despite_config_dir_override() {
        let _lock = crate::shared::test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let temp = tempfile::tempdir().expect("tempdir should be created");
        let _home = EnvVarGuard::set("HOME", Some(temp.path().to_string_lossy().as_ref()));
        let override_dir = temp.path().join("override-config");
        let _ao_config = EnvVarGuard::set("ANIMUS_CONFIG_DIR", Some(override_dir.to_string_lossy().as_ref()));
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
        assert!(canonical_dir.starts_with(temp.path().join(".animus")));
        assert!(!canonical_dir.starts_with(&override_dir));
    }

    #[test]
    fn get_run_jsonl_entries_merges_deterministically_with_source_metadata() {
        let _lock = crate::shared::test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
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
        let _lock = crate::shared::test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
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
        let _lock = crate::shared::test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let temp = tempfile::tempdir().expect("tempdir should be created");
        let _home = EnvVarGuard::set("HOME", Some(temp.path().to_string_lossy().as_ref()));
        let project_root = temp.path().join("project");
        std::fs::create_dir_all(&project_root).expect("project dir should be created");
        let run_id = "trace-jsonl-legacy";
        let legacy_run_dir = project_root.join(".animus").join("runs").join(run_id);
        std::fs::create_dir_all(&legacy_run_dir).expect("legacy run dir should be created");
        std::fs::write(
            legacy_run_dir.join("events.jsonl"),
            "{\"timestamp\":\"2024-01-03T00:00:00Z\",\"kind\":\"legacy\"}\n",
        )
        .expect("legacy events should be written");
        let legacy_state_run_dir = project_root.join(".animus").join("state").join("runs").join(run_id);
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
    fn artifact_dir_candidates_prioritize_scoped_root_over_project_local() {
        let _lock = crate::shared::test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let temp = tempfile::tempdir().expect("tempdir should be created");
        let _home = EnvVarGuard::set("HOME", Some(temp.path().to_string_lossy().as_ref()));
        let project_root = temp.path().join("project");
        std::fs::create_dir_all(&project_root).expect("project dir should be created");
        let execution_id = "exec-artifact-candidates";

        let candidates = artifact_dir_candidates(project_root.to_string_lossy().as_ref(), execution_id);
        assert_eq!(candidates.len(), 2);
        let scoped_root = protocol::scoped_state_root(&project_root).expect("scoped state root");
        assert_eq!(candidates[0], scoped_root.join("artifacts").join(execution_id));
        assert_eq!(candidates[1], project_root.join(".animus").join("artifacts").join(execution_id));

        for candidate in &candidates {
            std::fs::create_dir_all(candidate).expect("candidate artifact dir should be created");
        }
        let resolved = resolve_artifact_dir(project_root.to_string_lossy().as_ref(), execution_id)
            .expect("an artifact dir should be selected");
        assert_eq!(resolved, candidates[0]);
    }

    #[test]
    fn list_artifact_infos_reads_scoped_root_then_falls_back_to_project_local() {
        let _lock = crate::shared::test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let temp = tempfile::tempdir().expect("tempdir should be created");
        let _home = EnvVarGuard::set("HOME", Some(temp.path().to_string_lossy().as_ref()));
        let project_root = temp.path().join("project");
        std::fs::create_dir_all(&project_root).expect("project dir should be created");
        let execution_id = "exec-artifact-scoped";
        let candidates = artifact_dir_candidates(project_root.to_string_lossy().as_ref(), execution_id);

        std::fs::create_dir_all(&candidates[0]).expect("scoped artifact dir should be created");
        std::fs::write(candidates[0].join("report.json"), "{}").expect("scoped artifact should be written");
        let scoped = list_artifact_infos(project_root.to_string_lossy().as_ref(), execution_id)
            .expect("scoped artifacts should list");
        assert_eq!(scoped.len(), 1);
        assert_eq!(scoped[0].artifact_id, "report.json");
        assert!(scoped[0].file_path.as_deref().expect("file path").starts_with(candidates[0].to_str().unwrap()));

        std::fs::remove_dir_all(&candidates[0]).expect("scoped artifact dir should be removed");
        std::fs::create_dir_all(&candidates[1]).expect("legacy artifact dir should be created");
        std::fs::write(candidates[1].join("legacy.txt"), "legacy").expect("legacy artifact should be written");
        let legacy = list_artifact_infos(project_root.to_string_lossy().as_ref(), execution_id)
            .expect("legacy artifacts should list");
        assert_eq!(legacy.len(), 1);
        assert_eq!(legacy[0].artifact_id, "legacy.txt");
    }

    #[test]
    fn list_artifact_infos_rejects_unsafe_execution_ids() {
        for unsafe_id in ["", "../..", "../../etc", "a/b", "a\\b", "/etc/passwd"] {
            let err =
                list_artifact_infos("/tmp/project", unsafe_id).expect_err("unsafe execution id should be rejected");
            assert!(err.to_string().contains("unsafe path segments"), "{unsafe_id}: {err}");
        }
    }

    #[tokio::test]
    async fn output_download_rejects_traversal_ids() {
        let unsafe_pairs = [
            ("../..", "artifact.txt"),
            ("exec-1", "../../../../etc/passwd"),
            ("exec-1", "/etc/passwd"),
            ("exec-1", "..\\escape"),
            ("", "artifact.txt"),
            ("exec-1", ""),
        ];
        for (execution_id, artifact_id) in unsafe_pairs {
            let command = OutputCommand::Download(crate::cli_types::OutputDownloadArgs {
                execution_id: execution_id.to_string(),
                artifact_id: artifact_id.to_string(),
            });
            let err =
                handle_output(command, "/tmp/project", true).await.expect_err("unsafe download ids should be rejected");
            assert!(err.to_string().contains("unsafe path segments"), "{execution_id}/{artifact_id}: {err}");
        }
    }

    fn write_session_checkpoint(
        project_root: &Path,
        workflow_id: &str,
        phase_id: &str,
        run_id: &str,
        started_at: &str,
    ) {
        let workflow_run_dir = run_dir(project_root.to_string_lossy().as_ref(), &RunId(workflow_id.to_string()), None);
        let phases_dir = workflow_run_dir.join("phases");
        std::fs::create_dir_all(&phases_dir).expect("phases dir should be created");
        let checkpoint = serde_json::json!({
            "workflow_id": workflow_id,
            "phase_id": phase_id,
            "provider": "claude",
            "run_id": run_id,
            "status": "completed",
            "started_at": started_at,
        });
        std::fs::write(phases_dir.join(format!("{phase_id}.session.json")), checkpoint.to_string())
            .expect("checkpoint should be written");
    }

    #[test]
    fn run_owner_resolution_prefers_phase_checkpoint_over_same_named_run_directory() {
        let _lock = crate::shared::test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let temp = tempfile::tempdir().expect("tempdir");
        let _home = EnvVarGuard::set("HOME", Some(temp.path().to_string_lossy().as_ref()));
        let project_root = temp.path().join("project");
        std::fs::create_dir_all(&project_root).expect("project");
        write_session_checkpoint(&project_root, "wf-owned", "implementation", "run-phase-1", "2026-07-24T00:00:00Z");
        std::fs::create_dir_all(run_dir(
            project_root.to_string_lossy().as_ref(),
            &RunId("run-phase-1".to_string()),
            None,
        ))
        .expect("phase run dir");

        assert_eq!(
            resolve_workflow_id_for_run(project_root.to_string_lossy().as_ref(), "run-phase-1")
                .expect("resolve owner")
                .as_deref(),
            Some("wf-owned")
        );
    }

    #[test]
    fn latest_run_id_resolution_found_ambiguous_and_none() {
        let _lock = crate::shared::test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let temp = tempfile::tempdir().expect("tempdir should be created");
        let _home = EnvVarGuard::set("HOME", Some(temp.path().to_string_lossy().as_ref()));
        let project_root = temp.path().join("project");
        std::fs::create_dir_all(&project_root).expect("project dir should be created");
        let root = project_root.to_string_lossy().to_string();

        // None: no runs recorded at all.
        let err = resolve_latest_run_id_for_workflow(&root, "wf-none").expect_err("no runs must error");
        assert!(err.to_string().contains("no runs recorded for workflow wf-none"), "{err}");
        assert!(try_latest_run_id_for_workflow(&root, "wf-none").is_none());

        // Found: two phases; the later started_at wins.
        write_session_checkpoint(&project_root, "wf-found", "implementation", "run-early", "2026-06-01T00:00:00Z");
        write_session_checkpoint(&project_root, "wf-found", "review", "run-late", "2026-06-02T00:00:00Z");
        assert_eq!(resolve_latest_run_id_for_workflow(&root, "wf-found").expect("latest run"), "run-late");
        assert_eq!(try_latest_run_id_for_workflow(&root, "wf-found").as_deref(), Some("run-late"));

        // Two phases sharing the same run id at the same instant are NOT
        // ambiguous (one agent run can serve multiple checkpoints).
        write_session_checkpoint(&project_root, "wf-shared", "a", "run-one", "2026-06-03T00:00:00Z");
        write_session_checkpoint(&project_root, "wf-shared", "b", "run-one", "2026-06-03T00:00:00Z");
        assert_eq!(resolve_latest_run_id_for_workflow(&root, "wf-shared").expect("shared run"), "run-one");

        // Ambiguous: distinct run ids share the latest started_at.
        write_session_checkpoint(&project_root, "wf-ambig", "a", "run-a", "2026-06-04T00:00:00Z");
        write_session_checkpoint(&project_root, "wf-ambig", "b", "run-b", "2026-06-04T00:00:00Z");
        let err = resolve_latest_run_id_for_workflow(&root, "wf-ambig").expect_err("tie must be ambiguous");
        let message = err.to_string();
        assert!(message.contains("ambiguous latest run"), "{message}");
        assert!(message.contains("run-a") && message.contains("run-b"), "{message}");
        assert!(try_latest_run_id_for_workflow(&root, "wf-ambig").is_none());
    }

    #[test]
    fn latest_run_id_resolution_falls_back_to_legacy_wf_prefixed_run_dir() {
        let _lock = crate::shared::test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let temp = tempfile::tempdir().expect("tempdir should be created");
        let _home = EnvVarGuard::set("HOME", Some(temp.path().to_string_lossy().as_ref()));
        let project_root = temp.path().join("project");
        std::fs::create_dir_all(&project_root).expect("project dir should be created");
        let root = project_root.to_string_lossy().to_string();

        let legacy_dir = run_dir(&root, &RunId("wf-legacy-flow".to_string()), None);
        std::fs::create_dir_all(&legacy_dir).expect("legacy run dir should be created");
        assert_eq!(resolve_latest_run_id_for_workflow(&root, "legacy-flow").expect("legacy run"), "wf-legacy-flow");

        // Phase-suffixed legacy run dirs (`wf-<workflow_id>-<phase>-...`)
        // resolve too, preferring the one with run output.
        let suffixed_dir = run_dir(&root, &RunId("wf-old-flow-implementation-abc123".to_string()), None);
        std::fs::create_dir_all(&suffixed_dir).expect("suffixed legacy run dir should be created");
        std::fs::write(suffixed_dir.join("events.jsonl"), "{}\n").expect("events should be written");
        let empty_dir = run_dir(&root, &RunId("wf-old-flow-review-def456".to_string()), None);
        std::fs::create_dir_all(&empty_dir).expect("empty legacy run dir should be created");
        assert_eq!(
            resolve_latest_run_id_for_workflow(&root, "old-flow").expect("suffixed legacy run"),
            "wf-old-flow-implementation-abc123"
        );
    }

    #[test]
    fn decision_log_view_reads_fixture_decisions_jsonl() {
        let _lock = crate::shared::test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let temp = tempfile::tempdir().expect("tempdir should be created");
        let _home = EnvVarGuard::set("HOME", Some(temp.path().to_string_lossy().as_ref()));
        let project_root = temp.path().join("project");
        std::fs::create_dir_all(&project_root).expect("project dir should be created");
        let root = project_root.to_string_lossy().to_string();
        let run_id = "run-decisions-fixture";

        let canonical_dir = run_dir(&root, &RunId(run_id.to_string()), None);
        std::fs::create_dir_all(&canonical_dir).expect("run dir should be created");
        let fixture = concat!(
            "{\"kind\":\"metadata\",\"timestamp_ms\":1,\"payload\":{\"kind\":\"session_header\",\"provider_id\":\"claude\",\"model_id\":\"claude-sonnet\"}}\n",
            "{\"kind\":\"prompt\",\"timestamp_ms\":2,\"model_id\":\"claude-sonnet\",\"prompt\":\"fix the bug\",\"runtime_contract\":null}\n",
            "{\"kind\":\"tool_call\",\"timestamp_ms\":3,\"name\":\"Bash\",\"args\":{\"command\":\"cargo test\"}}\n",
            "{\"kind\":\"finished\",\"timestamp_ms\":4,\"exit_code\":0}\n",
        );
        std::fs::write(canonical_dir.join("decisions.jsonl"), fixture).expect("fixture should be written");

        let view = decision_log_view(&root, run_id).expect("decision log should load");
        assert_eq!(view.get("run_id").and_then(Value::as_str), Some(run_id));
        assert_eq!(view.get("provider_id").and_then(Value::as_str), Some("claude"));
        assert_eq!(view.get("truncated_tail").and_then(Value::as_bool), Some(false));
        assert_eq!(view.get("event_count").and_then(Value::as_u64), Some(4));
        let events = view.get("events").and_then(Value::as_array).expect("events array");
        assert_eq!(events.len(), 4);
        assert_eq!(events[0].get("kind").and_then(Value::as_str), Some("metadata"));
        assert_eq!(events[2].get("name").and_then(Value::as_str), Some("Bash"));
        assert_eq!(events[3].get("kind").and_then(Value::as_str), Some("finished"));

        // Missing decision log is a clear not-found error.
        let other_run_dir = run_dir(&root, &RunId("run-without-decisions".to_string()), None);
        std::fs::create_dir_all(&other_run_dir).expect("run dir should be created");
        let err = decision_log_view(&root, "run-without-decisions").expect_err("missing log must error");
        assert!(err.to_string().contains("no decision log for run run-without-decisions"), "{err}");
    }

    #[test]
    fn get_phase_outputs_reads_persisted_payloads() {
        let _lock = crate::shared::test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
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
            risk: None,
            target_phase: None,
            guardrail_violations: Vec::new(),
            payload: Some(serde_json::json!({
                "kind": "implementation_result",
                "verdict": "advance",
                "changed_files": ["src/lib.rs"]
            })),
            requested_skills: Vec::new(),
            resolved_skills: Vec::new(),
            applied_skills: Vec::new(),
        };
        let unit_test = PersistedPhaseOutput {
            phase_id: "unit-test".to_string(),
            completed_at: "2026-03-10T00:05:00Z".to_string(),
            verdict: Some("rework".to_string()),
            confidence: Some(1.0),
            reason: Some("Tests failed".to_string()),
            commit_message: None,
            evidence: Vec::new(),
            risk: None,
            target_phase: None,
            guardrail_violations: Vec::new(),
            payload: Some(serde_json::json!({
                "kind": "phase_result",
                "verdict": "rework",
                "failure_category": "tests_failed"
            })),
            requested_skills: Vec::new(),
            resolved_skills: Vec::new(),
            applied_skills: Vec::new(),
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

    fn phase_output_fixture(phase_id: &str) -> PersistedPhaseOutput {
        PersistedPhaseOutput {
            phase_id: phase_id.to_string(),
            completed_at: "2026-06-11T00:00:00Z".to_string(),
            verdict: Some("advance".to_string()),
            confidence: Some(0.9),
            reason: Some("Looks good".to_string()),
            commit_message: None,
            evidence: Vec::new(),
            risk: None,
            target_phase: None,
            guardrail_violations: Vec::new(),
            payload: None,
            requested_skills: Vec::new(),
            resolved_skills: Vec::new(),
            applied_skills: Vec::new(),
        }
    }

    #[test]
    fn render_phase_outputs_human_shows_applied_resolved_and_missing_skills() {
        let mut output = phase_output_fixture("code-review");
        output.requested_skills =
            vec!["review-checklist".to_string(), "gemini-only".to_string(), "security-audit".to_string()];
        output.applied_skills = vec![PersistedPhaseSkill {
            name: "review-checklist".to_string(),
            source: "project".to_string(),
            contributions: vec!["prompt".to_string(), "tool_policy".to_string()],
        }];
        output.resolved_skills = vec![
            PersistedPhaseSkill {
                name: "review-checklist".to_string(),
                source: "project".to_string(),
                contributions: vec!["prompt".to_string(), "tool_policy".to_string()],
            },
            PersistedPhaseSkill {
                name: "gemini-only".to_string(),
                source: "user".to_string(),
                contributions: vec!["prompt".to_string()],
            },
        ];

        let rendered = render_phase_outputs_human("wf-skill-verify", None, &[output]);
        assert!(rendered.contains("Workflow: wf-skill-verify"), "{rendered}");
        assert!(rendered.contains("Phase: code-review"), "{rendered}");
        assert!(rendered.contains("verdict:   advance (confidence 0.90)"), "{rendered}");
        assert!(rendered.contains("requested: review-checklist, gemini-only, security-audit"), "{rendered}");
        assert!(rendered.contains("applied:   review-checklist (project)  [prompt, tool_policy]"), "{rendered}");
        assert!(
            rendered.contains(
                "resolved:  gemini-only (user)  [prompt]  (not applied \u{2014} activation did not match the selected tool/model)"
            ),
            "{rendered}"
        );
        assert!(
            rendered.contains("missing:   security-audit  (not found \u{2014} check `animus skill list`)"),
            "{rendered}"
        );
        // The applied skill must not also be listed as resolved-not-applied.
        assert_eq!(rendered.matches("review-checklist (project)").count(), 1, "{rendered}");
    }

    #[test]
    fn render_phase_outputs_human_omits_skills_block_when_no_skill_metadata() {
        let rendered = render_phase_outputs_human("wf-plain", None, &[phase_output_fixture("implementation")]);
        assert!(rendered.contains("Phase: implementation"), "{rendered}");
        assert!(!rendered.contains("Skills:"), "{rendered}");
        assert!(!rendered.contains("missing:"), "{rendered}");
    }

    #[test]
    fn render_phase_outputs_human_reports_empty_outputs() {
        let rendered = render_phase_outputs_human("wf-empty", None, &[]);
        assert!(rendered.contains("No persisted phase outputs for workflow wf-empty."), "{rendered}");
        let filtered = render_phase_outputs_human("wf-empty", Some("review"), &[]);
        assert!(filtered.contains("No persisted output for phase 'review' of workflow wf-empty."), "{filtered}");
    }

    #[test]
    fn phase_outputs_json_shape_is_unchanged_by_skill_fields() {
        // Old persisted outputs (no skill fields) keep deserializing, and
        // serialization keeps omitting the skill keys when empty so the
        // `--json` envelope is byte-compatible for skill-less phases.
        let legacy = r#"{"phase_id":"impl","completed_at":"2026-06-11T00:00:00Z","verdict":"advance"}"#;
        let parsed: PersistedPhaseOutput = serde_json::from_str(legacy).expect("legacy output should deserialize");
        assert!(parsed.requested_skills.is_empty());
        assert!(parsed.resolved_skills.is_empty());
        assert!(parsed.applied_skills.is_empty());
        let serialized = serde_json::to_value(&parsed).expect("serialize");
        assert!(serialized.get("requested_skills").is_none(), "{serialized}");
        assert!(serialized.get("resolved_skills").is_none(), "{serialized}");
        assert!(serialized.get("applied_skills").is_none(), "{serialized}");
    }
}
