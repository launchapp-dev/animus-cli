use clap::{Args, ValueEnum};

pub(crate) const INPUT_JSON_PRECEDENCE_HELP: &str =
    "JSON payload for this command. When provided, values in this payload override individual CLI flags.";
pub(crate) const TASK_TYPE_HELP: &str = "Task type: feature|bugfix|hotfix|refactor|docs|test|chore|experiment.";
pub(crate) const TASK_TYPE_FILTER_HELP: &str =
    "Task type filter: feature|bugfix|hotfix|refactor|docs|test|chore|experiment.";
pub(crate) const TASK_STATUS_HELP: &str =
    "Task status: backlog|todo|ready|in-progress|in_progress|blocked|on-hold|on_hold|done|cancelled.";
pub(crate) const TASK_STATUS_FILTER_HELP: &str =
    "Status filter: backlog|todo|ready|in-progress|in_progress|blocked|on-hold|on_hold|done|cancelled.";
pub(crate) const TASK_PRIORITY_HELP: &str = "Task priority: critical|high|medium|low.";
pub(crate) const TASK_PRIORITY_FILTER_HELP: &str = "Priority filter: critical|high|medium|low.";
pub(crate) const REQUIREMENT_PRIORITY_HELP: &str = "Requirement priority: must|should|could|wont|won't.";
pub(crate) const REQUIREMENT_STATUS_HELP: &str =
    "Requirement status: draft|refined|planned|in-progress|in_progress|done.";
pub(crate) const REQUIREMENT_CATEGORY_HELP: &str =
    "Requirement category: documentation|usability|runtime|integration|quality|release|security.";
pub(crate) const REQUIREMENT_TYPE_HELP: &str =
    "Requirement type: product|functional|non-functional|nonfunctional|non_functional|technical|other.";
pub(crate) const TASK_RISK_FILTER_HELP: &str = "Risk filter: high|medium|low.";
pub(crate) const DEPENDENCY_TYPE_HELP: &str =
    "Dependency type: blocks-by|blocks_by|blocked-by|blocked_by|related-to|related_to.";

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

pub(crate) fn parse_percentage_u8(value: &str) -> Result<u8, String> {
    let parsed = value.parse::<u16>().map_err(|_| "must be a whole number".to_string())?;
    if parsed > 100 {
        return Err("must be between 0 and 100".to_string());
    }
    Ok(parsed as u8)
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
