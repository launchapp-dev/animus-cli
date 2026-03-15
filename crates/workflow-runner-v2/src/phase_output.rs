use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use uuid::Uuid;

use crate::phase_executor::PhaseExecutionOutcome;

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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<serde_json::Value>,
}

fn scoped_state_base(project_root: &str) -> PathBuf {
    let path = Path::new(project_root);
    protocol::scoped_state_root(path).unwrap_or_else(|| path.join(".ao"))
}

pub fn phase_output_dir(project_root: &str, workflow_id: &str) -> PathBuf {
    scoped_state_base(project_root).join("state").join("workflows").join(workflow_id).join("phase-outputs")
}

pub fn persist_phase_output(
    project_root: &str,
    workflow_id: &str,
    phase_id: &str,
    outcome: &PhaseExecutionOutcome,
) -> anyhow::Result<()> {
    let dir = phase_output_dir(project_root, workflow_id);
    std::fs::create_dir_all(&dir)?;

    let (verdict, confidence, reason, commit_message, evidence, guardrail_violations, payload) = match outcome {
        PhaseExecutionOutcome::Completed { commit_message, phase_decision, result_payload } => {
            let (v, c, r, ev, gv) = match phase_decision {
                Some(decision) => (
                    Some(format!("{:?}", decision.verdict).to_ascii_lowercase()),
                    Some(decision.confidence),
                    if decision.reason.is_empty() { None } else { Some(decision.reason.clone()) },
                    decision.evidence.clone(),
                    decision.guardrail_violations.clone(),
                ),
                None => (Some("advance".to_string()), None, None, Vec::new(), Vec::new()),
            };
            (v, c, r, commit_message.clone(), ev, gv, result_payload.clone())
        }
        PhaseExecutionOutcome::ManualPending { instructions, .. } => {
            (Some("manual_pending".to_string()), None, Some(instructions.clone()), None, Vec::new(), Vec::new(), None)
        }
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
        payload,
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
            result_payload: None,
        };

        persist_phase_output(project_root, workflow_id, "research", &outcome).unwrap();

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

    #[test]
    fn test_load_prior_phase_outputs_ordering() {
        let tmp = std::env::temp_dir().join(format!("ao-test-phase-output-order-{}", Uuid::new_v4()));
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
            result_payload: None,
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
            result_payload: None,
        };
        persist_phase_output(project_root, workflow_id, "implementation", &impl_outcome).unwrap();

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
                guardrail_violations: vec![],
                payload: None,
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
                payload: None,
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
    fn test_build_workflow_pipeline_context_returns_structured_json() {
        use protocol::orchestrator::{
            SubjectRef, WorkflowCheckpointMetadata, WorkflowMachineState, WorkflowPhaseExecution, WorkflowPhaseStatus,
            WorkflowStatus,
        };

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
            subject: SubjectRef::task("TASK-1".to_string()),
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
            }),
            result_payload: Some(serde_json::json!({"findings": ["pattern A"]})),
        };
        persist_phase_output(project_root, workflow_id, "research", &research_outcome).unwrap();

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
                guardrail_violations: vec![],
                payload: None,
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
                payload: None,
            },
        ];
        let result = format_prior_phase_outputs(&outputs);
        assert!(result.len() <= MAX_PRIOR_CONTEXT_CHARS);
        assert!(result.contains("### recent (completed)"));
    }
}
