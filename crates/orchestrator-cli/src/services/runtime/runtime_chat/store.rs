//! Conversation store — Animus's **portable + queryable + fallback** layer
//! for chat.
//!
//! ## What this store IS (and isn't)
//!
//! Per the v0.5.10 continuity model, **the provider owns live-session
//! continuity, not Animus.** The wrapped CLI tools (claude / codex / gemini)
//! each persist their own native session (e.g. Claude Code's
//! `~/.claude/projects/<proj>/<session-id>.jsonl`), keyed by the
//! `session_id` that flows out on [`SessionRun`] and back in on
//! `SessionRequest.extras.session_id`. When that native session is alive,
//! Animus passes the stored `session_id` and a prompt containing ONLY the
//! new user turn — the tool replays its own history. Animus does NOT
//! re-render prior turns into the prompt in that case.
//!
//! So this store is deliberately NOT the live-session replay engine. It is:
//!
//! * a **portable, provider-agnostic record** of every turn (normalized
//!   [`ChatMessage`]s) that `animus chat get` / `chat list` read, that
//!   `send --stream --json` mirrors, and that `animus cost --conversation`
//!   aggregates;
//! * the **resume fallback**: when no native session is alive (brand-new
//!   conversation, provider returned no `session_id`, a resume attempt
//!   failed, or the tool changed mid-conversation), the loop replays this
//!   stored history into the prompt — the ONLY case where Animus replays;
//! * the **continuity pointer**: [`ConversationMeta`] holds the current
//!   `session_id` + `tool` + `model`, captured from `SessionRun.session_id`
//!   after every turn so the next turn can resume.
//!
//! [`SessionRun`]: animus_session_backend::session::SessionRun

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};

/// Role of a persisted chat turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum ChatRole {
    User,
    Assistant,
}

/// One step in an assistant turn's timeline — text, thinking, a tool call, or
/// a tool result — persisted in arrival order. This lets a reloaded
/// conversation reconstruct the same interleaved view the live stream showed
/// (prose AND tool activity), rather than only the final aggregated text.
/// Serialized with an internal `kind` tag.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum TurnBlock {
    Text {
        text: String,
    },
    Thinking,
    ToolCall {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_name: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        arguments: Option<serde_json::Value>,
    },
    ToolResult {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_name: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        success: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output: Option<serde_json::Value>,
    },
}

/// A normalized, provider-agnostic record of a single turn in a
/// conversation. This is the portable artifact — it does NOT depend on any
/// one provider's native transcript format, so downstream apps and
/// `animus chat get` read a stable shape regardless of which tool produced
/// the turn.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ChatMessage {
    /// Monotonic 0-based index within the conversation.
    pub seq: u64,
    /// Who produced this turn.
    pub role: ChatRole,
    /// Aggregated text content of the turn.
    pub content: String,
    /// RFC 3339 timestamp when the turn was recorded.
    pub recorded_at: String,
    /// Provider tool that produced an assistant turn (`None` for user turns).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    /// Model that produced an assistant turn (`None` for user turns).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Token usage reported by the provider for an assistant turn, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<protocol::TokenUsage>,
    /// Provider-reported USD cost for an assistant turn, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
    /// Ordered timeline of the assistant turn (text / thinking / tool calls /
    /// results). Empty for user turns and for assistant turns persisted before
    /// this field existed — readers fall back to `content` in that case.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocks: Vec<TurnBlock>,
}

/// Conversation metadata — the thin continuity pointer plus identity.
///
/// `session_id` is the load-bearing field: it is the wrapped tool's native
/// session handle, captured from `SessionRun.session_id` after each turn.
/// The next turn hands it back via `SessionRequest.extras.session_id` so the
/// tool resumes its own session with full history intact. `tool` is stored
/// alongside it because a `session_id` is only valid for the tool that
/// issued it — switching tools mid-conversation invalidates the pointer and
/// forces a full-history replay.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ConversationMeta {
    /// Stable conversation id (also the on-disk directory name).
    pub id: String,
    /// Wrapped tool that currently owns the native session
    /// (`session_id`). `None` until the first turn completes.
    #[serde(default)]
    pub tool: Option<String>,
    /// Model used on the most recent turn.
    #[serde(default)]
    pub model: Option<String>,
    /// The wrapped tool's native session handle. `Some` once the provider
    /// has returned one; `None` for a brand-new conversation or after a
    /// resume failure cleared it. The next turn resumes when this is `Some`
    /// AND `tool` is unchanged.
    #[serde(default)]
    pub session_id: Option<String>,
    /// Optional human-facing title.
    #[serde(default)]
    pub title: Option<String>,
    /// RFC 3339 creation timestamp.
    pub created_at: String,
    /// RFC 3339 timestamp of the most recent turn.
    pub updated_at: String,
    /// Count of persisted turns (user + assistant).
    #[serde(default)]
    pub message_count: u64,
}

impl ConversationMeta {
    fn new(id: String) -> Self {
        let now = now_rfc3339();
        Self {
            id,
            tool: None,
            model: None,
            session_id: None,
            title: None,
            created_at: now.clone(),
            updated_at: now,
            message_count: 0,
        }
    }
}

/// One-line summary used by `animus chat list`.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct ConversationSummary {
    pub id: String,
    pub title: Option<String>,
    pub tool: Option<String>,
    pub model: Option<String>,
    pub message_count: u64,
    pub updated_at: String,
}

impl From<&ConversationMeta> for ConversationSummary {
    fn from(meta: &ConversationMeta) -> Self {
        Self {
            id: meta.id.clone(),
            title: meta.title.clone(),
            tool: meta.tool.clone(),
            model: meta.model.clone(),
            message_count: meta.message_count,
            updated_at: meta.updated_at.clone(),
        }
    }
}

/// Abstract conversation persistence. Production uses
/// [`FileConversationStore`]; tests use an in-memory mock so the turn loop
/// can be exercised without touching the filesystem.
pub(crate) trait ConversationStore: Send + Sync {
    /// Create a fresh conversation and return its meta. The store assigns
    /// the id when `id` is `None`.
    fn create(&self, id: Option<String>) -> Result<ConversationMeta>;
    /// Load conversation meta, or `None` when it does not exist.
    fn load_meta(&self, id: &str) -> Result<Option<ConversationMeta>>;
    /// Persist updated meta (continuity pointer, counts, timestamps).
    fn save_meta(&self, meta: &ConversationMeta) -> Result<()>;
    /// Append a turn to the conversation's event log.
    fn append_message(&self, id: &str, message: &ChatMessage) -> Result<()>;
    /// Read the full ordered turn history. Used for `chat get` and for the
    /// full-history fallback replay.
    fn load_messages(&self, id: &str) -> Result<Vec<ChatMessage>>;
    /// List all conversations, most-recently-updated first.
    fn list(&self) -> Result<Vec<ConversationSummary>>;
}

/// Filesystem-backed store rooted under the scoped runtime root
/// (`~/.animus/<repo-scope>/chat/<conversation-id>/`).
///
/// Layout per conversation:
/// * `meta.json` — [`ConversationMeta`] (the continuity pointer).
/// * `messages.jsonl` — append-only [`ChatMessage`] event log.
pub(crate) struct FileConversationStore {
    root: PathBuf,
}

impl FileConversationStore {
    /// Build a store rooted at `<scoped-state-root>/chat` for the project.
    pub(crate) fn for_project(project_root: &Path) -> Result<Self> {
        let scoped = protocol::scoped_state_root(project_root)
            .ok_or_else(|| anyhow!("could not resolve scoped runtime root for chat storage"))?;
        Ok(Self { root: scoped.join("chat") })
    }

    /// Build a store rooted at an explicit directory. Test-only escape hatch
    /// so the turn loop can be exercised against a temp dir without resolving
    /// the scoped runtime root.
    #[cfg(test)]
    pub(crate) fn with_root_for_test(root: PathBuf) -> Self {
        Self { root }
    }

    fn conversation_dir(&self, id: &str) -> PathBuf {
        self.root.join(id)
    }

    fn meta_path(&self, id: &str) -> PathBuf {
        self.conversation_dir(id).join("meta.json")
    }

    fn messages_path(&self, id: &str) -> PathBuf {
        self.conversation_dir(id).join("messages.jsonl")
    }
}

impl ConversationStore for FileConversationStore {
    fn create(&self, id: Option<String>) -> Result<ConversationMeta> {
        let id = id.unwrap_or_else(generate_conversation_id);
        ensure_safe_id(&id)?;
        let dir = self.conversation_dir(&id);
        if self.meta_path(&id).exists() {
            return Err(anyhow!("conversation '{id}' already exists"));
        }
        std::fs::create_dir_all(&dir).with_context(|| format!("creating conversation dir {}", dir.display()))?;
        let meta = ConversationMeta::new(id);
        self.save_meta(&meta)?;
        Ok(meta)
    }

    fn load_meta(&self, id: &str) -> Result<Option<ConversationMeta>> {
        ensure_safe_id(id)?;
        let path = self.meta_path(id);
        if !path.exists() {
            return Ok(None);
        }
        let raw = std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
        let meta = serde_json::from_str(&raw).with_context(|| format!("parsing {}", path.display()))?;
        Ok(Some(meta))
    }

    fn save_meta(&self, meta: &ConversationMeta) -> Result<()> {
        ensure_safe_id(&meta.id)?;
        let dir = self.conversation_dir(&meta.id);
        std::fs::create_dir_all(&dir).with_context(|| format!("creating conversation dir {}", dir.display()))?;
        let path = self.meta_path(&meta.id);
        let body = serde_json::to_string_pretty(meta)?;
        write_atomic(&path, body.as_bytes())
    }

    fn append_message(&self, id: &str, message: &ChatMessage) -> Result<()> {
        ensure_safe_id(id)?;
        let dir = self.conversation_dir(id);
        std::fs::create_dir_all(&dir).with_context(|| format!("creating conversation dir {}", dir.display()))?;
        let path = self.messages_path(id);
        let line = serde_json::to_string(message)?;
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .with_context(|| format!("opening {}", path.display()))?;
        file.write_all(line.as_bytes())?;
        file.write_all(b"\n")?;
        Ok(())
    }

    fn load_messages(&self, id: &str) -> Result<Vec<ChatMessage>> {
        ensure_safe_id(id)?;
        let path = self.messages_path(id);
        if !path.exists() {
            return Ok(Vec::new());
        }
        let raw = std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
        let mut messages = Vec::new();
        for (lineno, line) in raw.lines().enumerate() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let message: ChatMessage = serde_json::from_str(trimmed)
                .with_context(|| format!("parsing {} line {}", path.display(), lineno + 1))?;
            messages.push(message);
        }
        Ok(messages)
    }

    fn list(&self) -> Result<Vec<ConversationSummary>> {
        if !self.root.exists() {
            return Ok(Vec::new());
        }
        let mut summaries = Vec::new();
        for entry in std::fs::read_dir(&self.root).with_context(|| format!("reading {}", self.root.display()))? {
            let entry = entry?;
            if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                continue;
            }
            let Some(id) = entry.file_name().to_str().map(ToOwned::to_owned) else {
                continue;
            };
            if let Some(meta) = self.load_meta(&id)? {
                summaries.push(ConversationSummary::from(&meta));
            }
        }
        summaries.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        Ok(summaries)
    }
}

/// Render the conversation's stored history into a single prompt string for
/// the full-history fallback (case 2 of the continuity model). Only invoked
/// when no native session is alive.
pub(crate) fn render_history_prompt(messages: &[ChatMessage], new_user_turn: &str) -> String {
    let mut out = String::new();
    for message in messages {
        let label = match message.role {
            ChatRole::User => "User",
            ChatRole::Assistant => "Assistant",
        };
        out.push_str(label);
        out.push_str(": ");
        out.push_str(&message.content);
        out.push_str("\n\n");
    }
    out.push_str("User: ");
    out.push_str(new_user_turn);
    out
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, bytes).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("renaming into {}", path.display()))?;
    Ok(())
}

fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339()
}

fn generate_conversation_id() -> String {
    format!("conv-{}", uuid::Uuid::new_v4().simple())
}

/// Reject an unsafe conversation id before it is joined into a filesystem
/// path. Guards every read/append/save path — not just `create` — so an id
/// arriving from `chat get`, `chat send --conversation`, or
/// `cost conversation` can never escape the scoped chat directory via `../`.
fn ensure_safe_id(id: &str) -> Result<()> {
    if is_safe_id(id) {
        Ok(())
    } else {
        Err(anyhow!("invalid conversation id '{id}': use letters, digits, '-', '_' only"))
    }
}

/// Conversation ids become directory names, so reject path separators and
/// other surprises. Allow only a conservative identifier alphabet.
fn is_safe_id(id: &str) -> bool {
    !id.is_empty() && id != "." && id != ".." && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_history_prompt_includes_prior_turns_and_new_user_turn() {
        let messages = vec![
            ChatMessage {
                seq: 0,
                role: ChatRole::User,
                content: "hello".into(),
                recorded_at: now_rfc3339(),
                tool: None,
                model: None,
                usage: None,
                cost_usd: None,
                blocks: Vec::new(),
            },
            ChatMessage {
                seq: 1,
                role: ChatRole::Assistant,
                content: "hi there".into(),
                recorded_at: now_rfc3339(),
                tool: Some("claude".into()),
                model: Some("claude-sonnet-4-6".into()),
                usage: None,
                cost_usd: None,
                blocks: Vec::new(),
            },
        ];
        let prompt = render_history_prompt(&messages, "how are you?");
        assert!(prompt.contains("User: hello"), "prompt: {prompt}");
        assert!(prompt.contains("Assistant: hi there"), "prompt: {prompt}");
        assert!(prompt.trim_end().ends_with("User: how are you?"), "prompt: {prompt}");
    }

    #[test]
    fn is_safe_id_rejects_path_traversal() {
        assert!(is_safe_id("conv-abc123"));
        assert!(is_safe_id("my_conversation-1"));
        assert!(!is_safe_id(""));
        assert!(!is_safe_id(".."));
        assert!(!is_safe_id("../etc"));
        assert!(!is_safe_id("a/b"));
        assert!(!is_safe_id("a b"));
    }

    #[test]
    fn file_store_roundtrips_meta_and_messages() {
        let tmp = tempfile::tempdir().unwrap();
        let store = FileConversationStore { root: tmp.path().join("chat") };
        let mut meta = store.create(Some("conv-test".into())).unwrap();
        assert_eq!(meta.id, "conv-test");
        assert!(meta.session_id.is_none());

        // Capture a continuity pointer like the loop does after a turn.
        meta.session_id = Some("sess-1".into());
        meta.tool = Some("claude".into());
        meta.model = Some("claude-sonnet-4-6".into());
        meta.message_count = 2;
        store.save_meta(&meta).unwrap();

        let loaded = store.load_meta("conv-test").unwrap().unwrap();
        assert_eq!(loaded.session_id.as_deref(), Some("sess-1"));
        assert_eq!(loaded.tool.as_deref(), Some("claude"));

        store
            .append_message(
                "conv-test",
                &ChatMessage {
                    seq: 0,
                    role: ChatRole::User,
                    content: "q".into(),
                    recorded_at: now_rfc3339(),
                    tool: None,
                    model: None,
                    usage: None,
                    cost_usd: None,
                    blocks: Vec::new(),
                },
            )
            .unwrap();
        let messages = store.load_messages("conv-test").unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].content, "q");
    }

    #[test]
    fn assistant_blocks_round_trip_through_the_store() {
        let tmp = tempfile::tempdir().unwrap();
        let store = FileConversationStore { root: tmp.path().join("chat") };
        store.create(Some("conv-blocks".into())).unwrap();
        let msg = ChatMessage {
            seq: 1,
            role: ChatRole::Assistant,
            content: "done".into(),
            recorded_at: now_rfc3339(),
            tool: Some("codex".into()),
            model: Some("gpt-5.5".into()),
            usage: None,
            cost_usd: None,
            blocks: vec![
                TurnBlock::Text { text: "looking".into() },
                TurnBlock::ToolCall {
                    tool_name: Some("Read".into()),
                    arguments: Some(serde_json::json!({ "path": "a.ts" })),
                },
                TurnBlock::ToolResult {
                    tool_name: Some("Read".into()),
                    success: Some(true),
                    output: Some(serde_json::json!("contents")),
                },
                TurnBlock::Text { text: "done".into() },
            ],
        };
        store.append_message("conv-blocks", &msg).unwrap();
        let loaded = store.load_messages("conv-blocks").unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].blocks, msg.blocks, "blocks must survive a save/load round-trip");
    }

    #[test]
    fn legacy_messages_without_blocks_default_to_empty() {
        // A pre-existing messages.jsonl line lacks the `blocks` field entirely.
        let legacy = r#"{"seq":1,"role":"assistant","content":"hi","recorded_at":"2026-06-08T00:00:00Z"}"#;
        let msg: ChatMessage = serde_json::from_str(legacy).expect("legacy line must still parse");
        assert!(msg.blocks.is_empty(), "missing blocks must default to empty, not error");
    }

    #[test]
    fn create_rejects_duplicate_and_unsafe_ids() {
        let tmp = tempfile::tempdir().unwrap();
        let store = FileConversationStore { root: tmp.path().join("chat") };
        store.create(Some("dup".into())).unwrap();
        assert!(store.create(Some("dup".into())).is_err());
        assert!(store.create(Some("../escape".into())).is_err());
    }

    #[test]
    fn read_and_append_paths_reject_unsafe_ids() {
        let tmp = tempfile::tempdir().unwrap();
        let store = FileConversationStore { root: tmp.path().join("chat") };
        assert!(store.load_meta("../escape").is_err(), "load_meta must reject traversal ids");
        assert!(store.load_messages("../escape").is_err(), "load_messages must reject traversal ids");
        assert!(
            store
                .append_message(
                    "a/b",
                    &ChatMessage {
                        seq: 0,
                        role: ChatRole::User,
                        content: "x".into(),
                        recorded_at: now_rfc3339(),
                        tool: None,
                        model: None,
                        usage: None,
                        cost_usd: None,
                        blocks: Vec::new(),
                    },
                )
                .is_err(),
            "append_message must reject path-separator ids"
        );
    }

    #[test]
    fn list_sorts_by_updated_at_desc() {
        let tmp = tempfile::tempdir().unwrap();
        let store = FileConversationStore { root: tmp.path().join("chat") };
        let mut a = store.create(Some("a".into())).unwrap();
        let mut b = store.create(Some("b".into())).unwrap();
        a.updated_at = "2026-01-01T00:00:00Z".into();
        b.updated_at = "2026-06-01T00:00:00Z".into();
        store.save_meta(&a).unwrap();
        store.save_meta(&b).unwrap();
        let list = store.list().unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].id, "b");
        assert_eq!(list[1].id, "a");
    }
}
