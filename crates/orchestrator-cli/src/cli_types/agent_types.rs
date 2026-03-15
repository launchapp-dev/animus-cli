use clap::{ArgAction, Args, Subcommand, ValueEnum};

use super::{parse_positive_u64, RunnerScopeArg};

#[derive(Debug, Subcommand)]
pub(crate) enum AgentCommand {
    /// Start an agent run.
    Run(AgentRunArgs),
    /// Control an existing agent run.
    Control(AgentControlArgs),
    /// Read status for a run id.
    Status(AgentStatusArgs),
}

#[derive(Debug, Args)]
pub(crate) struct AgentRunArgs {
    #[arg(long, value_name = "RUN_ID", help = "Run identifier. Omit to auto-generate a UUID.")]
    pub(crate) run_id: Option<String>,
    #[arg(
        long,
        value_name = "TOOL",
        default_value = "claude",
        help = "CLI provider to execute, for example claude, codex, or gemini."
    )]
    pub(crate) tool: String,
    #[arg(
        long,
        value_name = "MODEL",
        help = "Model identifier passed to the selected tool. Defaults to the configured model for the selected --tool."
    )]
    pub(crate) model: Option<String>,
    #[arg(long, value_name = "TEXT", help = "Prompt text to send to the agent.")]
    pub(crate) prompt: Option<String>,
    #[arg(long, value_name = "PATH", help = "Working directory for the run. Must resolve inside the project root.")]
    pub(crate) cwd: Option<String>,
    #[arg(
        long,
        value_name = "SECONDS",
        value_parser = parse_positive_u64,
        help = "Run timeout in seconds."
    )]
    pub(crate) timeout_secs: Option<u64>,
    #[arg(long, value_name = "JSON", help = "Agent context JSON object.")]
    pub(crate) context_json: Option<String>,
    #[arg(long, value_name = "JSON", help = "Runtime contract JSON override.")]
    pub(crate) runtime_contract_json: Option<String>,
    #[arg(long, default_value_t = false, help = "Submit run and return immediately without streaming events.")]
    pub(crate) detach: bool,
    #[arg(
        long,
        action = ArgAction::Set,
        default_value_t = true,
        help = "Stream run events to stdout."
    )]
    pub(crate) stream: bool,
    #[arg(
        long,
        action = ArgAction::Set,
        default_value_t = true,
        help = "Persist run event logs under .ao/runs."
    )]
    pub(crate) save_jsonl: bool,
    #[arg(long, value_name = "PATH", help = "Override the base directory used for persisted run logs.")]
    pub(crate) jsonl_dir: Option<String>,
    #[arg(
        long,
        action = ArgAction::Set,
        default_value_t = true,
        help = "Start the runner automatically when required."
    )]
    pub(crate) start_runner: bool,
    #[arg(long, value_enum, value_name = "SCOPE", help = "Runner config scope: project or global.")]
    pub(crate) runner_scope: Option<RunnerScopeArg>,
}

#[derive(Debug, Args)]
pub(crate) struct AgentControlArgs {
    #[arg(long, value_name = "RUN_ID", help = "Run identifier.")]
    pub(crate) run_id: String,
    #[arg(long, value_enum, value_name = "ACTION", help = "Control action: pause, resume, or terminate.")]
    pub(crate) action: AgentControlActionArg,
    #[arg(long, default_value_t = false, help = "Start the runner automatically when required.")]
    pub(crate) start_runner: bool,
    #[arg(long, value_enum, value_name = "SCOPE", help = "Runner config scope: project or global.")]
    pub(crate) runner_scope: Option<RunnerScopeArg>,
}

#[derive(Clone, Debug, ValueEnum)]
pub(crate) enum AgentControlActionArg {
    Pause,
    Resume,
    Terminate,
}

#[derive(Debug, Args)]
pub(crate) struct AgentStatusArgs {
    #[arg(long, value_name = "RUN_ID", help = "Run identifier.")]
    pub(crate) run_id: String,
    #[arg(long, value_name = "PATH", help = "Override the base directory used to read persisted run logs.")]
    pub(crate) jsonl_dir: Option<String>,
    #[arg(long, default_value_t = false, help = "Start the runner automatically when required.")]
    pub(crate) start_runner: bool,
    #[arg(long, value_enum, value_name = "SCOPE", help = "Runner config scope: project or global.")]
    pub(crate) runner_scope: Option<RunnerScopeArg>,
}
