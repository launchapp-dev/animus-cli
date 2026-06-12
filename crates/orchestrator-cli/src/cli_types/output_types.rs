use clap::{Args, Subcommand};

#[derive(Debug, Subcommand)]
pub(crate) enum OutputCommand {
    /// Read run event payloads.
    Read(OutputRunArgs),
    /// Read persisted workflow phase outputs.
    PhaseOutputs(OutputPhaseOutputsArgs),
    /// List artifacts for an execution id.
    Artifacts(OutputArtifactsArgs),
    /// Download an artifact payload.
    Download(OutputDownloadArgs),
    /// Read aggregated JSONL output streams for a run.
    Jsonl(OutputJsonlArgs),
    /// Inspect run output with optional task/phase filtering.
    Monitor(OutputMonitorArgs),
    /// Infer CLI provider details from run output.
    Cli(OutputCliArgs),
    /// Read the per-run LLM decision log (decisions.jsonl).
    Decisions(OutputDecisionsArgs),
}

#[derive(Debug, Args)]
pub(crate) struct OutputRunArgs {
    /// Run id to read. Mutually exclusive with --workflow-id.
    #[arg(long, required_unless_present = "workflow_id", conflicts_with = "workflow_id")]
    pub(crate) run_id: Option<String>,
    /// Resolve the latest run id recorded for this workflow, then read it.
    #[arg(long)]
    pub(crate) workflow_id: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct OutputDecisionsArgs {
    /// Run id whose decision log to read. Mutually exclusive with --workflow-id.
    #[arg(long, required_unless_present = "workflow_id", conflicts_with = "workflow_id")]
    pub(crate) run_id: Option<String>,
    /// Resolve the latest run id recorded for this workflow, then read its decision log.
    #[arg(long)]
    pub(crate) workflow_id: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct OutputPhaseOutputsArgs {
    #[arg(long)]
    pub(crate) workflow_id: String,
    #[arg(long)]
    pub(crate) phase_id: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct OutputArtifactsArgs {
    #[arg(long)]
    pub(crate) execution_id: String,
}

#[derive(Debug, Args)]
pub(crate) struct OutputDownloadArgs {
    #[arg(long)]
    pub(crate) execution_id: String,
    #[arg(long)]
    pub(crate) artifact_id: String,
}

#[derive(Debug, Args)]
pub(crate) struct OutputJsonlArgs {
    #[arg(long)]
    pub(crate) run_id: String,
    #[arg(long, default_value_t = false)]
    pub(crate) entries: bool,
}

#[derive(Debug, Args)]
pub(crate) struct OutputMonitorArgs {
    #[arg(long)]
    pub(crate) run_id: String,
    #[arg(long)]
    pub(crate) task_id: Option<String>,
    #[arg(long)]
    pub(crate) phase_id: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct OutputCliArgs {
    #[arg(long)]
    pub(crate) run_id: String,
}
