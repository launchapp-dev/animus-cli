use clap::{Args, Subcommand};

use super::{
    parse_duration_secs_default_days, parse_positive_usize, ACTOR_JSON_HELP, INPUT_JSON_PRECEDENCE_HELP,
    WORKFLOW_SORT_HELP, WORKFLOW_STATUS_HELP,
};

/// Workflow identifier args: `--id` is primary, `--workflow-id` is a hidden
/// alias so domain-prefixed scripts keep working, `-i` is the short form.
#[derive(Debug, Args)]
pub(crate) struct WorkflowIdArgs {
    #[arg(short, long, alias = "workflow-id", value_name = "WORKFLOW_ID", help = "Workflow identifier.")]
    pub(crate) id: String,
}

#[derive(Debug, Subcommand)]
pub(crate) enum WorkflowCommand {
    /// List workflows.
    List(WorkflowListArgs),
    /// Get workflow details.
    Get(WorkflowIdArgs),
    /// Show workflow decisions.
    Decisions(WorkflowIdArgs),
    /// List and inspect workflow checkpoints.
    Checkpoints {
        #[command(subcommand)]
        command: WorkflowCheckpointCommand,
    },
    /// Run a workflow. Spawns a detached workflow_runner by default; use --sync to run in terminal.
    Run(WorkflowRunArgs),
    /// Resume a paused workflow and respawn its workflow_runner.
    Resume(WorkflowResumeArgs),
    /// Check whether a workflow can be resumed.
    ResumeStatus(WorkflowIdArgs),
    /// Pause an active workflow (confirmation required).
    Pause(WorkflowPauseArgs),
    /// Cancel a workflow (confirmation required).
    Cancel(WorkflowCancelArgs),
    /// Prune terminal workflow runs from history and disk. Dry-run by default; pass --yes to delete.
    Prune(WorkflowPruneArgs),
    /// Delete a single terminal workflow run from history and disk. Dry-run by default; pass --yes to delete.
    Delete(WorkflowDeleteArgs),
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
    /// Manage workflow definitions.
    Definitions {
        #[command(subcommand)]
        command: WorkflowDefinitionsCommand,
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
    /// Inspect rendered workflow phase prompts.
    Prompt {
        #[command(subcommand)]
        command: WorkflowPromptCommand,
    },
}

#[derive(Debug, Args)]
pub(crate) struct WorkflowListArgs {
    #[arg(long, value_name = "STATUS", help = WORKFLOW_STATUS_HELP)]
    pub(crate) status: Option<String>,
    #[arg(long, value_name = "WORKFLOW_REF", help = "Filter workflows by workflow definition/reference id.")]
    pub(crate) workflow_ref: Option<String>,
    #[arg(long, value_name = "TASK_ID", help = "Filter workflows linked to a task id.")]
    pub(crate) task_id: Option<String>,
    #[arg(long, value_name = "PHASE_ID", help = "Filter workflows containing a phase id.")]
    pub(crate) phase_id: Option<String>,
    #[arg(
        long,
        value_name = "TEXT",
        help = "Case-insensitive text search over workflow id, task id, ref, and phases."
    )]
    pub(crate) search: Option<String>,
    #[arg(long, value_name = "SORT", help = WORKFLOW_SORT_HELP)]
    pub(crate) sort: Option<String>,
    #[arg(
        long,
        value_name = "COUNT",
        value_parser = parse_positive_usize,
        help = "Maximum number of workflows to return."
    )]
    pub(crate) limit: Option<usize>,
    #[arg(
        long,
        value_name = "COUNT",
        default_value_t = 0,
        help = "Number of workflows to skip before returning results."
    )]
    pub(crate) offset: usize,
}

#[derive(Debug, Subcommand)]
pub(crate) enum WorkflowPhaseCommand {
    /// Approve a pending phase gate.
    Approve(WorkflowPhaseApproveArgs),
    /// Reject a pending phase gate.
    Reject(WorkflowPhaseRejectArgs),
}

#[derive(Debug, Subcommand)]
pub(crate) enum WorkflowPhasesCommand {
    /// List configured workflow phases.
    List,
    /// Get a workflow phase by id.
    Get(WorkflowPhaseGetArgs),
    /// Create or replace a phase definition in the generated overlay.
    Upsert(WorkflowPhaseUpsertArgs),
    /// Remove a generated-overlay phase definition (confirmation required).
    Remove(WorkflowPhaseRemoveArgs),
}

#[derive(Debug, Subcommand)]
pub(crate) enum WorkflowPromptCommand {
    /// Render workflow phase prompt text and prompt sections.
    Render(WorkflowPromptRenderArgs),
}

#[derive(Debug, Subcommand)]
pub(crate) enum WorkflowDefinitionsCommand {
    /// List configured workflow definitions.
    List,
    /// Create or replace a workflow definition.
    Upsert(WorkflowDefinitionUpsertArgs),
}

#[derive(Debug, Subcommand)]
pub(crate) enum WorkflowConfigCommand {
    /// Read resolved workflow config.
    Get(WorkflowConfigReadArgs),
    /// Validate workflow config shape and references.
    Validate(WorkflowConfigReadArgs),
    /// Validate and resolve YAML workflow files.
    Compile,
    /// Re-run the workflow YAML compile pipeline and (when the daemon is
    /// running) swap the in-memory config snapshot. Useful when filesystem
    /// notifications are unreliable on the host filesystem.
    Reload,
    /// Replace the entire workflow config by writing it through the installed
    /// writable config_source plugin. Validates before writing; rejected when
    /// the source is read-only (e.g. YAML).
    Set(WorkflowConfigSetArgs),
    /// Create or replace one agent definition (read-modify-write the full
    /// config). Does not collide with runtime `animus agent` verbs.
    AgentSet(WorkflowConfigAgentSetArgs),
    /// Remove one agent definition (read-modify-write the full config).
    AgentRemove(WorkflowConfigEntityIdArgs),
    /// Create or replace one workflow definition (read-modify-write).
    WorkflowSet(WorkflowConfigWorkflowSetArgs),
    /// Remove one workflow definition (read-modify-write).
    WorkflowRemove(WorkflowConfigEntityIdArgs),
}

/// Shared args for `workflow config get` / `workflow config validate`.
///
/// `--actor-json` scopes the resolved config to the asserted user
/// (global ∪ private ∪ shared; admins see all). Omit for global-only.
#[derive(Debug, Args)]
pub(crate) struct WorkflowConfigReadArgs {
    #[arg(long, value_name = "JSON", help = ACTOR_JSON_HELP)]
    pub(crate) actor_json: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct WorkflowConfigSetArgs {
    #[arg(
        long,
        value_name = "PATH",
        help = "Path to a JSON file with the full WorkflowConfig. Use '-' or omit to read JSON from stdin."
    )]
    pub(crate) file: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct WorkflowConfigAgentSetArgs {
    #[arg(long, value_name = "AGENT_ID", help = "Agent definition id to create or replace.")]
    pub(crate) id: String,
    #[arg(long, value_name = "JSON", help = "Agent profile overlay JSON payload (the value of agent_profiles.<id>).")]
    pub(crate) input_json: String,
}

#[derive(Debug, Args)]
pub(crate) struct WorkflowConfigWorkflowSetArgs {
    #[arg(long, value_name = "JSON", help = "Workflow definition JSON payload (must include an 'id' field).")]
    pub(crate) input_json: String,
}

#[derive(Debug, Args)]
pub(crate) struct WorkflowConfigEntityIdArgs {
    #[arg(long, value_name = "ID", help = "Entity id (agent id or workflow id) to remove.")]
    pub(crate) id: String,
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
    List(WorkflowIdArgs),
    /// Get a specific checkpoint for a workflow.
    Get(WorkflowCheckpointGetArgs),
    /// Prune checkpoints using count and/or age retention.
    Prune(WorkflowCheckpointPruneArgs),
}

#[derive(Debug, Args)]
pub(crate) struct WorkflowCheckpointGetArgs {
    #[arg(long, alias = "workflow-id", value_name = "WORKFLOW_ID", help = "Workflow identifier.")]
    pub(crate) id: String,
    #[arg(long, value_name = "INDEX", help = "Checkpoint index (zero-based).")]
    pub(crate) checkpoint: usize,
}

#[derive(Debug, Args)]
pub(crate) struct WorkflowCheckpointPruneArgs {
    #[arg(long, alias = "workflow-id", value_name = "WORKFLOW_ID", help = "Workflow identifier.")]
    pub(crate) id: String,
    #[arg(
        long,
        value_name = "COUNT",
        value_parser = parse_positive_usize,
        default_value_t = orchestrator_core::DEFAULT_CHECKPOINT_RETENTION_KEEP_LAST_PER_PHASE,
        help = "Retain at most this many checkpoints per phase."
    )]
    pub(crate) keep_last_per_phase: usize,
    #[arg(long, value_name = "HOURS", help = "Additionally prune checkpoints older than this age in hours.")]
    pub(crate) max_age_hours: Option<u64>,
    #[arg(long, default_value_t = false, help = "Preview prune result without deleting checkpoint files.")]
    pub(crate) dry_run: bool,
}

#[derive(Debug, Args)]
pub(crate) struct WorkflowRunArgs {
    #[arg(
        value_name = "PIPELINE",
        help = "Workflow definition name from project YAML or an installed pack (e.g. standard-workflow, hotfix-workflow, vendor.pack/review)."
    )]
    pub(crate) pipeline: Option<String>,
    #[arg(
        long,
        value_name = "TASK_ID",
        group = "subject",
        help = "Task to run the workflow for. Accepts a bare id (TASK-001) or the qualified form (task:TASK-001)."
    )]
    pub(crate) task_id: Option<String>,
    #[arg(long, value_name = "REQ_ID", group = "subject", help = "Requirement id to run the workflow for.")]
    pub(crate) requirement_id: Option<String>,
    #[arg(long, value_name = "TITLE", group = "subject", help = "Custom workflow title for freeform execution.")]
    pub(crate) title: Option<String>,
    #[arg(
        long = "subject-id",
        value_name = "SUBJECT_ID",
        group = "subject",
        help = "Generic subject to run the workflow for, any kind (BaaS dynamic kinds like blog/post). Accepts a qualified id (blog:BLOG-001 — kind trusted; the recommended form) or a bare id (BLOG-001 — kind probed across backends that declare concrete kinds; pure catch-all dynamic backends require the qualified form). Mutually exclusive with --task-id / --requirement-id / --title."
    )]
    pub(crate) subject_id: Option<String>,
    #[arg(long, value_name = "TEXT", help = "Custom workflow description (used with --title).")]
    pub(crate) description: Option<String>,
    #[arg(
        long,
        default_value_t = false,
        help = "Run synchronously in the terminal instead of enqueueing to the daemon."
    )]
    pub(crate) sync: bool,
    #[arg(long, value_name = "WORKFLOW_ID", help = "Resume an existing workflow from its current phase (sync only).")]
    pub(crate) workflow_id: Option<String>,
    #[arg(
        long,
        value_name = "PHASE_ID",
        help = "Run only this specific phase instead of the full pipeline (sync only)."
    )]
    pub(crate) phase: Option<String>,
    #[arg(long, value_name = "MODEL_ID", help = "Override the model for phase execution.")]
    pub(crate) model: Option<String>,
    #[arg(long, value_name = "TOOL_ID", help = "Override the tool/CLI for phase execution (claude, codex, gemini).")]
    pub(crate) tool: Option<String>,
    #[arg(long, value_name = "SECS", help = "Override phase timeout in seconds.")]
    pub(crate) phase_timeout_secs: Option<u64>,
    #[arg(long, value_name = "JSON", help = INPUT_JSON_PRECEDENCE_HELP)]
    pub(crate) input_json: Option<String>,
    #[arg(
        long = "var",
        value_name = "KEY=VALUE",
        help = "Workflow variable in KEY=VALUE format. Repeat for multiple variables."
    )]
    pub(crate) vars: Vec<String>,
    #[arg(long, value_name = "JSON", help = ACTOR_JSON_HELP)]
    pub(crate) actor_json: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct WorkflowPromptRenderArgs {
    #[arg(
        long,
        value_name = "WORKFLOW_ID",
        group = "subject",
        help = "Existing workflow id to render from persisted workflow state."
    )]
    pub(crate) workflow_id: Option<String>,
    #[arg(long, value_name = "TASK_ID", group = "subject", help = "Task id for ad-hoc prompt rendering.")]
    pub(crate) task_id: Option<String>,
    #[arg(
        long,
        value_name = "REQ_ID",
        group = "subject",
        help = "Requirement id for ad-hoc prompt rendering (alternative to --task-id)."
    )]
    pub(crate) requirement_id: Option<String>,
    #[arg(long, value_name = "TITLE", group = "subject", help = "Custom workflow title for ad-hoc prompt rendering.")]
    pub(crate) title: Option<String>,
    #[arg(long, value_name = "TEXT", help = "Custom workflow description (used with --title).")]
    pub(crate) description: Option<String>,
    #[arg(long, value_name = "WORKFLOW_REF", help = "Optional YAML workflow reference override for ad-hoc rendering.")]
    pub(crate) workflow_ref: Option<String>,
    #[arg(
        long,
        value_name = "PHASE_ID",
        help = "Specific phase to render. Defaults to the current phase for --workflow-id."
    )]
    pub(crate) phase: Option<String>,
    #[arg(long, default_value_t = false, help = "Render every phase in the selected workflow/pipeline.")]
    pub(crate) all_phases: bool,
    #[arg(long, value_name = "JSON", help = INPUT_JSON_PRECEDENCE_HELP)]
    pub(crate) input_json: Option<String>,
    #[arg(long, value_name = "TEXT", help = "Optional rework/failure context override for ad-hoc rendering.")]
    pub(crate) rework_context: Option<String>,
    #[arg(
        long = "var",
        value_name = "KEY=VALUE",
        help = "Workflow variable in KEY=VALUE format. Repeat for multiple variables."
    )]
    pub(crate) vars: Vec<String>,
}

#[derive(Debug, Args)]
pub(crate) struct WorkflowResumeArgs {
    #[arg(long, alias = "workflow-id", value_name = "WORKFLOW_ID", help = "Workflow identifier.")]
    pub(crate) id: String,
    #[arg(
        long,
        default_value_t = false,
        help = "Bypass idempotency block produced during crash recovery and auto-retry the phase."
    )]
    pub(crate) force: bool,
}

#[derive(Debug, Args)]
pub(crate) struct WorkflowPauseArgs {
    #[arg(long, alias = "workflow-id", value_name = "WORKFLOW_ID", help = "Workflow identifier.")]
    pub(crate) id: String,
    #[arg(
        long,
        value_name = "WORKFLOW_ID",
        help = "Confirmation token; must match --id. Deliberate repeat-the-id safety pattern (not a --yes style skip flag)."
    )]
    pub(crate) confirm: Option<String>,
    #[arg(long, default_value_t = false, help = "Preview pause payload without mutating workflow state.")]
    pub(crate) dry_run: bool,
}

#[derive(Debug, Args)]
pub(crate) struct WorkflowCancelArgs {
    #[arg(long, alias = "workflow-id", value_name = "WORKFLOW_ID", help = "Workflow identifier.")]
    pub(crate) id: String,
    #[arg(
        long,
        value_name = "WORKFLOW_ID",
        help = "Confirmation token; must match --id. Deliberate repeat-the-id safety pattern (not a --yes style skip flag)."
    )]
    pub(crate) confirm: Option<String>,
    #[arg(long, default_value_t = false, help = "Preview cancellation payload without mutating workflow state.")]
    pub(crate) dry_run: bool,
}

#[derive(Debug, Args)]
pub(crate) struct WorkflowPruneArgs {
    #[arg(
        long,
        value_name = "AGE",
        value_parser = parse_duration_secs_default_days,
        help = "Only prune runs that completed (or started) more than this long ago. Accepts a bare number of days (e.g. 30) or a unit suffix: 30d, 12h, 45m, 90s."
    )]
    pub(crate) older_than: Option<u64>,
    #[arg(
        long,
        value_name = "COUNT",
        help = "Keep the N most recent matching runs overall (not per workflow definition) and prune the rest."
    )]
    pub(crate) keep_last: Option<usize>,
    #[arg(
        long,
        value_name = "STATUS",
        help = "Only prune runs with this terminal status (completed, failed, escalated, cancelled). Default: all terminal statuses. In-progress, queued, and paused runs are never pruned."
    )]
    pub(crate) status: Option<String>,
    #[arg(
        long,
        default_value_t = false,
        help = "Actually delete the matching runs. Without this flag the command is a dry-run preview."
    )]
    pub(crate) yes: bool,
}

#[derive(Debug, Args)]
pub(crate) struct WorkflowDeleteArgs {
    #[arg(long, value_name = "RUN_ID", help = "Workflow run identifier to delete.")]
    pub(crate) run_id: String,
    #[arg(
        long,
        default_value_t = false,
        help = "Actually delete the run. Without this flag the command is a dry-run preview."
    )]
    pub(crate) yes: bool,
}

#[derive(Debug, Args)]
pub(crate) struct WorkflowPhaseApproveArgs {
    #[arg(long, alias = "workflow-id", value_name = "WORKFLOW_ID", help = "Workflow identifier.")]
    pub(crate) id: String,
    #[arg(long, value_name = "PHASE_ID", help = "Phase identifier.")]
    pub(crate) phase: String,
    #[arg(long, value_name = "TEXT", default_value = "Approved", help = "Approval note for the phase gate.")]
    pub(crate) note: String,
}

#[derive(Debug, Args)]
pub(crate) struct WorkflowPhaseRejectArgs {
    #[arg(long, alias = "workflow-id", value_name = "WORKFLOW_ID", help = "Workflow identifier.")]
    pub(crate) id: String,
    #[arg(long, value_name = "PHASE_ID", help = "Phase identifier.")]
    pub(crate) phase: String,
    #[arg(long, value_name = "TEXT", help = "Rejection note for the phase gate.")]
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
    #[arg(long, value_name = "JSON", help = "Phase runtime definition JSON payload.")]
    pub(crate) input_json: String,
}

#[derive(Debug, Args)]
pub(crate) struct WorkflowPhaseRemoveArgs {
    #[arg(long, value_name = "PHASE_ID", help = "Phase identifier.")]
    pub(crate) phase: String,
    #[arg(long, value_name = "PHASE_ID", help = "Confirmation token; must match --phase.")]
    pub(crate) confirm: Option<String>,
    #[arg(long, default_value_t = false, help = "Preview phase removal impact without mutating workflow config.")]
    pub(crate) dry_run: bool,
}

#[derive(Debug, Args)]
pub(crate) struct WorkflowDefinitionUpsertArgs {
    #[arg(long, value_name = "JSON", help = "Workflow definition JSON payload.")]
    pub(crate) input_json: String,
}

#[derive(Debug, Args)]
pub(crate) struct WorkflowStateMachineSetArgs {
    #[arg(long, value_name = "JSON", help = "Workflow state-machine configuration JSON payload.")]
    pub(crate) input_json: String,
}

#[derive(Debug, Args)]
pub(crate) struct WorkflowAgentRuntimeSetArgs {
    #[arg(long, value_name = "JSON", help = "Workflow agent-runtime configuration JSON payload.")]
    pub(crate) input_json: String,
}
