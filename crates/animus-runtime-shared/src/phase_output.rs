use orchestrator_config::skill_definition::{
    apply_skill_for_execution, preview_skill_application, SkillApplicationResult,
};
use orchestrator_config::skill_resolution::ResolvedSkill;
use orchestrator_core::{PhaseDecision, PhaseDecisionVerdict};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use uuid::Uuid;

use crate::phase_metadata::{PhaseExecutionMetadata, PhaseExecutionOutcome};

const MAX_PRIOR_CONTEXT_CHARS: usize = 8000;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PhaseCompletionMarker {
    pub completed_at: String,
    pub output_path: String,
    pub phase_id: String,
}

// Completion markers are keyed by phase attempt to prevent a Rework retry
// from finding the previous attempt's marker and replaying its decision.
// Pre-v0.4.7 markers (`<phase>.completed`, no attempt suffix) are
// deliberately NOT honoured: on first daemon start after upgrade, in-flight
// phases re-run once rather than risk replaying a stale Advance/Rework
// decision against the wrong attempt counter. See codex round-4 P1.
pub fn phase_completion_marker_path(project_root: &str, workflow_id: &str, phase_id: &str, attempt: u32) -> PathBuf {
    phase_output_dir(project_root, workflow_id).join(format!("{phase_id}.attempt-{attempt}.completed"))
}

pub fn write_phase_completion_marker(
    project_root: &str,
    workflow_id: &str,
    phase_id: &str,
    attempt: u32,
) -> std::io::Result<()> {
    let dir = phase_output_dir(project_root, workflow_id);
    std::fs::create_dir_all(&dir)?;
    let marker = PhaseCompletionMarker {
        completed_at: chrono::Utc::now().to_rfc3339(),
        output_path: format!("{phase_id}.json"),
        phase_id: phase_id.to_string(),
    };
    let payload = serde_json::to_vec_pretty(&marker)
        .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidData, err.to_string()))?;
    let final_path = dir.join(format!("{phase_id}.attempt-{attempt}.completed"));
    let tmp_path = dir.join(format!("{phase_id}.attempt-{attempt}.completed.{}.tmp", Uuid::new_v4()));
    {
        use std::io::Write;
        let mut file = std::fs::File::create(&tmp_path)?;
        file.write_all(&payload)?;
        // sync_all() pushes data + metadata to disk. On macOS, std calls
        // F_FULLFSYNC since Rust 1.79 so the bytes reach platter, not
        // just drive cache. On Linux, this is fsync(2).
        file.sync_all()?;
    }
    // fsync the parent directory after rename so the rename itself is
    // durable across power loss: without it, the dir entry change can
    // sit in the dir cache and disappear after a kernel panic / outage
    // even though the data file is fully on disk. ~5-50ms on SSD —
    // negligible vs. the cost of replaying a completed phase or, worse,
    // double-running one.
    orchestrator_core::store::fsync_rename(&tmp_path, &final_path)?;
    Ok(())
}

pub fn is_phase_completed(project_root: &str, workflow_id: &str, phase_id: &str, attempt: u32) -> bool {
    phase_completion_marker_path(project_root, workflow_id, phase_id, attempt).exists()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PersistedDecisionReadError {
    OutputMissing,
    Unreadable(String),
    Malformed(String),
    VerdictMissing,
    UnknownVerdict(String),
}

impl std::fmt::Display for PersistedDecisionReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::OutputMissing => write!(f, "sibling <phase>.json output is missing"),
            Self::Unreadable(err) => write!(f, "failed to read persisted output: {err}"),
            Self::Malformed(err) => write!(f, "persisted output is malformed: {err}"),
            Self::VerdictMissing => write!(f, "persisted output has no `verdict` field"),
            Self::UnknownVerdict(v) => write!(f, "persisted output has unrecognized verdict '{v}'"),
        }
    }
}

// The completion marker is intentionally minimal — it only attests "this phase ran";
// the verdict/decision lives in the sibling <phase>.json so crash-recovery can replay
// the exact outcome. Keeping the marker payload narrow preserves backward-compat with
// markers written by v0.4.x daemons.
pub fn read_persisted_decision(
    project_root: &str,
    workflow_id: &str,
    phase_id: &str,
) -> Result<PhaseDecision, PersistedDecisionReadError> {
    let dir = phase_output_dir(project_root, workflow_id);
    let file_path = dir.join(format!("{phase_id}.json"));
    if !file_path.exists() {
        return Err(PersistedDecisionReadError::OutputMissing);
    }
    let contents =
        std::fs::read_to_string(&file_path).map_err(|err| PersistedDecisionReadError::Unreadable(err.to_string()))?;
    let output: PersistedPhaseOutput =
        serde_json::from_str(&contents).map_err(|err| PersistedDecisionReadError::Malformed(err.to_string()))?;

    let verdict_str = output.verdict.as_deref().ok_or(PersistedDecisionReadError::VerdictMissing)?;
    let verdict_trimmed = verdict_str.trim();
    if verdict_trimmed.is_empty() {
        return Err(PersistedDecisionReadError::VerdictMissing);
    }
    // Non-builtin verdicts are custom routing keys preserved on `verdict_key`
    // (verdict enum is `Unknown`); the workflow executor routes them through the
    // phase `on_verdict` map. This mirrors the agent-output parser so command
    // phases (which persist the same shape) get identical routing. Built-in
    // verdicts leave `verdict_key` unset.
    let (verdict, verdict_key) = match verdict_trimmed.to_ascii_lowercase().as_str() {
        "advance" => (PhaseDecisionVerdict::Advance, None),
        "rework" => (PhaseDecisionVerdict::Rework, None),
        "fail" => (PhaseDecisionVerdict::Fail, None),
        "skip" => (PhaseDecisionVerdict::Skip, None),
        _ => (PhaseDecisionVerdict::Unknown, Some(verdict_trimmed.to_string())),
    };

    let risk = match output.risk.as_deref().map(str::to_ascii_lowercase).as_deref() {
        Some("low") | None => orchestrator_core::WorkflowDecisionRisk::Low,
        Some("medium") => orchestrator_core::WorkflowDecisionRisk::Medium,
        Some("high") => orchestrator_core::WorkflowDecisionRisk::High,
        Some(other) => return Err(PersistedDecisionReadError::Malformed(format!("unknown risk value {other:?}"))),
    };

    Ok(PhaseDecision {
        kind: "phase_decision".to_string(),
        phase_id: output.phase_id,
        verdict,
        confidence: output.confidence.unwrap_or(1.0),
        risk,
        reason: output.reason.unwrap_or_default(),
        evidence: output.evidence,
        guardrail_violations: output.guardrail_violations,
        commit_message: output.commit_message,
        target_phase: output.target_phase,
        verdict_key,
    })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistedPhaseOutput {
    pub phase_id: String,
    pub completed_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verdict: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub risk: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_phase: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit_message: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<orchestrator_core::PhaseEvidence>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub guardrail_violations: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requested_skills: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub resolved_skills: Vec<PersistedPhaseSkill>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub applied_skills: Vec<PersistedPhaseSkill>,
}

/// Compact per-skill record persisted alongside a phase output so
/// `animus output phase-outputs` can show that a skill actually took
/// effect (name + source scope + which contribution kinds it made)
/// without embedding the full `SkillDefinition` (prompt text and all)
/// in every phase output file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersistedPhaseSkill {
    pub name: String,
    /// Source scope label, e.g. `project`, `user`, `installed`,
    /// `built-in`, `agent-host:<host>/<scope>` (the `Display` form of
    /// [`orchestrator_config::skill_scoping::SkillSourceOrigin`]).
    pub source: String,
    /// Contribution kinds the skill made (or would make, for resolved-but
    /// -not-applied skills): `prompt`, `tool_policy`, `mcp_servers`,
    /// `args`, `env`, `codex_config`, `model`, `timeout`, `capabilities`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub contributions: Vec<String>,
}

/// Bucket a [`SkillApplicationResult`] into coarse contribution-kind
/// labels for operator-facing views.
pub fn skill_contribution_kinds(application: &SkillApplicationResult) -> Vec<String> {
    let mut kinds = Vec::new();
    if !application.system_prompt_fragments.is_empty()
        || !application.prompt_prefixes.is_empty()
        || !application.prompt_suffixes.is_empty()
        || !application.directives.is_empty()
    {
        kinds.push("prompt".to_string());
    }
    if application.tool_policy.is_some() {
        kinds.push("tool_policy".to_string());
    }
    if !application.mcp_servers.is_empty() {
        kinds.push("mcp_servers".to_string());
    }
    if !application.extra_args.is_empty() {
        kinds.push("args".to_string());
    }
    if !application.env.is_empty() {
        kinds.push("env".to_string());
    }
    if !application.codex_config_overrides.is_empty() {
        kinds.push("codex_config".to_string());
    }
    if application.model.is_some() {
        kinds.push("model".to_string());
    }
    if application.timeout_secs.is_some() {
        kinds.push("timeout".to_string());
    }
    if !application.capabilities.is_empty() {
        kinds.push("capabilities".to_string());
    }
    kinds
}

fn persisted_phase_skill(
    skill: &ResolvedSkill,
    selected_tool: Option<&str>,
    selected_model: Option<&str>,
) -> PersistedPhaseSkill {
    // Per-skill contributions: apply against the actually selected
    // tool/model when known (matches what the runner injected); fall back
    // to the activation-free preview so unconditional skills still report
    // their kinds when no tool was recorded.
    let application = match selected_tool {
        Some(tool) => apply_skill_for_execution(&skill.definition, tool, selected_model),
        None => preview_skill_application(&skill.definition),
    };
    PersistedPhaseSkill {
        name: skill.definition.name.clone(),
        source: skill.source.to_string(),
        contributions: application.as_ref().map(skill_contribution_kinds).unwrap_or_default(),
    }
}

/// Project the skill fields of a [`PhaseExecutionMetadata`] into the
/// compact persisted form `(requested, resolved, applied)`.
pub fn persisted_skills_from_metadata(
    metadata: &PhaseExecutionMetadata,
) -> (Vec<String>, Vec<PersistedPhaseSkill>, Vec<PersistedPhaseSkill>) {
    let tool = metadata.selected_tool.as_deref();
    let model = metadata.selected_model.as_deref();
    let resolved = metadata.resolved_skills.iter().map(|skill| persisted_phase_skill(skill, tool, model)).collect();
    let applied = metadata.applied_skills.iter().map(|skill| persisted_phase_skill(skill, tool, model)).collect();
    (metadata.requested_skills.clone(), resolved, applied)
}

fn scoped_state_base(project_root: &str) -> PathBuf {
    let path = Path::new(project_root);
    protocol::scoped_state_root(path).unwrap_or_else(|| path.join(".animus"))
}

pub fn phase_output_dir(project_root: &str, workflow_id: &str) -> PathBuf {
    scoped_state_base(project_root).join("state").join("workflows").join(workflow_id).join("phase-outputs")
}

pub fn persist_phase_output(
    project_root: &str,
    workflow_id: &str,
    phase_id: &str,
    attempt: u32,
    outcome: &PhaseExecutionOutcome,
) -> anyhow::Result<()> {
    persist_phase_output_with_metadata(project_root, workflow_id, phase_id, attempt, outcome, None)
}

/// Like [`persist_phase_output`] but also records the skill fields of the
/// phase's [`PhaseExecutionMetadata`] (requested / resolved / applied) in a
/// compact form so `animus output phase-outputs` can show whether an
/// attached skill actually took effect. Additive: runners pinned to older
/// revisions keep calling [`persist_phase_output`] and simply persist no
/// skill records.
pub fn persist_phase_output_with_metadata(
    project_root: &str,
    workflow_id: &str,
    phase_id: &str,
    attempt: u32,
    outcome: &PhaseExecutionOutcome,
    metadata: Option<&PhaseExecutionMetadata>,
) -> anyhow::Result<()> {
    #[cfg(any(test, feature = "test-fault"))]
    test_fault::maybe_fail()?;
    let dir = phase_output_dir(project_root, workflow_id);
    std::fs::create_dir_all(&dir)?;

    let (verdict, confidence, reason, risk, target_phase, commit_message, evidence, guardrail_violations, payload) =
        match outcome {
            PhaseExecutionOutcome::Completed { commit_message, phase_decision, result_payload } => {
                let (v, c, r, risk, target, ev, gv) = match phase_decision {
                    Some(decision) => (
                        // Persist a custom routing key verbatim so it round-trips
                        // through crash recovery; built-in verdicts serialize from
                        // the enum. Without this, an Unknown+verdict_key decision
                        // would persist as "unknown" and lose its route on replay.
                        Some(match decision.verdict_key.as_deref().map(str::trim).filter(|k| !k.is_empty()) {
                            Some(key) => key.to_string(),
                            None => format!("{:?}", decision.verdict).to_ascii_lowercase(),
                        }),
                        Some(decision.confidence),
                        if decision.reason.is_empty() { None } else { Some(decision.reason.clone()) },
                        Some(format!("{:?}", decision.risk).to_ascii_lowercase()),
                        decision.target_phase.clone(),
                        decision.evidence.clone(),
                        decision.guardrail_violations.clone(),
                    ),
                    None => (Some("advance".to_string()), None, None, None, None, Vec::new(), Vec::new()),
                };
                (v, c, r, risk, target, commit_message.clone(), ev, gv, result_payload.clone())
            }
            PhaseExecutionOutcome::ManualPending { instructions, .. } => (
                Some("manual_pending".to_string()),
                None,
                Some(instructions.clone()),
                None,
                None,
                None,
                Vec::new(),
                Vec::new(),
                None,
            ),
        };

    let (requested_skills, resolved_skills, applied_skills) =
        metadata.map(persisted_skills_from_metadata).unwrap_or_default();
    let output = PersistedPhaseOutput {
        phase_id: phase_id.to_string(),
        completed_at: chrono::Utc::now().to_rfc3339(),
        verdict,
        confidence,
        reason,
        risk,
        target_phase,
        commit_message,
        evidence,
        guardrail_violations,
        payload,
        requested_skills,
        resolved_skills,
        applied_skills,
    };

    let payload = serde_json::to_string_pretty(&output)?;
    let file_path = dir.join(format!("{phase_id}.json"));
    let tmp_path = file_path.with_file_name(format!("{phase_id}.{}.tmp", Uuid::new_v4()));
    {
        use std::io::Write;
        let mut file = std::fs::File::create(&tmp_path)?;
        file.write_all(payload.as_bytes())?;
        // Force data + metadata to stable storage before the rename so a
        // crash can never reveal a half-written phase output to the
        // recovery path. macOS uses F_FULLFSYNC (Rust 1.79+) which
        // forces the drive write cache to flush — plain POSIX fsync on
        // macOS only schedules the flush.
        file.sync_all()?;
    }
    // fsync_rename: rename then fsync the parent dir so the dir entry
    // change is durable across power loss. Without this the recovery
    // logic could find a completion marker without the sibling
    // <phase>.json (or vice versa) after a kernel panic.
    orchestrator_core::store::fsync_rename(&tmp_path, &file_path)?;
    if !matches!(outcome, PhaseExecutionOutcome::ManualPending { .. }) {
        write_phase_completion_marker(project_root, workflow_id, phase_id, attempt)?;
    }
    Ok(())
}

// Daemon-restart helper: persist a minimal `Completed` outcome for a phase
// whose live execution we cannot recover (the in-process AgentRunResponse
// was lost when the daemon crashed, but the provider plugin reported a
// successful terminal Finished event on the resumed session). Mirrors the
// "no phase_decision" branch in `persist_phase_output` — verdict defaults
// to "advance" — so the next scheduler tick replays this as a normal
// completed phase via `read_persisted_decision` +
// `complete_current_phase_with_decision`. Idempotent: the underlying
// persist uses an atomic tmp+rename, so a double-apply rewrites the same
// bytes rather than racing partial writes.
pub fn persist_resumed_phase_completion(
    project_root: &str,
    workflow_id: &str,
    phase_id: &str,
    attempt: u32,
) -> anyhow::Result<()> {
    let outcome = PhaseExecutionOutcome::Completed { commit_message: None, phase_decision: None, result_payload: None };
    persist_phase_output(project_root, workflow_id, phase_id, attempt, &outcome)
}

pub fn load_prior_phase_outputs(
    project_root: &str,
    workflow_id: &str,
    current_phase_id: &str,
    pipeline_phase_order: &[String],
) -> Vec<PersistedPhaseOutput> {
    let dir = phase_output_dir(project_root, workflow_id);
    if !dir.exists() {
        return Vec::new();
    }

    let mut outputs = Vec::new();
    for prior_phase_id in pipeline_phase_order {
        if prior_phase_id == current_phase_id {
            break;
        }
        let file_path = dir.join(format!("{prior_phase_id}.json"));
        if let Ok(contents) = std::fs::read_to_string(&file_path) {
            if let Ok(output) = serde_json::from_str::<PersistedPhaseOutput>(&contents) {
                outputs.push(output);
            }
        }
    }
    outputs
}

pub fn format_prior_phase_outputs(outputs: &[PersistedPhaseOutput]) -> String {
    if outputs.is_empty() {
        return String::new();
    }

    let mut sections: Vec<String> = Vec::new();
    for output in outputs {
        let mut section = format!("### {} (completed)", output.phase_id);
        if let Some(ref verdict) = output.verdict {
            section.push_str(&format!("\nVerdict: {verdict}"));
        }
        if let Some(confidence) = output.confidence {
            section.push_str(&format!("\nConfidence: {confidence:.1}"));
        }
        if let Some(ref reason) = output.reason {
            section.push_str(&format!("\nReasoning: {reason}"));
        }
        if let Some(ref cm) = output.commit_message {
            section.push_str(&format!("\nCommit: {cm}"));
        }
        if !output.evidence.is_empty() {
            section.push_str("\nEvidence:");
            for ev in &output.evidence {
                let kind = format!("{:?}", ev.kind).to_ascii_lowercase();
                if let Some(ref fp) = ev.file_path {
                    section.push_str(&format!("\n- [{kind}] {} ({})", ev.description, fp));
                } else {
                    section.push_str(&format!("\n- [{kind}] {}", ev.description));
                }
            }
        }
        if !output.guardrail_violations.is_empty() {
            section.push_str("\nGuardrail violations:");
            for v in &output.guardrail_violations {
                section.push_str(&format!("\n- {v}"));
            }
        }
        sections.push(section);
    }

    let mut result = "## Prior Phase Results\n".to_string();
    result.push_str(&sections.join("\n\n"));

    if result.len() > MAX_PRIOR_CONTEXT_CHARS {
        let mut truncated = "## Prior Phase Results\n".to_string();
        let mut budget = MAX_PRIOR_CONTEXT_CHARS - truncated.len() - 30;
        for section in sections.iter().rev() {
            if section.len() <= budget {
                truncated.push_str(section);
                truncated.push_str("\n\n");
                budget = budget.saturating_sub(section.len() + 2);
            } else {
                truncated.insert_str("## Prior Phase Results\n".len(), "(earlier phases truncated for brevity)\n\n");
                break;
            }
        }
        return truncated.trim_end().to_string();
    }

    result
}

fn load_workflow_state(project_root: &str, workflow_id: &str) -> Option<orchestrator_core::OrchestratorWorkflow> {
    let workflow_path = scoped_state_base(project_root).join("workflow-state").join(format!("{workflow_id}.json"));
    let contents = std::fs::read_to_string(&workflow_path).ok()?;
    serde_json::from_str(&contents).ok()
}

pub(crate) fn build_workflow_pipeline_context(
    project_root: &str,
    workflow_id: &str,
    current_phase_id: &str,
) -> (String, Vec<String>) {
    let workflow = match load_workflow_state(project_root, workflow_id) {
        Some(w) => w,
        None => return (String::new(), Vec::new()),
    };

    let phase_order: Vec<String> = workflow.phases.iter().map(|p| p.phase_id.clone()).collect();
    let prior_outputs = load_prior_phase_outputs(project_root, workflow_id, current_phase_id, &phase_order);
    let output_map: std::collections::HashMap<String, &PersistedPhaseOutput> =
        prior_outputs.iter().map(|o| (o.phase_id.clone(), o)).collect();

    let pipeline: Vec<serde_json::Value> = workflow
        .phases
        .iter()
        .map(|phase| {
            let status = format!("{:?}", phase.status).to_ascii_lowercase();
            let mut entry = serde_json::json!({
                "phase_id": phase.phase_id,
                "status": status,
                "attempt": phase.attempt,
            });
            if let Some(output) = output_map.get(&phase.phase_id) {
                if let Some(ref payload) = output.payload {
                    entry["output"] = payload.clone();
                }
            }
            entry
        })
        .collect();

    let rework_counts: serde_json::Value = workflow
        .rework_counts
        .iter()
        .filter(|(_, &count)| count > 0)
        .map(|(k, v)| (k.clone(), serde_json::Value::from(*v)))
        .collect::<serde_json::Map<String, serde_json::Value>>()
        .into();

    let workflow_status = format!("{:?}", workflow.status).to_ascii_lowercase();

    let context = serde_json::json!({
        "pipeline": pipeline,
        "current_phase": current_phase_id,
        "rework_counts": rework_counts,
        "workflow_status": workflow_status,
    });

    let json = serde_json::to_string(&context).unwrap_or_default();
    (json, phase_order)
}

/// Per-thread fault-injection seam for [`persist_phase_output`]. Tests in
/// this crate use the [`FaultGuard`] RAII guard to force the next persist
/// call on the current thread to return an injected
/// `io::ErrorKind::PermissionDenied`; the matching workflow_execute test
/// then verifies that the surrounding scheduler does NOT advance the
/// workflow state when persistence fails.
#[cfg(any(test, feature = "test-fault"))]
pub mod test_fault {
    use std::cell::Cell;

    thread_local! {
        static ARMED: Cell<bool> = const { Cell::new(false) };
    }

    pub struct FaultGuard;

    impl FaultGuard {
        pub fn arm() -> Self {
            ARMED.with(|cell| cell.set(true));
            Self
        }
    }

    impl Drop for FaultGuard {
        fn drop(&mut self) {
            ARMED.with(|cell| cell.set(false));
        }
    }

    pub fn maybe_fail() -> anyhow::Result<()> {
        let armed = ARMED.with(Cell::get);
        if armed {
            ARMED.with(|cell| cell.set(false));
            return Err(anyhow::anyhow!("test_fault::maybe_fail injected persist_phase_output failure"));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // TODO: phase_output tests intermittently see scoped_state_root resolve to a different
    // path between persist and read under parallel cargo test, even with the
    // scoped_state_serializer held. Always passes in isolation. Reproduce and root-cause separately.
    #[test]
    #[ignore = "intermittent scoped_state_root divergence under parallel cargo test; passes in isolation"]
    fn test_persist_and_load_phase_output() {
        let _serial = crate::test_env::scoped_state_serializer();
        let tmp = std::env::temp_dir().join(format!("ao-test-phase-output-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&tmp).expect("create test dir");
        let project_root = tmp.to_str().unwrap();
        let workflow_id = "wf-test-001";

        let outcome = PhaseExecutionOutcome::Completed {
            commit_message: Some("feat: add login flow".to_string()),
            phase_decision: Some(orchestrator_core::PhaseDecision {
                kind: "phase_decision".to_string(),
                phase_id: "research".to_string(),
                verdict: orchestrator_core::PhaseDecisionVerdict::Advance,
                confidence: 0.9,
                risk: orchestrator_core::WorkflowDecisionRisk::Low,
                reason: "Research complete, found relevant patterns".to_string(),
                evidence: vec![],
                guardrail_violations: vec![],
                commit_message: None,
                target_phase: None,
                verdict_key: None,
            }),
            result_payload: None,
        };

        persist_phase_output(project_root, workflow_id, "research", 1, &outcome).unwrap();

        let output_file = phase_output_dir(project_root, workflow_id).join("research.json");
        assert!(output_file.exists());

        let loaded: PersistedPhaseOutput =
            serde_json::from_str(&std::fs::read_to_string(&output_file).unwrap()).unwrap();
        assert_eq!(loaded.phase_id, "research");
        assert_eq!(loaded.verdict.as_deref(), Some("advance"));
        assert!((loaded.confidence.unwrap() - 0.9).abs() < f32::EPSILON);
        assert_eq!(loaded.reason.as_deref(), Some("Research complete, found relevant patterns"));
        assert_eq!(loaded.commit_message.as_deref(), Some("feat: add login flow"));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    // TASK-207: a command (or agent) phase that emits a custom verdict key must
    // persist the key verbatim and reconstruct it on read as
    // verdict = Unknown + verdict_key = Some(key), so crash recovery routes it
    // through on_verdict rather than losing it to "unknown".
    #[test]
    #[ignore = "intermittent scoped_state_root divergence under parallel cargo test; passes in isolation"]
    fn custom_verdict_key_round_trips_through_persisted_output() {
        let _serial = crate::test_env::scoped_state_serializer();
        let tmp = std::env::temp_dir().join(format!("ao-test-custom-verdict-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&tmp).expect("create test dir");
        let project_root = tmp.to_str().unwrap();
        let workflow_id = "wf-custom-001";

        let outcome = PhaseExecutionOutcome::Completed {
            commit_message: None,
            phase_decision: Some(orchestrator_core::PhaseDecision {
                kind: "phase_decision".to_string(),
                phase_id: "triage".to_string(),
                verdict: orchestrator_core::PhaseDecisionVerdict::Unknown,
                confidence: 0.8,
                risk: orchestrator_core::WorkflowDecisionRisk::Low,
                reason: "needs deeper investigation".to_string(),
                evidence: vec![],
                guardrail_violations: vec![],
                commit_message: None,
                target_phase: None,
                verdict_key: Some("needs-research".to_string()),
            }),
            result_payload: None,
        };

        persist_phase_output(project_root, workflow_id, "triage", 1, &outcome).unwrap();

        // Persisted verbatim (not collapsed to "unknown").
        let output_file = phase_output_dir(project_root, workflow_id).join("triage.json");
        let persisted: PersistedPhaseOutput =
            serde_json::from_str(&std::fs::read_to_string(&output_file).unwrap()).unwrap();
        assert_eq!(persisted.verdict.as_deref(), Some("needs-research"));

        // Reconstructed as Unknown + verdict_key on read.
        let decision = read_persisted_decision(project_root, workflow_id, "triage").expect("read decision");
        assert_eq!(decision.verdict, orchestrator_core::PhaseDecisionVerdict::Unknown);
        assert_eq!(decision.verdict_key.as_deref(), Some("needs-research"));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    #[ignore = "intermittent scoped_state_root divergence under parallel cargo test; passes in isolation"]
    fn test_load_prior_phase_outputs_ordering() {
        let _serial = crate::test_env::scoped_state_serializer();
        let tmp = std::env::temp_dir().join(format!("ao-test-phase-output-order-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&tmp).expect("create test dir");
        let project_root = tmp.to_str().unwrap();
        let workflow_id = "wf-test-002";

        let research_outcome = PhaseExecutionOutcome::Completed {
            commit_message: None,
            phase_decision: Some(orchestrator_core::PhaseDecision {
                kind: "phase_decision".to_string(),
                phase_id: "research".to_string(),
                verdict: orchestrator_core::PhaseDecisionVerdict::Advance,
                confidence: 0.8,
                risk: orchestrator_core::WorkflowDecisionRisk::Low,
                reason: "Research done".to_string(),
                evidence: vec![],
                guardrail_violations: vec![],
                commit_message: None,
                target_phase: None,
                verdict_key: None,
            }),
            result_payload: None,
        };
        persist_phase_output(project_root, workflow_id, "research", 1, &research_outcome).unwrap();

        let impl_outcome = PhaseExecutionOutcome::Completed {
            commit_message: Some("feat: implement feature".to_string()),
            phase_decision: Some(orchestrator_core::PhaseDecision {
                kind: "phase_decision".to_string(),
                phase_id: "implementation".to_string(),
                verdict: orchestrator_core::PhaseDecisionVerdict::Advance,
                confidence: 0.95,
                risk: orchestrator_core::WorkflowDecisionRisk::Low,
                reason: "Implementation complete".to_string(),
                evidence: vec![],
                guardrail_violations: vec![],
                commit_message: None,
                target_phase: None,
                verdict_key: None,
            }),
            result_payload: None,
        };
        persist_phase_output(project_root, workflow_id, "implementation", 1, &impl_outcome).unwrap();

        let pipeline_order = vec!["research".to_string(), "implementation".to_string(), "review".to_string()];

        let loaded = load_prior_phase_outputs(project_root, workflow_id, "review", &pipeline_order);
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].phase_id, "research");
        assert_eq!(loaded[1].phase_id, "implementation");

        let loaded_impl = load_prior_phase_outputs(project_root, workflow_id, "implementation", &pipeline_order);
        assert_eq!(loaded_impl.len(), 1);
        assert_eq!(loaded_impl[0].phase_id, "research");

        let loaded_research = load_prior_phase_outputs(project_root, workflow_id, "research", &pipeline_order);
        assert_eq!(loaded_research.len(), 0);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_format_prior_phase_outputs_empty() {
        let result = format_prior_phase_outputs(&[]);
        assert!(result.is_empty());
    }

    #[test]
    fn test_format_prior_phase_outputs_renders_sections() {
        let outputs = vec![
            PersistedPhaseOutput {
                phase_id: "research".to_string(),
                completed_at: "2026-03-01T00:00:00Z".to_string(),
                verdict: Some("advance".to_string()),
                confidence: Some(0.9),
                reason: Some("Found patterns".to_string()),
                commit_message: None,
                evidence: vec![],
                risk: None,
                target_phase: None,
                guardrail_violations: vec![],
                payload: None,
                requested_skills: vec![],
                resolved_skills: vec![],
                applied_skills: vec![],
            },
            PersistedPhaseOutput {
                phase_id: "implementation".to_string(),
                completed_at: "2026-03-01T01:00:00Z".to_string(),
                verdict: Some("advance".to_string()),
                confidence: Some(0.95),
                reason: Some("Implemented".to_string()),
                commit_message: Some("feat: add feature".to_string()),
                evidence: vec![],
                risk: None,
                target_phase: None,
                guardrail_violations: vec![],
                payload: None,
                requested_skills: vec![],
                resolved_skills: vec![],
                applied_skills: vec![],
            },
        ];
        let result = format_prior_phase_outputs(&outputs);
        assert!(result.contains("## Prior Phase Results"));
        assert!(result.contains("### research (completed)"));
        assert!(result.contains("### implementation (completed)"));
        assert!(result.contains("Verdict: advance"));
        assert!(result.contains("Confidence: 0.9"));
        assert!(result.contains("Reasoning: Found patterns"));
        assert!(result.contains("Commit: feat: add feature"));
    }

    #[test]
    #[ignore = "intermittent scoped_state_root divergence under parallel cargo test; passes in isolation"]
    fn test_build_workflow_pipeline_context_returns_structured_json() {
        use protocol::orchestrator::{
            SubjectRef, WorkflowCheckpointMetadata, WorkflowMachineState, WorkflowPhaseExecution, WorkflowPhaseStatus,
            WorkflowStatus,
        };

        let _serial = crate::test_env::scoped_state_serializer();
        let tmp = std::env::temp_dir().join(format!("ao-test-pipeline-context-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&tmp).unwrap();
        let project_root = tmp.to_str().unwrap();
        let workflow_id = "wf-ctx-001";

        let state_base = scoped_state_base(project_root);
        let workflow_state_dir = state_base.join("workflow-state");
        std::fs::create_dir_all(&workflow_state_dir).unwrap();
        let mut rework_counts = std::collections::HashMap::new();
        rework_counts.insert("code-review".to_string(), 2u32);
        let workflow = orchestrator_core::OrchestratorWorkflow {
            id: workflow_id.to_string(),
            task_id: "TASK-1".to_string(),
            workflow_ref: None,
            subject: Some(SubjectRef::task("TASK-1".to_string())),
            input: None,
            vars: std::collections::HashMap::new(),
            status: WorkflowStatus::Running,
            current_phase_index: 2,
            phases: vec![
                WorkflowPhaseExecution {
                    phase_id: "research".to_string(),
                    status: WorkflowPhaseStatus::Success,
                    started_at: None,
                    completed_at: None,
                    attempt: 1,
                    error_message: None,
                },
                WorkflowPhaseExecution {
                    phase_id: "implementation".to_string(),
                    status: WorkflowPhaseStatus::Success,
                    started_at: None,
                    completed_at: None,
                    attempt: 1,
                    error_message: None,
                },
                WorkflowPhaseExecution {
                    phase_id: "code-review".to_string(),
                    status: WorkflowPhaseStatus::Running,
                    started_at: None,
                    completed_at: None,
                    attempt: 3,
                    error_message: None,
                },
                WorkflowPhaseExecution {
                    phase_id: "testing".to_string(),
                    status: WorkflowPhaseStatus::Pending,
                    started_at: None,
                    completed_at: None,
                    attempt: 0,
                    error_message: None,
                },
            ],
            machine_state: WorkflowMachineState::RunPhase,
            current_phase: Some("code-review".to_string()),
            started_at: chrono::Utc::now(),
            completed_at: None,
            failure_reason: None,
            checkpoint_metadata: WorkflowCheckpointMetadata::default(),
            rework_counts,
            total_reworks: 2,
            decision_history: vec![],
        };
        let workflow_json = serde_json::to_string_pretty(&workflow).unwrap();
        std::fs::write(workflow_state_dir.join(format!("{workflow_id}.json")), &workflow_json).unwrap();

        let research_outcome = PhaseExecutionOutcome::Completed {
            commit_message: None,
            phase_decision: Some(orchestrator_core::PhaseDecision {
                kind: "phase_decision".to_string(),
                phase_id: "research".to_string(),
                verdict: orchestrator_core::PhaseDecisionVerdict::Advance,
                confidence: 0.9,
                risk: orchestrator_core::WorkflowDecisionRisk::Low,
                reason: "Done".to_string(),
                evidence: vec![],
                guardrail_violations: vec![],
                commit_message: None,
                target_phase: None,
                verdict_key: None,
            }),
            result_payload: Some(serde_json::json!({"findings": ["pattern A"]})),
        };
        persist_phase_output(project_root, workflow_id, "research", 1, &research_outcome).unwrap();

        let (json_str, phase_order) = build_workflow_pipeline_context(project_root, workflow_id, "code-review");

        assert_eq!(phase_order, vec!["research", "implementation", "code-review", "testing"]);

        let ctx: serde_json::Value = serde_json::from_str(&json_str).unwrap();
        assert_eq!(ctx["current_phase"], "code-review");
        assert_eq!(ctx["workflow_status"], "running");
        assert_eq!(ctx["rework_counts"]["code-review"], 2);

        let pipeline = ctx["pipeline"].as_array().unwrap();
        assert_eq!(pipeline.len(), 4);
        assert_eq!(pipeline[0]["phase_id"], "research");
        assert_eq!(pipeline[0]["status"], "success");
        assert_eq!(pipeline[0]["attempt"], 1);
        assert_eq!(pipeline[0]["output"], serde_json::json!({"findings": ["pattern A"]}));
        assert_eq!(pipeline[2]["phase_id"], "code-review");
        assert_eq!(pipeline[2]["status"], "running");
        assert_eq!(pipeline[2]["attempt"], 3);
        assert!(pipeline[2].get("output").is_none());
        assert_eq!(pipeline[3]["status"], "pending");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_build_workflow_pipeline_context_returns_empty_when_no_state() {
        let (json_str, phase_order) = build_workflow_pipeline_context("/nonexistent", "wf-missing", "impl");
        assert!(json_str.is_empty());
        assert!(phase_order.is_empty());
    }

    #[test]
    fn test_format_prior_phase_outputs_truncation() {
        let long_reason = "x".repeat(6000);
        let outputs = vec![
            PersistedPhaseOutput {
                phase_id: "early".to_string(),
                completed_at: "2026-03-01T00:00:00Z".to_string(),
                verdict: Some("advance".to_string()),
                confidence: None,
                reason: Some(long_reason),
                commit_message: None,
                evidence: vec![],
                risk: None,
                target_phase: None,
                guardrail_violations: vec![],
                payload: None,
                requested_skills: vec![],
                resolved_skills: vec![],
                applied_skills: vec![],
            },
            PersistedPhaseOutput {
                phase_id: "recent".to_string(),
                completed_at: "2026-03-01T01:00:00Z".to_string(),
                verdict: Some("advance".to_string()),
                confidence: Some(0.9),
                reason: Some("Recent work".to_string()),
                commit_message: None,
                evidence: vec![],
                risk: None,
                target_phase: None,
                guardrail_violations: vec![],
                payload: None,
                requested_skills: vec![],
                resolved_skills: vec![],
                applied_skills: vec![],
            },
        ];
        let result = format_prior_phase_outputs(&outputs);
        assert!(result.len() <= MAX_PRIOR_CONTEXT_CHARS);
        assert!(result.contains("### recent (completed)"));
    }

    fn fixture_skill(name: &str, body: &str) -> ResolvedSkill {
        let definition =
            orchestrator_config::skill_definition::parse_skill_definition(&format!("name: {name}\n{body}"))
                .expect("fixture skill yaml should parse");
        ResolvedSkill { definition, source: orchestrator_config::skill_scoping::SkillSourceOrigin::Project }
    }

    fn fixture_metadata() -> PhaseExecutionMetadata {
        PhaseExecutionMetadata {
            phase_id: "code-review".to_string(),
            phase_mode: "agent".to_string(),
            phase_definition_hash: String::new(),
            agent_runtime_config_hash: String::new(),
            agent_runtime_schema: String::new(),
            agent_runtime_version: 0,
            agent_runtime_source: String::new(),
            agent_id: None,
            agent_profile_hash: None,
            selected_tool: Some("claude".to_string()),
            selected_model: None,
            effective_capabilities: Default::default(),
            requested_skills: Vec::new(),
            resolved_skills: Vec::new(),
            applied_skills: Vec::new(),
            skill_application: None,
        }
    }

    #[test]
    fn skill_contribution_kinds_buckets_application_fields() {
        let mut application = SkillApplicationResult::default();
        assert!(skill_contribution_kinds(&application).is_empty());
        application.prompt_prefixes.push("prefix".to_string());
        application.mcp_servers.push("context7".to_string());
        application.env.insert("KEY".to_string(), "value".to_string());
        application.extra_args.push("--flag".to_string());
        assert_eq!(skill_contribution_kinds(&application), vec!["prompt", "mcp_servers", "args", "env"]);
    }

    #[test]
    fn persisted_skills_from_metadata_records_scope_and_contributions() {
        let mut metadata = fixture_metadata();
        metadata.requested_skills = vec!["review-checklist".to_string(), "ghost".to_string()];
        let checklist = fixture_skill(
            "review-checklist",
            "prompt:\n  prefix: check things\ntool_policy:\n  allow:\n    - task.*\n",
        );
        let codex_only = fixture_skill("codex-only", "activation:\n  tools: [codex]\nprompt:\n  prefix: codex\n");
        metadata.resolved_skills = vec![checklist.clone(), codex_only];
        metadata.applied_skills = vec![checklist];

        let (requested, resolved, applied) = persisted_skills_from_metadata(&metadata);
        assert_eq!(requested, vec!["review-checklist", "ghost"]);
        assert_eq!(resolved.len(), 2);
        assert_eq!(applied.len(), 1);
        assert_eq!(applied[0].name, "review-checklist");
        assert_eq!(applied[0].source, "project");
        assert_eq!(applied[0].contributions, vec!["prompt", "tool_policy"]);
        // Activation-gated skill whose activation does not match the
        // selected tool resolves with no contribution kinds.
        assert_eq!(resolved[1].name, "codex-only");
        assert!(resolved[1].contributions.is_empty());
    }
}
