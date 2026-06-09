use clap::{Args, ValueEnum};

pub(crate) const INPUT_JSON_PRECEDENCE_HELP: &str =
    "JSON payload for this command. When provided, values in this payload override individual CLI flags.";
pub(crate) const WORKFLOW_STATUS_HELP: &str =
    "Workflow status: pending|running|paused|completed|failed|escalated|cancelled.";
pub(crate) const WORKFLOW_SORT_HELP: &str = "Workflow sort: started-at|started_at|status|workflow-ref|workflow_ref|id.";

pub(crate) fn parse_positive_u64(value: &str) -> Result<u64, String> {
    let parsed = value.parse::<u64>().map_err(|_| "must be a whole number".to_string())?;
    if parsed == 0 {
        return Err("must be greater than 0".to_string());
    }
    Ok(parsed)
}

pub(crate) fn parse_positive_usize(value: &str) -> Result<usize, String> {
    let parsed = value.parse::<usize>().map_err(|_| "must be a whole number".to_string())?;
    if parsed == 0 {
        return Err("must be greater than 0".to_string());
    }
    Ok(parsed)
}

#[derive(Debug, Args)]
pub(crate) struct IdArgs {
    #[arg(short, long, value_name = "ID", help = "Entity identifier.")]
    pub(crate) id: String,
}

#[derive(Debug, Args)]
pub(crate) struct TaskIdArgs {
    #[arg(short, long, value_name = "TASK_ID", help = "Task identifier.")]
    pub(crate) task_id: String,
}

#[derive(Debug, Args)]
pub(crate) struct LogArgs {
    #[arg(
        long,
        value_name = "COUNT",
        value_parser = parse_positive_usize,
        help = "Maximum number of recent log lines to return."
    )]
    pub(crate) limit: Option<usize>,
    #[arg(long, help = "Filter log lines containing this search string.")]
    pub(crate) search: Option<String>,
}

#[derive(Clone, Debug, ValueEnum)]
pub(crate) enum RunnerScopeArg {
    Project,
    Global,
}

/// Reasoning / thinking effort level passed through to a provider CLI.
///
/// Threaded into the provider session request as `extras.reasoning_effort`;
/// each provider transport maps it to its own flag (codex
/// `-c model_reasoning_effort=...`, claude `--effort ...`). Omitting the
/// flag leaves each provider on its own default effort.
#[derive(Clone, Copy, Debug, ValueEnum)]
pub(crate) enum ReasoningEffortArg {
    Low,
    Medium,
    High,
}

impl ReasoningEffortArg {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            ReasoningEffortArg::Low => "low",
            ReasoningEffortArg::Medium => "medium",
            ReasoningEffortArg::High => "high",
        }
    }
}
