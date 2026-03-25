use std::process::ExitCode;

use clap::{Args, Parser, Subcommand};
use orchestrator_core::WorkflowStatus;
use orchestrator_logging::{init_workflow_tracing, Logger};
use serde::Serialize;

use workflow_runner_v2::workflow_execute::{execute_workflow, PhaseEvent, WorkflowExecuteParams};

#[derive(Parser)]
#[command(name = "ao-workflow-runner", about = "Standalone workflow phase runner")]
struct WorkflowRunnerCli {
    #[command(subcommand)]
    command: WorkflowRunnerCommand,
}

#[derive(Subcommand)]
enum WorkflowRunnerCommand {
    Execute(WorkflowExecuteArgs),
}

#[derive(Args)]
struct WorkflowExecuteArgs {
    #[arg(long)]
    workflow_id: Option<String>,

    #[arg(long)]
    task_id: Option<String>,

    #[arg(long)]
    requirement_id: Option<String>,

    #[arg(long)]
    title: Option<String>,

    #[arg(long)]
    description: Option<String>,

    #[arg(long)]
    workflow_ref: Option<String>,

    #[arg(long)]
    input_json: Option<String>,

    #[arg(long)]
    project_root: String,

    #[arg(long)]
    config_path: Option<String>,

    #[arg(long)]
    model: Option<String>,

    #[arg(long)]
    tool: Option<String>,

    #[arg(long)]
    phase_timeout_secs: Option<u64>,

    #[arg(long)]
    phase_routing_json: Option<String>,

    #[arg(long)]
    mcp_config_json: Option<String>,
}

#[derive(Debug, Serialize)]
struct RunnerEvent {
    event: &'static str,
    task_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    workflow_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    workflow_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    workflow_status: Option<WorkflowStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    exit_code: Option<i32>,
}

#[tokio::main]
async fn main() -> ExitCode {
    init_tracing();
    let cli = WorkflowRunnerCli::parse();

    match cli.command {
        WorkflowRunnerCommand::Execute(args) => match run_execute(args).await {
            Ok(code) => ExitCode::from(code),
            Err(error) => {
                eprintln!("ao-workflow-runner failed: {error}");
                ExitCode::from(1)
            }
        },
    }
}

fn init_tracing() {
    init_workflow_tracing();
}

async fn run_execute(args: WorkflowExecuteArgs) -> anyhow::Result<u8> {
    let subject_id = args
        .workflow_id
        .as_deref()
        .or(args.task_id.as_deref())
        .or(args.requirement_id.as_deref())
        .or(args.title.as_deref())
        .unwrap_or("unknown")
        .to_string();

    let startup = RunnerEvent {
        event: "runner_start",
        task_id: subject_id.clone(),
        workflow_id: args.workflow_id.clone(),
        workflow_ref: args.workflow_ref.clone(),
        workflow_status: None,
        exit_code: None,
    };
    eprintln!("{}", serde_json::to_string(&startup).unwrap_or_default());

    let phase_routing = args.phase_routing_json.as_deref().and_then(|json| serde_json::from_str(json).ok());
    let mcp_config = args.mcp_config_json.as_deref().and_then(|json| serde_json::from_str(json).ok());
    let log_project_root = args.project_root.clone();
    let log_workflow_ref = args.workflow_ref.clone().unwrap_or_default();
    let wf_log_root = log_project_root.clone();
    let wf_log_ref = log_workflow_ref.clone();

    let params = WorkflowExecuteParams {
        project_root: args.project_root,
        workflow_id: args.workflow_id,
        task_id: args.task_id,
        requirement_id: args.requirement_id,
        title: args.title,
        description: args.description,
        workflow_ref: args.workflow_ref.clone(),
        input: args.input_json.as_deref().map(serde_json::from_str).transpose()?,
        vars: std::collections::HashMap::new(),
        model: args.model,
        tool: args.tool,
        phase_timeout_secs: args.phase_timeout_secs,
        phase_filter: None,
        on_phase_event: {
            let log_root = log_project_root;
            let log_wf_ref = log_workflow_ref;
            Some(Box::new(move |event| {
                let logger = Logger::for_project(std::path::Path::new(&log_root));
                match event {
                    PhaseEvent::Started { phase_id, phase_index, total_phases } => {
                        logger
                            .info("phase.start", format!("{phase_id} ({}/{total_phases})", phase_index + 1))
                            .phase(phase_id)
                            .meta(serde_json::json!({ "workflow_ref": log_wf_ref }))
                            .emit();
                    }
                    PhaseEvent::Completed { phase_id, duration, success, error, model, tool } => {
                        let mut b = if success {
                            logger.info("phase.complete", format!("{phase_id} {}ms", duration.as_millis()))
                        } else {
                            logger.error("phase.complete", format!("{phase_id} failed {}ms", duration.as_millis()))
                        };
                        b = b.phase(phase_id).duration(duration.as_millis() as u64);
                        if let Some(ref e) = error {
                            b = b.err(e);
                        }
                        if let Some(ref m) = model {
                            if let Some(ref t) = tool {
                                b = b.model_tool(m, t);
                            }
                        }
                        b.emit();
                    }
                    PhaseEvent::Decision { phase_id, decision } => {
                        logger.info("phase.decision", format!("{phase_id}: {:?}", decision.verdict))
                            .phase(phase_id)
                            .meta(serde_json::json!({ "verdict": format!("{:?}", decision.verdict), "reason": decision.reason }))
                            .emit();
                    }
                }
            }))
        },
        hub: None,
        phase_routing,
        mcp_config,
    };

    {
        let wf_logger = Logger::for_project(std::path::Path::new(&wf_log_root));
        wf_logger
            .info("workflow.start", format!("started {}", wf_log_ref))
            .subject(subject_id.as_str())
            .meta(serde_json::json!({"workflow_ref": wf_log_ref}))
            .emit();
    }

    let wf_start = std::time::Instant::now();
    let result = execute_workflow(params).await;
    let wf_duration = wf_start.elapsed();

    {
        let wf_logger = Logger::for_project(std::path::Path::new(&wf_log_root));
        let success = matches!(&result, Ok(r) if r.success);
        let mut b = if success {
            wf_logger.info("workflow.complete", format!("{} completed", wf_log_ref))
        } else {
            wf_logger.error("workflow.complete", format!("{} failed", wf_log_ref))
        };
        b = b
            .subject(subject_id.as_str())
            .duration(wf_duration.as_millis() as u64)
            .meta(serde_json::json!({"workflow_ref": wf_log_ref}));
        if let Err(ref e) = result {
            b = b.err(e.to_string());
        } else if let Ok(ref r) = result {
            b = b.meta(serde_json::json!({
                "workflow_ref": wf_log_ref,
                "phases_completed": r.phases_completed,
                "phases_total": r.phases_total,
            }));
        }
        b.emit();
    }

    let exit_code: i32 = match &result {
        Ok(r) if r.success => 0,
        Ok(_) => 1,
        Err(_) => 1,
    };

    let workflow_ref = match &result {
        Ok(value) => {
            if value.workflow_ref.trim().is_empty() {
                args.workflow_ref.clone()
            } else {
                Some(value.workflow_ref.clone())
            }
        }
        Err(_) => args.workflow_ref.clone(),
    };
    let workflow_status = match &result {
        Ok(value) => Some(value.workflow_status),
        Err(_) => Some(WorkflowStatus::Failed),
    };

    let completion = RunnerEvent {
        event: "runner_complete",
        task_id: subject_id,
        workflow_id: result.as_ref().ok().map(|value| value.workflow_id.clone()),
        workflow_ref,
        workflow_status,
        exit_code: Some(exit_code),
    };
    eprintln!("{}", serde_json::to_string(&completion).unwrap_or_default());

    if let Err(ref error) = result {
        eprintln!("workflow execution failed: {error}");
    }

    Ok(clamp_exit_code(exit_code))
}

fn clamp_exit_code(code: i32) -> u8 {
    match u8::try_from(code) {
        Ok(value) => value,
        Err(_) => {
            if code < 0 {
                1
            } else {
                u8::MAX
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamp_exit_code_zero() {
        assert_eq!(clamp_exit_code(0), 0);
    }

    #[test]
    fn clamp_exit_code_normal() {
        assert_eq!(clamp_exit_code(1), 1);
        assert_eq!(clamp_exit_code(42), 42);
        assert_eq!(clamp_exit_code(255), 255);
    }

    #[test]
    fn clamp_exit_code_negative() {
        assert_eq!(clamp_exit_code(-1), 1);
        assert_eq!(clamp_exit_code(-128), 1);
    }

    #[test]
    fn clamp_exit_code_overflow() {
        assert_eq!(clamp_exit_code(256), u8::MAX);
        assert_eq!(clamp_exit_code(i32::MAX), u8::MAX);
    }

    #[test]
    fn runner_event_serialization() {
        let event = RunnerEvent {
            event: "runner_start",
            task_id: "TASK-001".to_string(),
            workflow_id: None,
            workflow_ref: Some("default".to_string()),
            workflow_status: None,
            exit_code: None,
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("runner_start"));
        assert!(json.contains("TASK-001"));
        assert!(!json.contains("exit_code"));

        let complete = RunnerEvent {
            event: "runner_complete",
            task_id: "TASK-001".to_string(),
            workflow_id: Some("WF-001".to_string()),
            workflow_ref: None,
            workflow_status: Some(WorkflowStatus::Completed),
            exit_code: Some(0),
        };
        let json = serde_json::to_string(&complete).unwrap();
        assert!(json.contains("runner_complete"));
        assert!(json.contains("\"workflow_id\":\"WF-001\""));
        assert!(json.contains("\"workflow_status\":\"completed\""));
        assert!(json.contains("\"exit_code\":0"));
        assert!(!json.contains("workflow_ref"));
    }

    #[test]
    fn source_contains_no_subprocess_delegation() {
        let source = include_str!("main.rs");
        let marker_command = format!("{}Command", "Tokio");
        let marker_resolve = format!("{}ao_binary", "resolve_");
        assert!(source.matches(&marker_command).count() == 0, "main.rs must not delegate to subprocess");
        assert!(source.matches(&marker_resolve).count() == 0, "main.rs must not contain proxy logic");
    }
}
