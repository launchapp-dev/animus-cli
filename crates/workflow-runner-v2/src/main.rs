use std::process::ExitCode;

use clap::{Args, Parser, Subcommand};
use orchestrator_core::WorkflowStatus;
use serde::Serialize;

use workflow_runner_v2::workflow_execute::{execute_workflow, WorkflowExecuteParams};

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
        on_phase_event: None,
        hub: None,
        phase_routing,
        mcp_config,
    };

    let result = execute_workflow(params).await;

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
