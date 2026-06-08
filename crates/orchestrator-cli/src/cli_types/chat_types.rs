use clap::{Args, Subcommand};

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
}

#[derive(Debug, Args)]
pub(crate) struct ChatGetArgs {
    /// Conversation id to read.
    #[arg(value_name = "ID")]
    pub(crate) id: String,
}
