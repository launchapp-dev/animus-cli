use clap::{ArgAction, Args, Subcommand, ValueEnum};

use super::{parse_positive_u64, ReasoningEffortArg, RunnerScopeArg};

#[derive(Debug, Subcommand)]
pub(crate) enum AgentCommand {
    /// List configured agent profiles.
    List,
    /// Get a configured agent profile.
    Get(AgentGetArgs),
    /// Start an agent run.
    Run(AgentRunArgs),
    /// Control an existing agent run.
    Control(AgentControlArgs),
    /// Read status for a run id.
    Status(AgentStatusArgs),
    /// Read and update project-scoped agent memory.
    Memory {
        #[command(subcommand)]
        command: AgentMemoryCommand,
    },
    /// Send and inspect project-scoped agent messages.
    Message {
        #[command(subcommand)]
        command: AgentMessageCommand,
    },
}

#[derive(Debug, Args)]
pub(crate) struct AgentGetArgs {
    #[arg(long, value_name = "AGENT_ID", help = "Configured agent profile id.")]
    pub(crate) id: String,
}

#[derive(Debug, Subcommand)]
pub(crate) enum AgentMemoryCommand {
    /// Read memory for a configured agent.
    Get(AgentMemoryGetArgs),
    /// Append a memory entry for a configured agent.
    Append(AgentMemoryAppendArgs),
    /// Clear memory for a configured agent.
    Clear(AgentMemoryClearArgs),
}

#[derive(Debug, Args)]
pub(crate) struct AgentMemoryGetArgs {
    #[arg(long, value_name = "AGENT_ID", help = "Configured agent profile id.")]
    pub(crate) agent: String,
}

#[derive(Debug, Args)]
pub(crate) struct AgentMemoryAppendArgs {
    #[arg(long, value_name = "AGENT_ID", help = "Configured agent profile id.")]
    pub(crate) agent: String,
    #[arg(long, value_name = "TEXT", help = "Memory text to append.")]
    pub(crate) text: String,
    #[arg(long, value_name = "SOURCE", help = "Optional source label for the memory entry.")]
    pub(crate) source: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct AgentMemoryClearArgs {
    #[arg(long, value_name = "AGENT_ID", help = "Configured agent profile id.")]
    pub(crate) agent: String,
}

#[derive(Debug, Subcommand)]
pub(crate) enum AgentMessageCommand {
    /// Send a message on an agent channel.
    Send(AgentMessageSendArgs),
    /// List agent messages.
    List(AgentMessageListArgs),
}

#[derive(Debug, Args)]
pub(crate) struct AgentMessageSendArgs {
    #[arg(long, value_name = "CHANNEL", help = "Configured agent channel name.")]
    pub(crate) channel: String,
    #[arg(long, value_name = "AGENT_ID", help = "Sender agent profile id.")]
    pub(crate) from: String,
    #[arg(long, value_name = "AGENT_ID", help = "Optional recipient agent profile id.")]
    pub(crate) to: Option<String>,
    #[arg(long, value_name = "TEXT", help = "Message text.")]
    pub(crate) text: String,
    #[arg(long, value_name = "WORKFLOW_ID", help = "Optional workflow id context.")]
    pub(crate) workflow_id: Option<String>,
    #[arg(long, value_name = "PHASE_ID", help = "Optional phase id context.")]
    pub(crate) phase_id: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct AgentMessageListArgs {
    #[arg(long, value_name = "CHANNEL", help = "Filter messages by channel.")]
    pub(crate) channel: Option<String>,
    #[arg(long, value_name = "AGENT_ID", help = "Filter messages sent by or addressed to an agent.")]
    pub(crate) agent: Option<String>,
    #[arg(long, value_name = "COUNT", help = "Maximum messages to return.")]
    pub(crate) limit: Option<usize>,
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
    #[arg(
        long,
        value_enum,
        value_name = "LEVEL",
        help = "Reasoning/thinking effort for the provider: low, medium, or high. Overrides any configured value."
    )]
    pub(crate) reasoning_effort: Option<ReasoningEffortArg>,
    #[arg(
        long,
        value_name = "MODE",
        help = "Provider permission/approval mode, forwarded verbatim (claude: default|acceptEdits|bypassPermissions|plan; codex: untrusted|on-failure|on-request|never; gemini: default|auto_edit|yolo). Overrides any configured agent-profile value."
    )]
    pub(crate) permission_mode: Option<String>,
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
        help = "Persist run event logs under the scoped runtime directory."
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
    #[arg(
        long,
        value_name = "AGENT_ID",
        help = "Agent profile whose declared MCP servers this run receives (when no --runtime-contract-json is supplied)."
    )]
    pub(crate) agent: Option<String>,
    #[arg(
        long,
        value_name = "SKILL",
        help = "Skill whose declared MCP servers are added to this run's set (unioned with the profile's)."
    )]
    pub(crate) skill: Option<String>,
    #[arg(
        long = "mcp-server",
        value_name = "NAME",
        help = "Additional MCP server to wire by name (repeatable). 'animus' selects the built-in stdio surface."
    )]
    pub(crate) mcp_server: Vec<String>,
    #[arg(long, help = "Drop the built-in 'animus' MCP server from the resolved set.")]
    pub(crate) no_animus_mcp: bool,
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
