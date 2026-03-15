use crate::cli_types::{QaApprovalCommand, QaCommand};
use crate::{not_found_error, print_value};
use anyhow::Result;
use chrono::Utc;
use orchestrator_core::{
    load_qa_approvals, load_qa_results, save_qa_approvals, save_qa_results, QaGateResultRecord, QaPhaseGateResult,
    QaReviewApprovalRecord,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct QaGateInput {
    id: String,
    #[serde(default)]
    metric: Option<String>,
    #[serde(default)]
    min_value: Option<f64>,
    #[serde(default)]
    required: bool,
}

pub(crate) async fn handle_qa(command: QaCommand, project_root: &str, json: bool) -> Result<()> {
    match command {
        QaCommand::Evaluate(args) => {
            let gates = if let Some(gates_json) = args.gates_json {
                serde_json::from_str::<Vec<QaGateInput>>(&gates_json)?
            } else {
                vec![
                    QaGateInput {
                        id: "lint".to_string(),
                        metric: Some("lint_passed".to_string()),
                        min_value: Some(1.0),
                        required: true,
                    },
                    QaGateInput {
                        id: "tests".to_string(),
                        metric: Some("tests_passed".to_string()),
                        min_value: Some(1.0),
                        required: true,
                    },
                ]
            };

            let metrics = args
                .metrics_json
                .as_deref()
                .map(serde_json::from_str::<BTreeMap<String, Value>>)
                .transpose()?
                .unwrap_or_default();
            let metadata = args
                .metadata_json
                .as_deref()
                .map(serde_json::from_str::<BTreeMap<String, Value>>)
                .transpose()?
                .unwrap_or_default();

            let mut gate_results = Vec::new();
            for gate in gates {
                let passed = if let Some(metric_name) = gate.metric.as_deref() {
                    let metric_value = metrics
                        .get(metric_name)
                        .and_then(|value| value.as_f64())
                        .or_else(|| {
                            metrics.get(metric_name).and_then(|value| value.as_bool()).map(|value| {
                                if value {
                                    1.0
                                } else {
                                    0.0
                                }
                            })
                        })
                        .unwrap_or(0.0);
                    metric_value >= gate.min_value.unwrap_or(1.0)
                } else {
                    true
                };
                gate_results.push(QaGateResultRecord {
                    gate_id: gate.id,
                    passed,
                    reason: if passed { "gate passed".to_string() } else { "gate failed".to_string() },
                    gate_type: None,
                    metric: gate.metric,
                    actual_value: None,
                    threshold: gate.min_value.map(Value::from),
                    blocking: Some(gate.required),
                    evaluated_at: None,
                    confidence_score: None,
                });
            }

            let passed = gate_results.iter().all(|result| result.passed);
            let mut store = load_qa_results(project_root)?;
            store
                .results
                .retain(|result| !(result.workflow_id == args.workflow_id && result.phase_id == args.phase_id));
            let result = QaPhaseGateResult {
                workflow_id: args.workflow_id,
                phase_id: args.phase_id,
                task_id: args.task_id,
                worktree_path: args.worktree_path.unwrap_or_default(),
                passed,
                gate_results,
                metrics,
                metadata,
                evaluated_at: Utc::now().to_rfc3339(),
            };
            store.results.push(result.clone());
            save_qa_results(project_root, &store)?;
            print_value(result, json)
        }
        QaCommand::Get(args) => {
            let store = load_qa_results(project_root)?;
            let result = store
                .results
                .into_iter()
                .find(|result| result.workflow_id == args.workflow_id && result.phase_id == args.phase_id)
                .ok_or_else(|| not_found_error("phase gate results not found"))?;
            print_value(result, json)
        }
        QaCommand::List(args) => {
            let store = load_qa_results(project_root)?;
            let results: Vec<_> =
                store.results.into_iter().filter(|result| result.workflow_id == args.workflow_id).collect();
            print_value(results, json)
        }
        QaCommand::Approval { command } => match command {
            QaApprovalCommand::Add(args) => {
                let mut store = load_qa_approvals(project_root)?;
                store.approvals.push(QaReviewApprovalRecord {
                    workflow_id: args.workflow_id,
                    phase_id: args.phase_id,
                    gate_id: args.gate_id,
                    approved_by: args.approved_by,
                    approved_at: Utc::now().to_rfc3339(),
                    comment: args.comment,
                });
                save_qa_approvals(project_root, &store)?;
                print_value(store.approvals, json)
            }
            QaApprovalCommand::List(args) => {
                let store = load_qa_approvals(project_root)?;
                let approvals: Vec<_> = store
                    .approvals
                    .into_iter()
                    .filter(|approval| approval.workflow_id == args.workflow_id && approval.gate_id == args.gate_id)
                    .collect();
                print_value(approvals, json)
            }
        },
    }
}
