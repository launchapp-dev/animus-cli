use clap::{Args, Subcommand, ValueEnum};

use super::{ReasoningEffortArg, ACTOR_JSON_HELP};

/// `animus chat` — hold multi-turn conversations with a provider tool.
///
/// Continuity is owned by the wrapped CLI tool's native session; Animus
/// stores a portable, queryable transcript and a thin continuity pointer
/// (`session_id` + `tool`). See `docs/reference/chat.md`.
// `Send` carries the rich per-turn flag set (provider/model/mcp/actor/...), so
// it dwarfs the other variants. Boxing it would break clap's `Subcommand`
// derive (a tuple-variant field must itself be `Args`); accept the size delta.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Subcommand)]
pub(crate) enum ChatCommand {
    /// Start a new (empty) conversation and print its id.
    New(ChatNewArgs),
    /// Send a user message to a conversation and stream the reply.
    Send(ChatSendArgs),
    /// Print a conversation's full transcript.
    Get(ChatGetArgs),
    /// List conversations, most-recently-updated first.
    List(ChatListArgs),
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

/// Conversation visibility for `animus chat new`.
#[derive(Debug, Clone, Copy, ValueEnum)]
pub(crate) enum ChatVisibilityArg {
    /// Visible only to the owner (and to admin/unscoped listings). Default.
    Private,
    /// Visible to every user, in addition to the owner.
    Shared,
}

#[derive(Debug, Args)]
pub(crate) struct ChatNewArgs {
    /// Explicit conversation id. Omit to auto-generate (`conv-<uuid>`).
    #[arg(long, value_name = "ID")]
    pub(crate) id: Option<String>,
    /// Optional human-facing title.
    #[arg(long)]
    pub(crate) title: Option<String>,
    /// Owner (authenticated user id) to stamp onto the conversation. Used by a
    /// `conversation_store` plugin for per-user history; advisory for the
    /// in-tree filesystem store.
    #[arg(long, value_name = "USER_ID")]
    pub(crate) as_user: Option<String>,
    /// Conversation visibility: private (owner-only) or shared.
    #[arg(long, value_enum, default_value = "private")]
    pub(crate) visibility: ChatVisibilityArg,
}

#[derive(Debug, Args)]
pub(crate) struct ChatListArgs {
    /// Limit the listing to conversations owned by this user id PLUS any
    /// shared ones. Omit for the full (legacy/admin) listing.
    #[arg(long, value_name = "USER_ID")]
    pub(crate) as_user: Option<String>,
    /// Maximum number of conversations to return after ownership filtering.
    #[arg(long, value_name = "N")]
    pub(crate) limit: Option<usize>,
    /// Number of newest conversations to skip after ownership filtering.
    #[arg(long, default_value_t = 0)]
    pub(crate) offset: usize,
}

#[derive(Debug, Args)]
pub(crate) struct ChatSendArgs {
    /// Target conversation id. When omitted, a fresh conversation is created
    /// and its id is reported on the terminal frame.
    #[arg(long, value_name = "ID")]
    pub(crate) conversation: Option<String>,
    /// Fail unless the conversation still has this revision when the turn
    /// lock is acquired. Application layers should pass the revision returned
    /// by `chat get` to close preflight-to-mutation races.
    #[arg(long, value_name = "N", requires = "conversation")]
    pub(crate) expected_revision: Option<u64>,
    /// CLI provider to execute, for example claude, codex, or gemini. Defaults
    /// to the bound agent profile's tool, then claude for an unbound chat.
    #[arg(long)]
    pub(crate) tool: Option<String>,
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
    /// Owner (authenticated user id) stamped onto a conversation created by
    /// this send (when `--conversation` is omitted). Advisory for the in-tree
    /// store; used by a `conversation_store` plugin for per-user history.
    #[arg(long, value_name = "USER_ID")]
    pub(crate) as_user: Option<String>,
    /// Visibility for a conversation created by this send.
    #[arg(long, value_enum, default_value = "private")]
    pub(crate) visibility: ChatVisibilityArg,
    /// Transport-asserted authz identity for this chat turn (distinct from
    /// `--as-user`, which is the conversation-ownership stamp). Binds the
    /// chat agent's built-in `animus` MCP server to this user so its
    /// per-user subject / queue / integration tools are scoped accordingly.
    #[arg(long, value_name = "JSON", help = ACTOR_JSON_HELP)]
    pub(crate) actor_json: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct ChatGetArgs {
    /// Conversation id to read.
    #[arg(value_name = "ID")]
    pub(crate) id: String,
    /// Acting user id. Advisory for the in-tree store; a `conversation_store`
    /// plugin may use it to enforce read access.
    #[arg(long, value_name = "USER_ID")]
    pub(crate) as_user: Option<String>,
    /// Maximum number of transcript messages to return.
    #[arg(long, value_name = "N")]
    pub(crate) limit: Option<usize>,
    /// Number of transcript messages to skip from the start.
    #[arg(long, default_value_t = 0)]
    pub(crate) offset: usize,
}

#[derive(Debug, Args)]
pub(crate) struct ChatRenameArgs {
    /// Conversation id to rename.
    #[arg(value_name = "ID")]
    pub(crate) id: String,
    /// New title. Pass an empty string to clear it.
    #[arg(long)]
    pub(crate) title: String,
    /// Acting user id. Advisory for the in-tree store; a `conversation_store`
    /// plugin may use it to enforce rename (mutation) access.
    #[arg(long, value_name = "USER_ID")]
    pub(crate) as_user: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct ChatDeleteArgs {
    /// Conversation id to delete.
    #[arg(value_name = "ID")]
    pub(crate) id: String,
    /// Acting user id. Advisory for the in-tree store; a `conversation_store`
    /// plugin may use it to enforce delete access.
    #[arg(long, value_name = "USER_ID")]
    pub(crate) as_user: Option<String>,
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
    /// Acting user id. Advisory for the in-tree store; a `conversation_store`
    /// plugin may use it to enforce read access on the exported transcript.
    #[arg(long, value_name = "USER_ID")]
    pub(crate) as_user: Option<String>,
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
    /// Limit the search to conversations owned by this user id PLUS any shared
    /// ones. Omit for the full (legacy/admin) search.
    #[arg(long, value_name = "USER_ID")]
    pub(crate) as_user: Option<String>,
}
