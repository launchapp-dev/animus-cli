use clap::{Args, Subcommand};

use super::{parse_positive_usize, IdArgs, INPUT_JSON_PRECEDENCE_HELP};

#[derive(Debug, Subcommand)]
pub(crate) enum WorkflowCommand {
    /// List workflows.
    List,
    /// Get workflow details.
    Get(IdArgs),
    /// Show workflow decisions.
    Decisions(IdArgs),
    /// List and inspect workflow checkpoints.
    Checkpoints {
        #[command(subcommand)]
        command: WorkflowCheckpointCommand,
    },
    /// Start a workflow for a task.
    Run(WorkflowRunArgs),
    /// Resume a paused workflow.
    Resume(IdArgs),
    /// Check whether a workflow can be resumed.
    ResumeStatus(IdArgs),
    /// Pause an active workflow (confirmation required).
    Pause(WorkflowPauseArgs),
    /// Cancel a workflow (confirmation required).
    Cancel(WorkflowCancelArgs),
    /// Manual actions for a specific workflow phase.
    Phase {
        #[command(subcommand)]
        command: WorkflowPhaseCommand,
    },
    /// Manage workflow phase definitions.
    Phases {
        #[command(subcommand)]
        command: WorkflowPhasesCommand,
    },
    /// Manage workflow pipeline definitions.
    Pipelines {
        #[command(subcommand)]
        command: WorkflowPipelinesCommand,
    },
    /// Read and validate workflow configuration.
    Config {
        #[command(subcommand)]
        command: WorkflowConfigCommand,
    },
    /// Read and update workflow state machine configuration.
    StateMachine {
        #[command(subcommand)]
        command: WorkflowStateMachineCommand,
    },
    /// Read and update workflow agent runtime configuration.
    AgentRuntime {
        #[command(subcommand)]
        command: WorkflowAgentRuntimeCommand,
    },
    /// Execute a workflow synchronously (no daemon required).
    Execute(WorkflowExecuteArgs),
    /// Update a pipeline by id.
    UpdatePipeline(WorkflowPipelineUpdateArgs),
}

#[derive(Debug, Subcommand)]
pub(crate) enum WorkflowPhaseCommand {
    /// Approve a pending phase gate.
    Approve(WorkflowPhaseApproveArgs),
}

#[derive(Debug, Subcommand)]
pub(crate) enum WorkflowPhasesCommand {
    /// List configured workflow phases.
    List,
    /// Get a workflow phase by id.
    Get(WorkflowPhaseGetArgs),
    /// Create or replace a workflow phase definition.
    Upsert(WorkflowPhaseUpsertArgs),
    /// Remove a workflow phase definition (confirmation required).
    Remove(WorkflowPhaseRemoveArgs),
}

#[derive(Debug, Subcommand)]
pub(crate) enum WorkflowPipelinesCommand {
    /// List configured workflow pipelines.
    List,
    /// Create or replace a workflow pipeline definition.
    Upsert(WorkflowPipelineUpsertArgs),
}

#[derive(Debug, Subcommand)]
pub(crate) enum WorkflowConfigCommand {
    /// Read resolved workflow config.
    Get,
    /// Validate workflow config shape and references.
    Validate,
    /// Migrate legacy workflow config to v2.
    MigrateV2,
}

#[derive(Debug, Subcommand)]
pub(crate) enum WorkflowStateMachineCommand {
    /// Read workflow state-machine config.
    Get,
    /// Validate workflow state-machine config.
    Validate,
    /// Replace workflow state-machine config JSON.
    Set(WorkflowStateMachineSetArgs),
}

#[derive(Debug, Subcommand)]
pub(crate) enum WorkflowAgentRuntimeCommand {
    /// Read workflow agent-runtime config.
    Get,
    /// Validate workflow agent-runtime config.
    Validate,
    /// Replace workflow agent-runtime config JSON.
    Set(WorkflowAgentRuntimeSetArgs),
}

#[derive(Debug, Subcommand)]

pub(crate) enum WorkflowCheckpointCommand {
    /// List checkpoints for a workflow.
    List(IdArgs),
    /// Get a specific checkpoint for a workflow.
    Get(WorkflowCheckpointGetArgs),
    /// Prune checkpoints using count and/or age retention.
    Prune(WorkflowCheckpointPruneArgs),
}

#[derive(Debug, Args)]
pub(crate) struct WorkflowCheckpointGetArgs {
    #[arg(long, value_name = "WORKFLOW_ID", help = "Workflow identifier.")]
    pub(crate) id: String,
    #[arg(long, value_name = "INDEX", help = "Checkpoint index (zero-based).")]
    pub(crate) checkpoint: usize,
}

#[derive(Debug, Args)]
pub(crate) struct WorkflowCheckpointPruneArgs {
    #[arg(long, value_name = "WORKFLOW_ID", help = "Workflow identifier.")]
    pub(crate) id: String,
    #[arg(
        long,
        value_name = "COUNT",
        value_parser = parse_positive_usize,
        default_value_t = orchestrator_core::DEFAULT_CHECKPOINT_RETENTION_KEEP_LAST_PER_PHASE,
        help = "Retain at most this many checkpoints per phase."
    )]
    pub(crate) keep_last_per_phase: usize,
    #[arg(
        long,
        value_name = "HOURS",
        help = "Additionally prune checkpoints older than this age in hours."
    )]
    pub(crate) max_age_hours: Option<u64>,
    #[arg(
        long,
        default_value_t = false,
        help = "Preview prune result without deleting checkpoint files."
    )]
    pub(crate) dry_run: bool,
}

#[derive(Debug, Args)]
pub(crate) struct WorkflowRunArgs {
    #[arg(
        long,
        value_name = "TASK_ID",
        help = "Task id to run the workflow for."
    )]
    pub(crate) task_id: String,
    #[arg(
        long,
        value_name = "PIPELINE_ID",
        help = "Optional pipeline id override."
    )]
    pub(crate) pipeline_id: Option<String>,
    #[arg(long, value_name = "JSON", help = INPUT_JSON_PRECEDENCE_HELP)]
    pub(crate) input_json: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct WorkflowExecuteArgs {
    #[arg(long, value_name = "TASK_ID", help = "Task id to execute the workflow for.")]
    pub(crate) task_id: String,
    #[arg(long, value_name = "PIPELINE_ID", help = "Optional pipeline id override.")]
    pub(crate) pipeline_id: Option<String>,
    #[arg(long, value_name = "PHASE_ID", help = "Run only this specific phase instead of the full workflow.")]
    pub(crate) phase: Option<String>,
    #[arg(long, value_name = "MODEL_ID", help = "Override the model for phase execution.")]
    pub(crate) model: Option<String>,
    #[arg(long, value_name = "TOOL_ID", help = "Override the tool/CLI for phase execution (claude, codex, gemini).")]
    pub(crate) tool: Option<String>,
    #[arg(long, value_name = "SECS", help = "Override phase timeout in seconds.")]
    pub(crate) phase_timeout_secs: Option<u64>,
    #[arg(long, value_name = "JSON", help = "JSON payload for additional config overrides.")]
    pub(crate) input_json: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct WorkflowPauseArgs {
    #[arg(long, value_name = "WORKFLOW_ID", help = "Workflow identifier.")]
    pub(crate) id: String,
    #[arg(
        long,
        value_name = "WORKFLOW_ID",
        help = "Confirmation token; must match --id."
    )]
    pub(crate) confirm: Option<String>,
    #[arg(
        long,
        default_value_t = false,
        help = "Preview pause payload without mutating workflow state."
    )]
    pub(crate) dry_run: bool,
}

#[derive(Debug, Args)]
pub(crate) struct WorkflowCancelArgs {
    #[arg(long, value_name = "WORKFLOW_ID", help = "Workflow identifier.")]
    pub(crate) id: String,
    #[arg(
        long,
        value_name = "WORKFLOW_ID",
        help = "Confirmation token; must match --id."
    )]
    pub(crate) confirm: Option<String>,
    #[arg(
        long,
        default_value_t = false,
        help = "Preview cancellation payload without mutating workflow state."
    )]
    pub(crate) dry_run: bool,
}

#[derive(Debug, Args)]
pub(crate) struct WorkflowPhaseApproveArgs {
    #[arg(long, value_name = "WORKFLOW_ID", help = "Workflow identifier.")]
    pub(crate) id: String,
    #[arg(long, value_name = "PHASE_ID", help = "Phase identifier.")]
    pub(crate) phase: String,
    #[arg(long, value_name = "TEXT", help = "Approval note for the phase gate.")]
    pub(crate) note: String,
}

#[derive(Debug, Args)]
pub(crate) struct WorkflowPhaseGetArgs {
    #[arg(long, value_name = "PHASE_ID", help = "Phase identifier.")]
    pub(crate) phase: String,
}

#[derive(Debug, Args)]
pub(crate) struct WorkflowPhaseUpsertArgs {
    #[arg(long, value_name = "PHASE_ID", help = "Phase identifier.")]
    pub(crate) phase: String,
    #[arg(
        long,
        value_name = "JSON",
        help = "Phase runtime definition JSON payload."
    )]
    pub(crate) input_json: String,
}

#[derive(Debug, Args)]
pub(crate) struct WorkflowPhaseRemoveArgs {
    #[arg(long, value_name = "PHASE_ID", help = "Phase identifier.")]
    pub(crate) phase: String,
    #[arg(
        long,
        value_name = "PHASE_ID",
        help = "Confirmation token; must match --phase."
    )]
    pub(crate) confirm: Option<String>,
    #[arg(
        long,
        default_value_t = false,
        help = "Preview phase removal impact without mutating workflow config."
    )]
    pub(crate) dry_run: bool,
}

#[derive(Debug, Args)]
pub(crate) struct WorkflowPipelineUpsertArgs {
    #[arg(
        long,
        value_name = "JSON",
        help = "Workflow pipeline definition JSON payload."
    )]
    pub(crate) input_json: String,
}

#[derive(Debug, Args)]
pub(crate) struct WorkflowPipelineUpdateArgs {
    #[arg(long, value_name = "PIPELINE_ID", help = "Pipeline identifier.")]
    pub(crate) id: String,
    #[arg(long, value_name = "NAME", help = "Pipeline display name.")]
    pub(crate) name: String,
    #[arg(long, value_name = "TEXT", help = "Optional pipeline description.")]
    pub(crate) description: Option<String>,
    #[arg(
        long = "phase",
        value_name = "PHASE_ID",
        help = "Ordered phase ids for the pipeline. Repeat to add multiple phases."
    )]
    pub(crate) phases: Vec<String>,
    #[arg(long, value_name = "JSON", help = INPUT_JSON_PRECEDENCE_HELP)]
    pub(crate) input_json: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct WorkflowStateMachineSetArgs {
    #[arg(
        long,
        value_name = "JSON",
        help = "Workflow state-machine configuration JSON payload."
    )]
    pub(crate) input_json: String,
}

#[derive(Debug, Args)]
pub(crate) struct WorkflowAgentRuntimeSetArgs {
    #[arg(
        long,
        value_name = "JSON",
        help = "Workflow agent-runtime configuration JSON payload."
    )]
    pub(crate) input_json: String,
}
