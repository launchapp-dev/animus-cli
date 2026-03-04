use std::path::Path;
use std::process::ExitCode;

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use clap::{Args, Parser, Subcommand};
use orchestrator_core::{
    load_workflow_config, resolve_pipeline_phase_plan, PhaseExecutionRequest, PhaseExecutionResult,
    PhaseExecutor, PhaseVerdict,
};
use serde::Serialize;

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
    task_id: String,

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
struct PhaseReport {
    task_id: String,
    pipeline_id: String,
    phase_id: String,
    exit_code: i32,
    verdict: String,
    error: Option<String>,
    commit_message: Option<String>,
}

impl PhaseReport {
    fn new(task_id: String, pipeline_id: String, phase_id: String, result: &PhaseExecutionResult) -> Self {
        let verdict = match &result.verdict {
            PhaseVerdict::Advance => "advance",
            PhaseVerdict::Rework { target_phase } => {
                return Self {
                    task_id,
                    pipeline_id,
                    phase_id,
                    exit_code: result.exit_code,
                    verdict: format!("rework:{target_phase}"),
                    error: result.error.clone(),
                    commit_message: result.commit_message.clone(),
                };
            }
            PhaseVerdict::Skip => "skip",
            PhaseVerdict::Failed { .. } => "failed",
        }
        .to_string();

        Self {
            task_id,
            pipeline_id,
            phase_id,
            exit_code: result.exit_code,
            verdict,
            error: result.error.clone(),
            commit_message: result.commit_message.clone(),
        }
    }
}

#[derive(Debug, Default)]
struct StubPhaseExecutor;

#[async_trait]
impl PhaseExecutor for StubPhaseExecutor {
    async fn execute_phase(
        &self,
        request: PhaseExecutionRequest,
    ) -> Result<PhaseExecutionResult> {
        let payload = serde_json::json!({
            "task_id": request.task_id.clone(),
            "phase_id": request.phase_id.clone(),
            "pipeline_id": request.pipeline_id.clone(),
            "project_root": request.project_root,
            "config_dir": request.config_dir,
            "model_override": request.model_override,
            "tool_override": request.tool_override,
            "timeout": request.timeout,
        });
        println!(
            "ao-workflow-runner stub: task='{}' phase='{}' pipeline='{}' model={:?} tool={:?} timeout={:?}",
            payload["task_id"],
            payload["phase_id"],
            payload["pipeline_id"],
            payload["model_override"],
            payload["tool_override"],
            payload["timeout"]
        );

        let output_log = serde_json::to_string_pretty(&payload)?;
        Ok(PhaseExecutionResult {
            exit_code: 0,
            verdict: PhaseVerdict::Advance,
            output_log,
            error: None,
            commit_message: Some(format!(
                "stub executor: no-op for phase {}",
                payload["phase_id"]
            )),
        })
    }
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

async fn run_execute(args: WorkflowExecuteArgs) -> Result<u8> {
    let workflow_config = load_workflow_config(Path::new(&args.project_root))
        .with_context(|| format!("failed to load workflow config from {}", args.project_root))?;

    let pipeline_id = args
        .pipeline
        .clone()
        .unwrap_or_else(|| workflow_config.default_pipeline_id.trim().to_string());

    let phase_ids = resolve_pipeline_phase_plan(&workflow_config, args.pipeline.as_deref()).ok_or_else(
        || {
            anyhow!(
                "pipeline '{}' is not found or has no executable phases",
                pipeline_id
            )
        },
    )?;

    let config_dir = args.config_path.clone().unwrap_or_else(|| args.project_root.clone());
    let executor = StubPhaseExecutor::default();
    let mut reports = Vec::new();

    for phase_id in phase_ids {
        let request = PhaseExecutionRequest {
            task_id: args.task_id.clone(),
            phase_id: phase_id.clone(),
            pipeline_id: pipeline_id.clone(),
            project_root: args.project_root.clone(),
            config_dir: config_dir.clone(),
            model_override: args.model.clone(),
            tool_override: args.tool.clone(),
            timeout: args.phase_timeout_secs,
        };

        let result = executor
            .execute_phase(request)
            .await
            .with_context(|| format!("failed to execute phase '{}'", phase_id))?;

        let report = PhaseReport::new(args.task_id.clone(), pipeline_id.clone(), phase_id.clone(), &result);
        println!("{}", serde_json::to_string_pretty(&report)?);
        reports.push(report);

        if result.exit_code != 0 {
            return Ok(clamp_exit_code(result.exit_code));
        }

        if let PhaseVerdict::Failed { reason } = result.verdict {
            eprintln!("phase '{}' failed: {reason}", phase_id);
            return Ok(clamp_exit_code(result.exit_code.max(1)));
        }
    }

    let summary = serde_json::json!({
        "status": "completed",
        "task_id": args.task_id,
        "pipeline_id": pipeline_id,
        "phases": reports,
    });
    println!("{}", serde_json::to_string_pretty(&summary)?);

    Ok(0)
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
