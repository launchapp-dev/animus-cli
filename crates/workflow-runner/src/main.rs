use std::process::ExitCode;

use clap::{Args, Parser, Subcommand};
use serde::Serialize;

use workflow_runner::workflow_execute::{execute_workflow, WorkflowExecuteParams};

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
    task_id: Option<String>,

    #[arg(long)]
    requirement_id: Option<String>,

    #[arg(long)]
    title: Option<String>,

    #[arg(long)]
    description: Option<String>,

    #[arg(long)]
    pipeline: Option<String>,

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
}

#[derive(Debug, Serialize)]
struct RunnerEvent {
    event: &'static str,
    task_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pipeline: Option<String>,
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
        .task_id
        .as_deref()
        .or(args.requirement_id.as_deref())
        .or(args.title.as_deref())
        .unwrap_or("unknown")
        .to_string();

    let startup = RunnerEvent {
        event: "runner_start",
        task_id: subject_id.clone(),
        pipeline: args.pipeline.clone(),
        exit_code: None,
    };
    eprintln!("{}", serde_json::to_string(&startup).unwrap_or_default());

    let params = WorkflowExecuteParams {
        project_root: args.project_root,
        task_id: args.task_id,
        requirement_id: args.requirement_id,
        title: args.title,
        description: args.description,
        pipeline_id: args.pipeline.clone(),
        model: args.model,
        tool: args.tool,
        phase_timeout_secs: args.phase_timeout_secs,
    };

    let result = execute_workflow(params).await;

    let exit_code: i32 = match &result {
        Ok(r) if r.success => 0,
        Ok(_) => 1,
        Err(_) => 1,
    };

    let completion = RunnerEvent {
        event: "runner_complete",
        task_id: subject_id,
        pipeline: args.pipeline,
        exit_code: Some(exit_code),
    };
    eprintln!(
        "{}",
        serde_json::to_string(&completion).unwrap_or_default()
    );

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
            pipeline: Some("default".to_string()),
            exit_code: None,
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("runner_start"));
        assert!(json.contains("TASK-001"));
        assert!(!json.contains("exit_code"));

        let complete = RunnerEvent {
            event: "runner_complete",
            task_id: "TASK-001".to_string(),
            pipeline: None,
            exit_code: Some(0),
        };
        let json = serde_json::to_string(&complete).unwrap();
        assert!(json.contains("runner_complete"));
        assert!(json.contains("\"exit_code\":0"));
        assert!(!json.contains("pipeline"));
    }

    #[test]
    fn source_contains_no_subprocess_delegation() {
        let source = include_str!("main.rs");
        let marker_command = format!("{}Command", "Tokio");
        let marker_resolve = format!("{}ao_binary", "resolve_");
        assert!(
            source.matches(&marker_command).count() == 0,
            "main.rs must not delegate to subprocess"
        );
        assert!(
            source.matches(&marker_resolve).count() == 0,
            "main.rs must not contain proxy logic"
        );
    }
}
