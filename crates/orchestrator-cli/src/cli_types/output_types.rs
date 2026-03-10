use clap::{Args, Subcommand};

#[derive(Debug, Subcommand)]
pub(crate) enum OutputCommand {
    /// Read run event payloads.
    Run(OutputRunArgs),
    /// Read persisted workflow phase outputs.
    PhaseOutputs(OutputPhaseOutputsArgs),
    /// List artifacts for an execution id.
    Artifacts(OutputArtifactsArgs),
    /// Download an artifact payload.
    Download(OutputDownloadArgs),
    /// List artifact file ids for an execution.
    Files(OutputFilesArgs),
    /// Read aggregated JSONL output streams for a run.
    Jsonl(OutputJsonlArgs),
    /// Inspect run output with optional task/phase filtering.
    Monitor(OutputMonitorArgs),
    /// Infer CLI provider details from run output.
    Cli(OutputCliArgs),
}

#[derive(Debug, Args)]
pub(crate) struct OutputRunArgs {
    #[arg(long)]
    pub(crate) run_id: String,
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
pub(crate) struct OutputFilesArgs {
    #[arg(long)]
    pub(crate) execution_id: String,
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
