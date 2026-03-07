use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use uuid::Uuid;

use super::phase_executor::PhaseExecutionOutcome;

const MAX_PRIOR_CONTEXT_CHARS: usize = 8000;

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
    pub commit_message: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<orchestrator_core::PhaseEvidence>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub guardrail_violations: Vec<String>,
}

pub fn phase_output_dir(project_root: &str, workflow_id: &str) -> PathBuf {
    Path::new(project_root)
        .join(".ao")
        .join("state")
        .join("workflows")
        .join(workflow_id)
        .join("phase-outputs")
}

pub fn persist_phase_output(
    project_root: &str,
    workflow_id: &str,
    phase_id: &str,
    outcome: &PhaseExecutionOutcome,
) -> anyhow::Result<()> {
    let dir = phase_output_dir(project_root, workflow_id);
    std::fs::create_dir_all(&dir)?;

    let (verdict, confidence, reason, commit_message, evidence, guardrail_violations) =
        match outcome {
            PhaseExecutionOutcome::Completed {
                commit_message,
                phase_decision,
            } => {
                let (v, c, r, ev, gv) = match phase_decision {
                    Some(decision) => (
                        Some(format!("{:?}", decision.verdict).to_ascii_lowercase()),
                        Some(decision.confidence),
                        if decision.reason.is_empty() {
                            None
                        } else {
                            Some(decision.reason.clone())
                        },
                        decision.evidence.clone(),
                        decision.guardrail_violations.clone(),
                    ),
                    None => (
                        Some("advance".to_string()),
                        None,
                        None,
                        Vec::new(),
                        Vec::new(),
                    ),
                };
                (v, c, r, commit_message.clone(), ev, gv)
            }
            PhaseExecutionOutcome::NeedsResearch { reason } => (
                Some("rework".to_string()),
                None,
                Some(format!("Needs research: {reason}")),
                None,
                Vec::new(),
                Vec::new(),
            ),
            PhaseExecutionOutcome::ManualPending { instructions, .. } => (
                Some("manual_pending".to_string()),
                None,
                Some(instructions.clone()),
                None,
                Vec::new(),
                Vec::new(),
            ),
        };

    let output = PersistedPhaseOutput {
        phase_id: phase_id.to_string(),
        completed_at: chrono::Utc::now().to_rfc3339(),
        verdict,
        confidence,
        reason,
        commit_message,
        evidence,
        guardrail_violations,
    };

    let payload = serde_json::to_string_pretty(&output)?;
    let file_path = dir.join(format!("{phase_id}.json"));
    let tmp_path = file_path.with_file_name(format!("{phase_id}.{}.tmp", Uuid::new_v4()));
    std::fs::write(&tmp_path, &payload)?;
    std::fs::rename(&tmp_path, &file_path)?;
    Ok(())
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
                truncated.insert_str(
                    "## Prior Phase Results\n".len(),
                    "(earlier phases truncated for brevity)\n\n",
                );
                break;
            }
        }
        return truncated.trim_end().to_string();
    }

    result
}

pub(super) fn pipeline_phase_order_for_workflow(project_root: &str, workflow_id: &str) -> Vec<String> {
    let workflow_path = Path::new(project_root)
        .join(".ao")
        .join("workflow-state")
        .join(format!("{workflow_id}.json"));
    let contents = match std::fs::read_to_string(&workflow_path) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    let workflow: orchestrator_core::OrchestratorWorkflow = match serde_json::from_str(&contents) {
        Ok(w) => w,
        Err(_) => return Vec::new(),
    };
    workflow
        .phases
        .iter()
        .map(|phase| phase.phase_id.clone())
        .collect()
}

pub(super) fn format_output_chunk_for_display(text: &str, _verbose: bool, use_colors: bool, tool: &str) -> Option<String> {
    let trimmed = text.trim_start();
    if !trimmed.starts_with('{') {
        return Some(text.to_string());
    }

    if let Ok(val) = serde_json::from_str::<serde_json::Value>(trimmed) {
        if let Some(obj) = val.as_object() {
            let event_type = obj.get("type").and_then(|v| v.as_str()).unwrap_or("");
            if event_type == "tool_error" {
                let msg = obj
                    .get("error")
                    .and_then(|v| v.as_str())
                    .or_else(|| obj.get("message").and_then(|v| v.as_str()))
                    .unwrap_or("unknown error");
                let (red, reset) = if use_colors {
                    ("\x1b[31m", "\x1b[0m")
                } else {
                    ("", "")
                };
                return Some(format!("{red}  error: {msg}{reset}\n"));
            }
        }
    }

    match extract_display_text(text, tool) {
        Some(t) => {
            let mut out = if use_colors {
                termimad::text(&t).to_string()
            } else {
                t
            };
            if !out.ends_with('\n') {
                out.push('\n');
            }
            Some(out)
        }
        None => None,
    }
}

fn extract_display_text(line: &str, tool: &str) -> Option<String> {
    let trimmed = line.trim();
    let obj: serde_json::Value = serde_json::from_str(trimmed).ok()?;
    let tool_lower = tool.to_ascii_lowercase();
    match tool_lower.as_str() {
        t if t.contains("claude") => extract_claude_text(&obj),
        t if t.contains("codex") => extract_codex_text(&obj),
        t if t.contains("gemini") => extract_gemini_text(&obj),
        t if t.contains("oai-runner") || t.contains("oai_runner") => extract_oai_runner_text(&obj),
        t if t.contains("opencode") => extract_opencode_text(&obj),
        _ => extract_generic_text(&obj),
    }
}

fn extract_claude_text(obj: &serde_json::Value) -> Option<String> {
    let event_type = obj.get("type").and_then(|v| v.as_str()).unwrap_or("");
    match event_type {
        "content_block_delta" => obj.pointer("/delta/text").and_then(|v| v.as_str()).map(str::to_string),
        "result" => obj.get("result").and_then(|v| v.as_str()).map(str::to_string)
            .or_else(|| obj.pointer("/result/text").and_then(|v| v.as_str()).map(str::to_string)),
        "assistant" => {
            let content = obj.pointer("/message/content").and_then(|v| v.as_array())?;
            let text: String = content.iter()
                .filter(|b| b.get("type").and_then(|v| v.as_str()) == Some("text"))
                .filter_map(|b| b.get("text").and_then(|v| v.as_str()))
                .collect();
            (!text.is_empty()).then_some(text)
        }
        "content_block_start" => obj.pointer("/content_block/text")
            .and_then(|v| v.as_str()).filter(|t| !t.is_empty()).map(str::to_string),
        _ => obj.get("content").and_then(|v| v.as_str()).map(str::to_string),
    }
}

fn extract_codex_text(obj: &serde_json::Value) -> Option<String> {
    let event_type = obj.get("type").and_then(|v| v.as_str()).unwrap_or("");
    if !matches!(event_type, "item.completed" | "item.started" | "") { return None; }
    let item = obj.get("item")?;
    let item_type = item.get("type").and_then(|v| v.as_str()).unwrap_or("");
    if !matches!(item_type, "agent_message" | "message" | "") { return None; }
    if let Some(text) = item.get("text").and_then(|v| v.as_str()).filter(|t| !t.is_empty()) {
        return Some(text.to_string());
    }
    let content = item.get("content").and_then(|v| v.as_array())?;
    let text: String = content.iter()
        .filter(|b| matches!(b.get("type").and_then(|v| v.as_str()).unwrap_or(""), "output_text" | "text" | ""))
        .filter_map(|b| b.get("text").and_then(|v| v.as_str()))
        .collect();
    (!text.is_empty()).then_some(text)
}

fn extract_gemini_text(obj: &serde_json::Value) -> Option<String> {
    let event_type = obj.get("type").and_then(|v| v.as_str()).unwrap_or("");
    if event_type == "partialResult" {
        if let Some(t) = obj.pointer("/partialResult/text").and_then(|v| v.as_str()) {
            return Some(t.to_string());
        }
    }
    if let Some(t) = obj.get("text").and_then(|v| v.as_str()) { return Some(t.to_string()); }
    if let Some(t) = obj.get("response").and_then(|v| v.as_str()) { return Some(t.to_string()); }
    if let Some(t) = obj.pointer("/content/text").and_then(|v| v.as_str()) { return Some(t.to_string()); }
    let parts = obj.pointer("/content/parts").and_then(|v| v.as_array())
        .or_else(|| obj.get("candidates").and_then(|v| v.as_array())
            .and_then(|c| c.first()).and_then(|c| c.pointer("/content/parts")).and_then(|v| v.as_array()));
    let text: String = parts?.iter().filter_map(|p| p.get("text").and_then(|v| v.as_str())).collect();
    (!text.is_empty()).then_some(text)
}

fn extract_oai_runner_text(obj: &serde_json::Value) -> Option<String> {
    match obj.get("type").and_then(|v| v.as_str()).unwrap_or("") {
        "text_chunk" | "result" => obj.get("text").and_then(|v| v.as_str()).map(str::to_string),
        _ => None,
    }
}

fn extract_opencode_text(obj: &serde_json::Value) -> Option<String> {
    if obj.get("type").and_then(|v| v.as_str()) == Some("text") {
        return obj.get("text").and_then(|v| v.as_str()).map(str::to_string);
    }
    obj.get("content").and_then(|v| v.as_str()).map(str::to_string)
}

fn extract_generic_text(obj: &serde_json::Value) -> Option<String> {
    obj.get("text").and_then(|v| v.as_str()).map(str::to_string)
        .or_else(|| obj.get("content").and_then(|v| v.as_str()).map(str::to_string))
}


pub(super) fn format_tool_call_for_display(
    tool_name: &str,
    parameters: &serde_json::Value,
    use_colors: bool,
) -> String {
    let (cyan, dim, reset) = if use_colors {
        ("\x1b[36m", "\x1b[2m", "\x1b[0m")
    } else {
        ("", "", "")
    };
    let detail = match tool_name {
        "Read" | "Write" | "Edit" => parameters
            .get("file_path")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        "Bash" => {
            let cmd = parameters
                .get("command")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if cmd.len() > 60 {
                format!("{}...", &cmd[..60])
            } else {
                cmd.to_string()
            }
        }
        "Grep" | "Glob" => parameters
            .get("pattern")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        name if name.starts_with("mcp__") => {
            let compact = parameters.to_string();
            if compact.len() > 80 {
                format!("{}...", &compact[..80])
            } else {
                compact
            }
        }
        _ => {
            let compact = parameters.to_string();
            if compact.len() > 80 {
                format!("{}...", &compact[..80])
            } else {
                compact
            }
        }
    };
    if detail.is_empty() {
        format!("{cyan}  → {tool_name}{reset}\n")
    } else {
        format!("{cyan}  → {tool_name}{reset} {dim}{detail}{reset}\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_persist_and_load_phase_output() {
        let tmp = std::env::temp_dir().join(format!("ao-test-phase-output-{}", Uuid::new_v4()));
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
            }),
        };

        persist_phase_output(project_root, workflow_id, "research", &outcome).unwrap();

        let output_file = phase_output_dir(project_root, workflow_id).join("research.json");
        assert!(output_file.exists());

        let loaded: PersistedPhaseOutput =
            serde_json::from_str(&std::fs::read_to_string(&output_file).unwrap()).unwrap();
        assert_eq!(loaded.phase_id, "research");
        assert_eq!(loaded.verdict.as_deref(), Some("advance"));
        assert!((loaded.confidence.unwrap() - 0.9).abs() < f32::EPSILON);
        assert_eq!(
            loaded.reason.as_deref(),
            Some("Research complete, found relevant patterns")
        );
        assert_eq!(
            loaded.commit_message.as_deref(),
            Some("feat: add login flow")
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_load_prior_phase_outputs_ordering() {
        let tmp = std::env::temp_dir().join(format!(
            "ao-test-phase-output-order-{}",
            Uuid::new_v4()
        ));
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
            }),
        };
        persist_phase_output(project_root, workflow_id, "research", &research_outcome).unwrap();

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
            }),
        };
        persist_phase_output(project_root, workflow_id, "implementation", &impl_outcome).unwrap();

        let pipeline_order = vec![
            "research".to_string(),
            "implementation".to_string(),
            "review".to_string(),
        ];

        let loaded = load_prior_phase_outputs(project_root, workflow_id, "review", &pipeline_order);
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].phase_id, "research");
        assert_eq!(loaded[1].phase_id, "implementation");

        let loaded_impl =
            load_prior_phase_outputs(project_root, workflow_id, "implementation", &pipeline_order);
        assert_eq!(loaded_impl.len(), 1);
        assert_eq!(loaded_impl[0].phase_id, "research");

        let loaded_research =
            load_prior_phase_outputs(project_root, workflow_id, "research", &pipeline_order);
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
                guardrail_violations: vec![],
            },
            PersistedPhaseOutput {
                phase_id: "implementation".to_string(),
                completed_at: "2026-03-01T01:00:00Z".to_string(),
                verdict: Some("advance".to_string()),
                confidence: Some(0.95),
                reason: Some("Implemented".to_string()),
                commit_message: Some("feat: add feature".to_string()),
                evidence: vec![],
                guardrail_violations: vec![],
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
                guardrail_violations: vec![],
            },
            PersistedPhaseOutput {
                phase_id: "recent".to_string(),
                completed_at: "2026-03-01T01:00:00Z".to_string(),
                verdict: Some("advance".to_string()),
                confidence: Some(0.9),
                reason: Some("Recent work".to_string()),
                commit_message: None,
                evidence: vec![],
                guardrail_violations: vec![],
            },
        ];
        let result = format_prior_phase_outputs(&outputs);
        assert!(result.len() <= MAX_PRIOR_CONTEXT_CHARS);
        assert!(result.contains("### recent (completed)"));
    }

    #[test]
    fn test_persist_needs_research_outcome() {
        let tmp = std::env::temp_dir().join(format!(
            "ao-test-phase-output-research-{}",
            Uuid::new_v4()
        ));
        let project_root = tmp.to_str().unwrap();
        let workflow_id = "wf-test-003";

        let outcome = PhaseExecutionOutcome::NeedsResearch {
            reason: "Need API docs".to_string(),
        };
        persist_phase_output(project_root, workflow_id, "implementation", &outcome).unwrap();

        let output_file =
            phase_output_dir(project_root, workflow_id).join("implementation.json");
        let loaded: PersistedPhaseOutput =
            serde_json::from_str(&std::fs::read_to_string(&output_file).unwrap()).unwrap();
        assert_eq!(loaded.verdict.as_deref(), Some("rework"));
        assert!(loaded.reason.as_deref().unwrap().contains("Need API docs"));

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
