use super::*;

use crate::domain_state::{load_handoffs, save_handoffs, HandoffRecord};
use crate::types::{AgentHandoffStatus, HandoffTargetRole};

use protocol::default_model_for_tool as protocol_default_model_for_tool;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::time::{Duration, Instant};
use tokio::process::Command as TokioCommand;
use tokio::time::timeout;
use uuid::Uuid;

const DEFAULT_HANDOFF_TIMEOUT_SECS: u64 = 60;
const DEFAULT_HANDOFF_MAX_DEPTH: usize = 5;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct HandoffLogEntry {
    pub timestamp: chrono::DateTime<chrono::Utc>,
    pub handoff_id: String,
    pub root_run_id: String,
    pub run_id: String,
    pub workflow_id: String,
    pub target_role: HandoffTargetRole,
    pub intent_hash: String,
    pub event: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<Value>,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct WorkflowPhaseRuntimeSettings {
    #[serde(default)]
    tool: Option<String>,
    #[serde(default)]
    model: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct WorkflowPipelineRuntimeRecord {
    #[allow(dead_code)]
    id: String,
    #[serde(default)]
    phase_settings: std::collections::HashMap<String, WorkflowPhaseRuntimeSettings>,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct WorkflowRuntimeConfigLite {
    #[serde(default)]
    default_pipeline_id: String,
    #[serde(default)]
    pipelines: Vec<WorkflowPipelineRuntimeRecord>,
}

fn handoff_timeout() -> Duration {
    let secs = std::env::var("AO_HANDOFF_TIMEOUT_SECS")
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_HANDOFF_TIMEOUT_SECS);
    Duration::from_secs(secs)
}

fn handoff_max_depth() -> usize {
    std::env::var("AO_HANDOFF_MAX_DEPTH")
        .ok()
        .and_then(|value| value.trim().parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_HANDOFF_MAX_DEPTH)
}

fn workflow_runtime_config_path(project_root: &Path) -> PathBuf {
    project_root
        .join(".ao")
        .join("state")
        .join("workflow-config.json")
}

fn load_workflow_runtime_config(project_root: &Path) -> WorkflowRuntimeConfigLite {
    let path = workflow_runtime_config_path(project_root);
    if !path.exists() {
        return WorkflowRuntimeConfigLite::default();
    }
    let Ok(content) = fs::read_to_string(path) else {
        return WorkflowRuntimeConfigLite::default();
    };
    serde_json::from_str::<WorkflowRuntimeConfigLite>(&content).unwrap_or_default()
}

fn resolve_handoff_settings_from_runtime_config(
    project_root: &Path,
    phase_key: &str,
) -> Option<WorkflowPhaseRuntimeSettings> {
    let config = load_workflow_runtime_config(project_root);
    let pipeline_id = config.default_pipeline_id.trim();
    let pipeline = if pipeline_id.is_empty() {
        config.pipelines.first()
    } else {
        config
            .pipelines
            .iter()
            .find(|pipeline| pipeline.id.eq_ignore_ascii_case(pipeline_id))
            .or_else(|| config.pipelines.first())
    }?;
    pipeline
        .phase_settings
        .iter()
        .find(|(phase_id, _)| phase_id.eq_ignore_ascii_case(phase_key))
        .map(|(_, settings)| settings.clone())
}

fn resolve_handoff_execution_target(
    project_root: &Path,
    role: HandoffTargetRole,
) -> (String, String) {
    let role_key = role.as_str().to_ascii_uppercase().replace('-', "_");
    let tool_env_key = format!("AO_HANDOFF_TOOL_{role_key}");
    let model_env_key = format!("AO_HANDOFF_MODEL_{role_key}");
    let phase_key = format!("handoff-{}", role.as_str());
    let runtime_settings = resolve_handoff_settings_from_runtime_config(project_root, &phase_key);

    let tool = std::env::var(&tool_env_key)
        .ok()
        .or_else(|| std::env::var("AO_HANDOFF_TOOL").ok())
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| !value.is_empty())
        .or_else(|| {
            runtime_settings
                .as_ref()
                .and_then(|settings| settings.tool.as_deref())
                .map(|value| value.trim().to_ascii_lowercase())
                .filter(|value| !value.is_empty())
        })
        .unwrap_or_else(|| "codex".to_string());

    let model = std::env::var(&model_env_key)
        .ok()
        .or_else(|| std::env::var("AO_HANDOFF_MODEL").ok())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .or_else(|| {
            runtime_settings
                .as_ref()
                .and_then(|settings| settings.model.as_deref())
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
        })
        .unwrap_or_else(|| {
            protocol_default_model_for_tool(&tool)
                .unwrap_or("gpt-5.3-codex")
                .to_string()
        });

    (tool, model)
}

fn resolve_workflow_id_for_run(project_root: &Path, run_id: &str) -> Option<String> {
    let manager = WorkflowStateManager::new(project_root);
    let workflows = manager.list().ok()?;
    workflows.into_iter().find_map(|workflow| {
        if workflow.id == run_id || workflow.task_id == run_id {
            Some(workflow.id)
        } else {
            None
        }
    })
}

fn transcript_path(project_root: &Path, workflow_id: &str, root_run_id: &str) -> PathBuf {
    project_root
        .join(".ao")
        .join("state")
        .join("agent-handoffs")
        .join(sanitize_segment(workflow_id))
        .join(format!("{}.jsonl", sanitize_segment(root_run_id)))
}

fn load_history(path: &Path) -> Result<Vec<HandoffLogEntry>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let file = OpenOptions::new().read(true).open(path)?;
    let reader = BufReader::new(file);
    let mut entries = Vec::new();
    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(entry) = serde_json::from_str::<HandoffLogEntry>(&line) {
            entries.push(entry);
        }
    }
    Ok(entries)
}

fn append_entry(path: &Path, entry: HandoffLogEntry) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    writeln!(file, "{}", serde_json::to_string(&entry)?)?;
    Ok(())
}

fn hash_intent(role: HandoffTargetRole, question: &str) -> String {
    let normalized = format!("{}:{}", role.as_str(), question.trim().to_ascii_lowercase());
    let mut hasher = Sha256::new();
    hasher.update(normalized.as_bytes());
    format!("{:x}", hasher.finalize())
}

fn sanitize_segment(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

fn record_handoff(
    project_root: &Path,
    run_id: &str,
    question: &str,
    context: &Value,
    result: &AgentHandoffResult,
) {
    let project_root_str = project_root.to_string_lossy().to_string();
    let mut store = match load_handoffs(&project_root_str) {
        Ok(store) => store,
        Err(_) => return,
    };

    store.handoffs.push(HandoffRecord {
        handoff_id: result.handoff_id.clone(),
        run_id: run_id.to_string(),
        target_role: result.target_role.as_str().to_string(),
        question: question.to_string(),
        context: context.clone(),
        status: match result.status {
            AgentHandoffStatus::Completed => "completed".to_string(),
            AgentHandoffStatus::Failed => "failed".to_string(),
        },
        response: result.response.clone(),
        error: result.error.clone(),
        created_at: Utc::now().to_rfc3339(),
        duration_ms: result.duration_ms,
    });
    let _ = save_handoffs(&project_root_str, &store);
}

async fn ask_target(
    project_root: &Path,
    target: HandoffTargetRole,
    question: &str,
    context: &Value,
) -> Result<String> {
    let (tool, model) = resolve_handoff_execution_target(project_root, target);
    let prompt = format!(
        "You are {} supporting another agent in Agent Orchestrator.\n\
Answer concisely and concretely. If uncertain, provide deterministic next steps.\n\
Question:\n{}\n\
Context (JSON):\n{}\n",
        target.as_str().to_ascii_uppercase(),
        question,
        serde_json::to_string_pretty(context).unwrap_or_else(|_| "{}".to_string())
    );

    let mut cmd = TokioCommand::new(&tool);
    match tool.as_str() {
        "claude" => {
            cmd.arg("--print").arg("--no-session-persistence");
            if !model.is_empty() {
                cmd.arg("--model").arg(&model);
            }
            cmd.arg(&prompt);
        }
        "codex" => {
            cmd.arg("exec").arg("--skip-git-repo-check");
            if !model.is_empty() {
                cmd.arg("--model").arg(&model);
            }
            cmd.arg(&prompt);
        }
        "gemini" => {
            if !model.is_empty() {
                cmd.arg("--model").arg(&model);
            }
            cmd.arg("-p").arg(&prompt);
        }
        "opencode" => {
            cmd.arg("run");
            if !model.is_empty() {
                cmd.arg("-m").arg(&model);
            }
            cmd.arg(&prompt);
        }
        _ => {
            cmd.arg(&prompt);
        }
    }
    cmd.current_dir(project_root);

    let output = timeout(handoff_timeout(), cmd.output())
        .await
        .map_err(|_| anyhow!("handoff timed out after {:?}", handoff_timeout()))?
        .with_context(|| format!("failed to launch handoff tool '{tool}'"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!(
            "handoff CLI '{}' exited with status {}: {}",
            tool,
            output.status,
            stderr.trim()
        ));
    }

    let response = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if response.is_empty() {
        Ok("No response received from target reviewer. Continue with deterministic fallback and capture unresolved questions in task notes.".to_string())
    } else {
        Ok(response)
    }
}

async fn request_handoff_impl(
    project_root: &Path,
    request: AgentHandoffRequestInput,
) -> AgentHandoffResult {
    let started_at = Instant::now();
    let handoff_id = request
        .handoff_id
        .clone()
        .unwrap_or_else(|| format!("handoff-{}", Uuid::new_v4()));
    let run_id = request.run_id.clone();
    let target_role = request.target_role;
    let question = request.question.clone();
    let context = request.context.clone();
    let intent_hash = hash_intent(target_role, &question);

    if run_id.trim().is_empty() {
        return AgentHandoffResult {
            handoff_id,
            run_id,
            root_run_id: String::new(),
            workflow_id: "unknown-workflow".to_string(),
            target_role,
            status: AgentHandoffStatus::Failed,
            response: None,
            error: Some("run_id is required".to_string()),
            duration_ms: started_at.elapsed().as_millis() as u64,
            depth: 0,
        };
    }

    let root_run_id = context
        .get("root_run_id")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(ToString::to_string)
        .unwrap_or_else(|| run_id.clone());
    let workflow_id = resolve_workflow_id_for_run(project_root, &run_id)
        .unwrap_or_else(|| "unknown-workflow".to_string());
    let transcript = transcript_path(project_root, &workflow_id, &root_run_id);

    let history = load_history(&transcript).unwrap_or_default();
    let depth = history
        .iter()
        .filter(|entry| entry.event == "completed" || entry.event == "failed")
        .count();

    if depth >= handoff_max_depth() {
        let error = format!(
            "Handoff depth exceeded (depth={}, max_depth={})",
            depth,
            handoff_max_depth()
        );
        let result = AgentHandoffResult {
            handoff_id: handoff_id.clone(),
            run_id: run_id.clone(),
            root_run_id: root_run_id.clone(),
            workflow_id: workflow_id.clone(),
            target_role,
            status: AgentHandoffStatus::Failed,
            response: None,
            error: Some(error.clone()),
            duration_ms: started_at.elapsed().as_millis() as u64,
            depth,
        };
        let _ = append_entry(
            &transcript,
            HandoffLogEntry {
                timestamp: Utc::now(),
                handoff_id,
                root_run_id,
                run_id,
                workflow_id,
                target_role,
                intent_hash,
                event: "failed".to_string(),
                payload: Some(json!({ "error": error })),
            },
        );
        record_handoff(
            project_root,
            &request.run_id,
            &request.question,
            &request.context,
            &result,
        );
        return result;
    }

    let mut seen_pairs: HashSet<(String, String)> = HashSet::new();
    for entry in &history {
        seen_pairs.insert((
            entry.target_role.as_str().to_string(),
            entry.intent_hash.clone(),
        ));
    }
    let loop_key = (target_role.as_str().to_string(), intent_hash.clone());
    if seen_pairs.contains(&loop_key) {
        let error = format!(
            "Handoff cycle detected for role={} intent_hash={}",
            loop_key.0, loop_key.1
        );
        let result = AgentHandoffResult {
            handoff_id: handoff_id.clone(),
            run_id: run_id.clone(),
            root_run_id: root_run_id.clone(),
            workflow_id: workflow_id.clone(),
            target_role,
            status: AgentHandoffStatus::Failed,
            response: None,
            error: Some(error.clone()),
            duration_ms: started_at.elapsed().as_millis() as u64,
            depth,
        };
        let _ = append_entry(
            &transcript,
            HandoffLogEntry {
                timestamp: Utc::now(),
                handoff_id,
                root_run_id,
                run_id,
                workflow_id,
                target_role,
                intent_hash,
                event: "failed".to_string(),
                payload: Some(json!({ "error": error })),
            },
        );
        record_handoff(
            project_root,
            &request.run_id,
            &request.question,
            &request.context,
            &result,
        );
        return result;
    }

    let _ = append_entry(
        &transcript,
        HandoffLogEntry {
            timestamp: Utc::now(),
            handoff_id: handoff_id.clone(),
            root_run_id: root_run_id.clone(),
            run_id: run_id.clone(),
            workflow_id: workflow_id.clone(),
            target_role,
            intent_hash: intent_hash.clone(),
            event: "started".to_string(),
            payload: Some(json!({
                "question": question,
                "context": context,
            })),
        },
    );

    let result = match ask_target(project_root, target_role, &question, &context).await {
        Ok(response) => {
            let _ = append_entry(
                &transcript,
                HandoffLogEntry {
                    timestamp: Utc::now(),
                    handoff_id: handoff_id.clone(),
                    root_run_id: root_run_id.clone(),
                    run_id: run_id.clone(),
                    workflow_id: workflow_id.clone(),
                    target_role,
                    intent_hash: intent_hash.clone(),
                    event: "completed".to_string(),
                    payload: Some(json!({ "response": response })),
                },
            );
            AgentHandoffResult {
                handoff_id,
                run_id,
                root_run_id,
                workflow_id,
                target_role,
                status: AgentHandoffStatus::Completed,
                response: Some(response),
                error: None,
                duration_ms: started_at.elapsed().as_millis() as u64,
                depth: depth + 1,
            }
        }
        Err(error) => {
            let message = error.to_string();
            let _ = append_entry(
                &transcript,
                HandoffLogEntry {
                    timestamp: Utc::now(),
                    handoff_id: handoff_id.clone(),
                    root_run_id: root_run_id.clone(),
                    run_id: run_id.clone(),
                    workflow_id: workflow_id.clone(),
                    target_role,
                    intent_hash,
                    event: "failed".to_string(),
                    payload: Some(json!({ "error": message })),
                },
            );
            AgentHandoffResult {
                handoff_id,
                run_id,
                root_run_id,
                workflow_id,
                target_role,
                status: AgentHandoffStatus::Failed,
                response: None,
                error: Some(error.to_string()),
                duration_ms: started_at.elapsed().as_millis() as u64,
                depth: depth + 1,
            }
        }
    };

    record_handoff(
        project_root,
        &request.run_id,
        &request.question,
        &request.context,
        &result,
    );
    result
}

#[async_trait]
impl ReviewServiceApi for InMemoryServiceHub {
    async fn request_handoff(&self, input: AgentHandoffRequestInput) -> Result<AgentHandoffResult> {
        let project_root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        Ok(request_handoff_impl(&project_root, input).await)
    }
}

#[async_trait]
impl ReviewServiceApi for FileServiceHub {
    async fn request_handoff(&self, input: AgentHandoffRequestInput) -> Result<AgentHandoffResult> {
        Ok(request_handoff_impl(&self.project_root, input).await)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn request_handoff_requires_run_id() {
        let temp = TempDir::new().expect("tempdir");
        let request = AgentHandoffRequestInput {
            handoff_id: None,
            run_id: String::new(),
            target_role: HandoffTargetRole::Em,
            question: "What should we do next?".to_string(),
            context: json!({}),
        };
        let result = request_handoff_impl(temp.path(), request).await;
        assert_eq!(result.status, AgentHandoffStatus::Failed);
        assert!(result
            .error
            .unwrap_or_default()
            .contains("run_id is required"));
        assert_eq!(result.depth, 0);
    }
}
