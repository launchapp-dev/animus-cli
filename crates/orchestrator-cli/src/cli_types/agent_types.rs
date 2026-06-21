use clap::{ArgAction, Args, Subcommand, ValueEnum};

use super::{parse_positive_u64, ReasoningEffortArg};

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
    /// Inspect and answer pending agent questions and approval requests.
    Interactions {
        #[command(subcommand)]
        command: AgentInteractionsCommand,
    },
    /// Resolve an approval decision for an external provider-CLI hook (gemini
    /// BeforeTool, opencode plugin, oai harness). Reads ONE tool call as JSON
    /// on stdin and routes it through the same approval logic that backs the
    /// MCP `animus.agent.request_approval` tool. Hidden: this is a machine
    /// integration point, not an interactive verb.
    #[command(hide = true)]
    ApproveHook(AgentApproveHookArgs),
}

/// Output format for `animus agent approve-hook`. Each provider's command hook
/// expects a different stdout shape, so the verb renders the resolved decision
/// in the requested contract.
#[derive(Clone, Debug, Default, ValueEnum)]
pub(crate) enum ApproveHookFormat {
    /// Gemini BeforeTool command-hook contract: stdin is
    /// `{ tool_name, tool_input, cwd?, session_id? }`; allow prints `{}` and
    /// deny prints `{"decision":"deny","reason":"..."}`. Any stray stdout makes
    /// gemini default to ALLOW, so only the decision JSON goes to stdout.
    Gemini,
    /// Claude PreToolUse command-hook contract: stdin is
    /// `{ tool_name, tool_input, session_id, cwd, ... }` (same input nesting as
    /// gemini's BeforeTool); both decisions emit
    /// `{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"allow"|"deny","permissionDecisionReason":"<reason>"}}`.
    /// An explicit `allow` auto-approves the tool call rather than falling
    /// through to claude's normal permission flow (mirrors the in-tree
    /// `animus-hook` PreToolUse contract).
    Claude,
    /// Generic contract for the opencode plugin / oai harness: stdin is
    /// `{ tool_name, input? }`; stdout is
    /// `{"decision":"allow"|"deny","reason":"...","updated_input"?:<json>}`.
    #[default]
    Generic,
}

#[derive(Debug, Args)]
pub(crate) struct AgentApproveHookArgs {
    #[arg(
        long,
        value_name = "AGENT_ID",
        help = "Agent profile whose approval_policy governs the decision. Required: the hook supplies no pinned identity."
    )]
    pub(crate) agent_id: String,
    #[arg(
        long,
        value_enum,
        value_name = "FORMAT",
        default_value_t = ApproveHookFormat::Generic,
        help = "stdin/stdout contract: claude (PreToolUse), gemini (BeforeTool), or generic (opencode/oai)."
    )]
    pub(crate) format: ApproveHookFormat,
    #[arg(long, value_name = "WORKFLOW_ID", help = "Optional workflow id context recorded on any escalation.")]
    pub(crate) workflow_id: Option<String>,
    #[arg(long, value_name = "TASK_ID", help = "Optional task id context recorded on any escalation.")]
    pub(crate) task_id: Option<String>,
    #[arg(
        long,
        value_name = "SECONDS",
        value_parser = parse_positive_u64,
        help = "Timeout in seconds for a human escalation (Ask policy). On timeout the decision fails closed (deny)."
    )]
    pub(crate) timeout_secs: Option<u64>,
}

#[derive(Debug, Subcommand)]
pub(crate) enum AgentInteractionsCommand {
    /// List pending interactions (use --all to include answered and expired).
    List(AgentInteractionsListArgs),
    /// Show a single interaction by id.
    Show(AgentInteractionsShowArgs),
    /// Answer a pending question or approval request.
    Answer(AgentInteractionsAnswerArgs),
}

#[derive(Debug, Args)]
pub(crate) struct AgentInteractionsListArgs {
    #[arg(long, default_value_t = false, help = "Include answered and expired interactions.")]
    pub(crate) all: bool,
    #[arg(long, value_name = "AGENT_ID", help = "Filter interactions by requesting agent id.")]
    pub(crate) agent: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct AgentInteractionsShowArgs {
    #[arg(value_name = "ID", help = "Interaction id.")]
    pub(crate) id: String,
}

#[derive(Debug, Args)]
pub(crate) struct AgentInteractionsAnswerArgs {
    #[arg(value_name = "ID", help = "Interaction id.")]
    pub(crate) id: String,
    #[arg(long, value_name = "TEXT", help = "Answer text for a question interaction.")]
    pub(crate) text: Option<String>,
    #[arg(long, default_value_t = false, conflicts_with_all = ["deny", "text"], help = "Approve an approval request.")]
    pub(crate) allow: bool,
    #[arg(long, default_value_t = false, conflicts_with = "text", help = "Deny an approval request.")]
    pub(crate) deny: bool,
    #[arg(long, value_name = "TEXT", help = "Optional message returned to the agent alongside the decision.")]
    pub(crate) message: Option<String>,
    #[arg(
        long,
        value_name = "QUESTION=LABEL",
        conflicts_with_all = ["allow", "deny"],
        help = "Answer a structured question: \"<question text|header|1-based index>=<label[,label...]>\". Repeat for multiple questions; comma-separate labels for multi-select."
    )]
    pub(crate) select: Vec<String>,
    #[arg(
        long,
        default_value_t = false,
        requires = "allow",
        help = "Echo the request's localSettings-destination permission suggestions back as updatedPermissions (allowed approvals only)."
    )]
    pub(crate) remember: bool,
    #[arg(
        long = "updated-input",
        value_name = "JSON",
        requires = "allow",
        help = "Operator-modified tool input (JSON) echoed as updatedInput on an allowed approval."
    )]
    pub(crate) updated_input: Option<String>,
    #[arg(long = "by", value_name = "NAME", help = "Who answered. Defaults to 'human'.")]
    pub(crate) answered_by: Option<String>,
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
    #[arg(
        long,
        default_value_t = false,
        help = "Enable kernel-mediated approvals: sets extras.approvals on the session request so transports route permission decisions through animus.agent.request_approval. Implied when the selected --agent profile declares an approval_policy."
    )]
    pub(crate) approvals: bool,
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
        hide = true,
        help = "Deprecated no-op (the agent-runner sidecar was removed in v0.5.3; provider plugins handle CLI invocation)."
    )]
    pub(crate) start_runner: bool,
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
    #[arg(
        long,
        default_value_t = false,
        hide = true,
        help = "Deprecated no-op (the agent-runner sidecar was removed in v0.5.3; provider plugins handle CLI invocation)."
    )]
    pub(crate) start_runner: bool,
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
    #[arg(
        long,
        default_value_t = false,
        hide = true,
        help = "Deprecated no-op (the agent-runner sidecar was removed in v0.5.3; provider plugins handle CLI invocation)."
    )]
    pub(crate) start_runner: bool,
}
