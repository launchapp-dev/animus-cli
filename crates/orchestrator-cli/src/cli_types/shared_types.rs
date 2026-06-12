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

/// Parse a duration spec like `30d`, `12h`, `45m`, `90s` into whole seconds.
/// A bare number (no unit suffix) is interpreted with `default_unit_secs`.
/// Shared by `--since`-style flags (default seconds) and age flags like
/// `workflow prune --older-than` (default days).
pub(crate) fn parse_duration_secs(value: &str, default_unit_secs: u64) -> Result<u64, String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err("must be non-empty".to_string());
    }
    let split = trimmed.find(|c: char| !c.is_ascii_digit()).unwrap_or(trimmed.len());
    let (digits, suffix) = trimmed.split_at(split);
    if digits.is_empty() {
        return Err("must start with a number (e.g. 30, 12h, 5m)".to_string());
    }
    let amount: u64 = digits.parse().map_err(|_| "is not a valid number".to_string())?;
    let unit_secs = match suffix {
        "" => default_unit_secs,
        "s" => 1,
        "m" => 60,
        "h" => 3_600,
        "d" => 86_400,
        other => return Err(format!("has unknown unit '{other}'; supported: s, m, h, d")),
    };
    Ok(amount.saturating_mul(unit_secs))
}

/// clap value parser for age flags: bare numbers mean days (back-compat with
/// the previous `--older-than <DAYS>` shape), unit suffixes s/m/h/d are
/// accepted (e.g. `30`, `30d`, `12h`).
pub(crate) fn parse_duration_secs_default_days(value: &str) -> Result<u64, String> {
    parse_duration_secs(value, 86_400)
}

/// clap value parser for `--since`-style window flags: bare numbers mean
/// seconds, unit suffixes s/m/h/d are accepted (e.g. `90`, `30m`, `7d`).
pub(crate) fn parse_duration_secs_default_seconds(value: &str) -> Result<u64, String> {
    parse_duration_secs(value, 1)
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
