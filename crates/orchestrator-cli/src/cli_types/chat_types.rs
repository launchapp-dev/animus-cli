use clap::{Args, Subcommand, ValueEnum};

use super::ReasoningEffortArg;

/// `animus chat` — hold multi-turn conversations with a provider tool.
///
/// Continuity is owned by the wrapped CLI tool's native session; Animus
/// stores a portable, queryable transcript and a thin continuity pointer
/// (`session_id` + `tool`). See `docs/reference/chat.md`.
#[derive(Debug, Subcommand)]
pub(crate) enum ChatCommand {
    /// Start a new (empty) conversation and print its id.
    New(ChatNewArgs),
    /// Send a user message to a conversation and stream the reply.
    Send(ChatSendArgs),
    /// Print a conversation's full transcript.
    Get(ChatGetArgs),
    /// List conversations, most-recently-updated first.
    List,
    /// Set or clear a conversation's title.
    Rename(ChatRenameArgs),
    /// Permanently delete a conversation.
    Delete(ChatDeleteArgs),
    /// Export a conversation transcript as Markdown or JSON.
    Export(ChatExportArgs),
    /// Search conversation transcripts across the scope.
    Search(ChatSearchArgs),
}

/// Output format for `animus chat export`.
#[derive(Debug, Clone, Copy, ValueEnum)]
pub(crate) enum ChatExportFormat {
    /// Human-readable Markdown transcript.
    Markdown,
    /// Full `{ meta, messages }` JSON (same shape as `chat get`).
    Json,
}

#[derive(Debug, Args)]
pub(crate) struct ChatNewArgs {
    /// Explicit conversation id. Omit to auto-generate (`conv-<uuid>`).
    #[arg(long, value_name = "ID")]
    pub(crate) id: Option<String>,
    /// Optional human-facing title.
    #[arg(long)]
    pub(crate) title: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct ChatSendArgs {
    /// Target conversation id. When omitted, a fresh conversation is created
    /// and its id is reported on the terminal frame.
    #[arg(long, value_name = "ID")]
    pub(crate) conversation: Option<String>,
    /// CLI provider to execute, for example claude, codex, or gemini.
    #[arg(long, default_value = "claude")]
    pub(crate) tool: String,
    /// Model identifier. Defaults to the tool's default model.
    #[arg(long)]
    pub(crate) model: Option<String>,
    /// The user message for this turn.
    #[arg(value_name = "MESSAGE")]
    pub(crate) message: String,
    /// Working directory for the provider process. Defaults to the project
    /// root; must stay inside it.
    #[arg(long, value_name = "PATH")]
    pub(crate) cwd: Option<String>,
    /// Stream assistant output incrementally as it arrives.
    #[arg(long)]
    pub(crate) stream: bool,
    /// Reasoning/thinking effort for the provider: low, medium, or high.
    #[arg(long, value_enum, value_name = "LEVEL")]
    pub(crate) reasoning_effort: Option<ReasoningEffortArg>,
    /// Provider permission/approval mode, forwarded verbatim (claude:
    /// default|acceptEdits|bypassPermissions|plan; codex:
    /// untrusted|on-failure|on-request|never; gemini:
    /// default|auto_edit|yolo). Overrides any configured agent-profile value.
    #[arg(long, value_name = "MODE")]
    pub(crate) permission_mode: Option<String>,
    /// Enable kernel-mediated approvals: sets `extras.approvals` on the
    /// session request so transports route permission decisions through
    /// `animus.agent.request_approval`. Implied when the selected `--agent`
    /// profile declares an `approval_policy`.
    #[arg(long, default_value_t = false)]
    pub(crate) approvals: bool,
    /// Title for the conversation. Names a freshly-created conversation, or
    /// renames the target one. An empty string clears the title.
    #[arg(long)]
    pub(crate) title: Option<String>,
    /// Agent profile whose declared MCP servers this chat agent receives.
    /// The profile's `mcp_servers` names are resolved against the project's
    /// `mcp_servers` map.
    #[arg(long, value_name = "AGENT_ID")]
    pub(crate) agent: Option<String>,
    /// Skill whose declared MCP servers are added to this chat agent's set
    /// (unioned with the profile's, when an `--agent` is also selected).
    #[arg(long, value_name = "SKILL")]
    pub(crate) skill: Option<String>,
    /// Additional MCP server to wire by name (repeatable). Each name is
    /// looked up in the project's `mcp_servers` map; `animus` selects the
    /// built-in stdio surface.
    #[arg(long = "mcp-server", value_name = "NAME")]
    pub(crate) mcp_server: Vec<String>,
    /// Drop the built-in `animus` MCP server from the resolved set.
    #[arg(long)]
    pub(crate) no_animus_mcp: bool,
}

#[derive(Debug, Args)]
pub(crate) struct ChatGetArgs {
    /// Conversation id to read.
    #[arg(value_name = "ID")]
    pub(crate) id: String,
}

#[derive(Debug, Args)]
pub(crate) struct ChatRenameArgs {
    /// Conversation id to rename.
    #[arg(value_name = "ID")]
    pub(crate) id: String,
    /// New title. Pass an empty string to clear it.
    #[arg(long)]
    pub(crate) title: String,
}

#[derive(Debug, Args)]
pub(crate) struct ChatDeleteArgs {
    /// Conversation id to delete.
    #[arg(value_name = "ID")]
    pub(crate) id: String,
}

#[derive(Debug, Args)]
pub(crate) struct ChatExportArgs {
    /// Conversation id to export.
    #[arg(value_name = "ID")]
    pub(crate) id: String,
    /// Output format.
    #[arg(long, value_enum, default_value = "markdown")]
    pub(crate) format: ChatExportFormat,
    /// Write to this file instead of stdout.
    #[arg(long, value_name = "PATH")]
    pub(crate) output: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct ChatSearchArgs {
    /// Text to find in conversation transcripts.
    #[arg(value_name = "QUERY")]
    pub(crate) query: String,
    /// Maximum number of matches to return.
    #[arg(long, default_value_t = 20)]
    pub(crate) limit: usize,
    /// Match case-sensitively (default is case-insensitive).
    #[arg(long)]
    pub(crate) case_sensitive: bool,
}
