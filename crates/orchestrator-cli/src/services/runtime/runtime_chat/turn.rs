//! The chat turn loop and its continuity model.
//!
//! ## Continuity model (v0.5.10): providers own continuity
//!
//! Each turn is strictly **one** of two modes, never both — feeding history
//! twice (once into a resumed native session, once into a replayed prompt)
//! would double the wrapped tool's context and cost. That double-feed is the
//! bug this module is written to avoid.
//!
//! 1. **Session alive** — the conversation has a stored `session_id` from a
//!    prior turn *with the same tool*:
//!    * `prompt` = ONLY the new user message.
//!    * `extras.session_id` = the stored id. The wrapped CLI tool resumes
//!      its own native session, which already carries all prior context.
//!    * Animus does NOT replay history into the prompt.
//!
//! 2. **No live session** — brand-new conversation, OR the provider returned
//!    no `session_id`, OR a resume attempt failed, OR the tool changed
//!    mid-conversation:
//!    * `prompt` = full rendered history from Animus's stored messages.
//!    * no `extras.session_id`. This is the ONLY case where Animus replays.
//!
//! After every turn we capture `SessionRun.session_id` into conversation
//! meta so the next turn can resume. If a resume turn (mode 1) comes back
//! with a "session not found / invalid" error, we fall back to mode 2 (full
//! history) and retry exactly **once**.
//!
//! ## TurnProducer factoring
//!
//! [`run_turn`] does not call the [`SessionBackendResolver`] directly; it
//! drives a [`TurnProducer`]. Production wires
//! [`ResolverTurnProducer`], which resolves the provider plugin and starts a
//! session. v0.5.11's `chat_provider` plugin role will slot in as an
//! alternate `TurnProducer` without changing the continuity logic here, and
//! tests inject a scripted mock producer.

use std::path::PathBuf;
use std::sync::Arc;

use animus_session_backend::session::{SessionEvent, SessionRequest, SessionRun};
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use orchestrator_plugin_host::session::SessionBackendResolver;
use serde_json::{json, Value};

use super::sink::{ChatStreamEvent, ChatStreamSink};
use super::store::{render_history_prompt, ChatMessage, ChatRole, ConversationStore, TurnBlock};

/// Outcome of draining one provider session.
struct TurnOutput {
    /// Aggregated assistant text.
    text: String,
    /// Ordered timeline of the turn (text / thinking / tool calls / results),
    /// captured live so a reloaded conversation can show tool activity, not
    /// just the final prose.
    blocks: Vec<TurnBlock>,
    /// Final session id captured from `Started` / `SessionRun`.
    session_id: Option<String>,
    /// Provider-reported USD cost, if any.
    cost_usd: Option<f64>,
    /// Provider-reported token usage, if any.
    usage: Option<protocol::TokenUsage>,
    /// Set when the provider reported a non-recoverable error indicating the
    /// resumed session is gone/invalid. Triggers the single full-history
    /// retry.
    stale_session: bool,
    /// A non-recoverable error message that is NOT a stale-session signal —
    /// propagated to the caller as a hard failure.
    fatal_error: Option<String>,
}

/// Produces a provider session stream from a [`SessionRequest`].
///
/// `resume_session_id` is the continuity seam: when `Some`, the producer must
/// route through the backend's **resume** path (the `agent/resume` RPC) so a
/// provider that distinguishes start-vs-resume actually re-attaches to its
/// native session rather than spawning a fresh one. When `None`, the producer
/// starts a brand-new session. This abstraction lets v0.5.11's `chat_provider`
/// plugin and the test mock substitute for the live resolver without the turn
/// loop knowing the difference.
#[async_trait]
pub(crate) trait TurnProducer: Send + Sync {
    async fn start(&self, request: SessionRequest, resume_session_id: Option<&str>) -> Result<SessionRun>;

    /// Whether the backend for `tool` advertises native session resume
    /// (`agent/resume`). The loop consults this BEFORE choosing resume-vs-
    /// replay: a backend that cannot resume must take the full-history replay
    /// path, never a message-only prompt that would silently drop context
    /// (codex round-4 P2). Defaults to `false` so an unknown/unprobeable
    /// backend conservatively replays.
    fn supports_resume(&self, _tool: &str) -> bool {
        false
    }
}

/// Production producer: resolves the installed provider plugin for the
/// request's tool and starts (or resumes) a session.
pub(crate) struct ResolverTurnProducer {
    resolver: Arc<SessionBackendResolver>,
}

impl ResolverTurnProducer {
    pub(crate) fn for_project(project_root: &std::path::Path) -> Self {
        Self { resolver: Arc::new(SessionBackendResolver::with_plugin_discovery(project_root)) }
    }

    /// Resolve the backend for `tool` using a minimal probe request (the
    /// resolver only inspects `request.tool`).
    fn resolve_backend(
        &self,
        tool: &str,
    ) -> Option<Arc<dyn animus_session_backend::session::session_backend::SessionBackend>> {
        let probe = SessionRequest {
            tool: tool.to_string(),
            model: String::new(),
            prompt: String::new(),
            cwd: PathBuf::from("."),
            project_root: None,
            mcp_endpoint: None,
            permission_mode: None,
            timeout_secs: None,
            env_vars: Vec::new(),
            extras: Value::Object(Default::default()),
        };
        self.resolver.resolve(&probe).ok()
    }
}

#[async_trait]
impl TurnProducer for ResolverTurnProducer {
    async fn start(&self, request: SessionRequest, resume_session_id: Option<&str>) -> Result<SessionRun> {
        let backend = self.resolver.resolve(&request).map_err(|err| anyhow!("provider session failed: {err}"))?;
        // Route through the backend's resume RPC (`agent/resume`) only when we
        // hold a session_id AND the backend advertises resume support. The loop
        // already gates `resume_session_id` on `supports_resume`, so this is a
        // defensive double-check: a backend that cannot resume always takes a
        // fresh start (and the loop will have built a full-history prompt).
        let run = match resume_session_id {
            Some(session_id) if backend.capabilities().supports_resume => {
                backend.resume_session(request, session_id).await
            }
            _ => backend.start_session(request).await,
        };
        run.map_err(|err| anyhow!("provider session failed: {err}"))
    }

    fn supports_resume(&self, tool: &str) -> bool {
        self.resolve_backend(tool).map(|backend| backend.capabilities().supports_resume).unwrap_or(false)
    }
}

/// Inputs for a single turn.
pub(crate) struct TurnContext<'a> {
    pub conversation_id: &'a str,
    pub tool: &'a str,
    pub model: &'a str,
    pub user_message: &'a str,
    pub cwd: PathBuf,
    pub project_root: PathBuf,
    /// Provider reasoning/thinking effort (`low`/`medium`/`high`), threaded
    /// into `extras.reasoning_effort` for the provider transport to map.
    pub reasoning_effort: Option<&'a str>,
    /// Provider permission/approval mode, threaded into
    /// `SessionRequest.permission_mode` verbatim for the provider transport
    /// to map (claude `--permission-mode`, codex `-c approval_policy`,
    /// gemini approval mode).
    pub permission_mode: Option<&'a str>,
    /// Per-agent MCP runtime contract for this conversation, threaded into
    /// `extras.runtime_contract` so the provider wires the profile/skill-
    /// scoped MCP servers. `None` when the tool cannot speak MCP.
    pub mcp_contract: Option<&'a Value>,
}

/// Run a single conversation turn.
///
/// Ordering (persistence-first for crash safety):
/// 1. Persist the user message before any provider call.
/// 2. Decide resume-vs-replay from stored meta (the XOR continuity model).
/// 3. Drive the [`TurnProducer`]; stream events to the sink + accumulate.
/// 4. Persist the assistant message; capture `session_id` into meta.
/// 5. On a stale-session error during a resume turn, retry ONCE with full
///    history.
///
/// Returns the seq of the persisted assistant message.
pub(crate) async fn run_turn(
    producer: &dyn TurnProducer,
    store: &dyn ConversationStore,
    sink: &mut dyn ChatStreamSink,
    ctx: TurnContext<'_>,
) -> Result<u64> {
    // Cross-process lock held for the WHOLE turn (read meta → append user →
    // run provider → append assistant → save meta). Two simultaneous sends to
    // one conversation would otherwise both read the same message_count,
    // append same-seq user messages (which the replay filter then drops
    // together), and last-writer-win on meta. Serializing the full turn is
    // the correct semantic for a single conversation; other conversations use
    // other lock files and proceed in parallel. Acquisition is a non-blocking
    // try + async sleep so a contended lock never parks a runtime worker
    // while the holder is itself awaiting provider events.
    let _lock = loop {
        if let Some(lock) = store.try_lock_conversation(ctx.conversation_id)? {
            break lock;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    };

    let mut meta = store
        .load_meta(ctx.conversation_id)?
        .ok_or_else(|| anyhow!("conversation '{}' not found", ctx.conversation_id))?;

    // (1) Persist the user message FIRST — before the provider call — so a
    // crash mid-turn never loses the user's input.
    let user_seq = meta.message_count;
    store.append_message(
        ctx.conversation_id,
        &ChatMessage {
            seq: user_seq,
            role: ChatRole::User,
            content: ctx.user_message.to_string(),
            recorded_at: now_rfc3339(),
            tool: None,
            model: None,
            usage: None,
            cost_usd: None,
            blocks: Vec::new(),
        },
    )?;
    meta.message_count += 1;
    // Persist the bumped count BEFORE the provider call. If the provider
    // fails or the process crashes mid-turn, the user message and the count
    // stay consistent, so the next turn assigns a fresh seq instead of
    // colliding with — and the replay filter dropping — the prior user turn.
    meta.updated_at = now_rfc3339();
    store.save_meta(&meta)?;

    // (2) Resume-vs-replay decision. A stored session_id is only valid for
    // the tool that issued it — a tool change forces replay. A backend that
    // does not advertise native resume must ALSO replay: handing it a
    // message-only prompt would silently drop all prior context, so its only
    // continuity is Animus's full-history replay (codex round-4 P2).
    let tool_unchanged = meta.tool.as_deref() == Some(ctx.tool);
    let can_resume = meta.session_id.is_some() && tool_unchanged && producer.supports_resume(ctx.tool);

    // History to replay in mode 2: every turn EXCEPT the user message we
    // just appended (which goes in as the prompt's trailing "User:" line).
    let prior_history: Vec<ChatMessage> = {
        let all = store.load_messages(ctx.conversation_id)?;
        all.into_iter().filter(|m| m.seq != user_seq).collect()
    };

    // First attempt: resume if we can, else replay.
    let mut resumed = can_resume;
    let resume_session_id = if can_resume { meta.session_id.clone() } else { None };

    sink.emit(&ChatStreamEvent::TurnStarted {
        conversation_id: ctx.conversation_id.to_string(),
        tool: ctx.tool.to_string(),
        model: ctx.model.to_string(),
        resumed,
    })?;

    let mut output = drive_once(producer, sink, &ctx, &prior_history, resume_session_id.as_deref(), resumed).await?;

    // (5) Stale-session fallback: if a resume attempt reports the session is
    // gone/invalid, retry ONCE with the full-history replay and no
    // session_id. Only meaningful when we actually resumed.
    if output.stale_session && resumed {
        resumed = false;
        sink.emit(&ChatStreamEvent::Warning {
            message: format!("native session for '{}' is gone; replaying full history once", ctx.conversation_id),
        })?;
        sink.emit(&ChatStreamEvent::TurnStarted {
            conversation_id: ctx.conversation_id.to_string(),
            tool: ctx.tool.to_string(),
            model: ctx.model.to_string(),
            resumed,
        })?;
        output = drive_once(producer, sink, &ctx, &prior_history, None, resumed).await?;
        if output.stale_session {
            return Err(anyhow!(
                "provider reported a stale session even after a full-history replay for conversation '{}'",
                ctx.conversation_id
            ));
        }
    }

    if let Some(message) = output.fatal_error {
        return Err(anyhow!(message));
    }

    // A stale-session error on a NON-resumed turn (a fresh start or the
    // full-history replay) is never retried — there is no session to discard —
    // so it must surface as a hard failure rather than persisting an empty
    // assistant turn as success (codex round-2 P2).
    if output.stale_session {
        return Err(anyhow!(
            "provider reported a session error on a fresh attempt for conversation '{}'",
            ctx.conversation_id
        ));
    }

    // (4) Persist the assistant message and capture continuity pointer.
    let assistant_seq = meta.message_count;
    store.append_message(
        ctx.conversation_id,
        &ChatMessage {
            seq: assistant_seq,
            role: ChatRole::Assistant,
            content: output.text.clone(),
            recorded_at: now_rfc3339(),
            tool: Some(ctx.tool.to_string()),
            model: Some(ctx.model.to_string()),
            usage: output.usage.clone(),
            cost_usd: output.cost_usd,
            blocks: output.blocks.clone(),
        },
    )?;
    meta.message_count += 1;

    // Capture SessionRun.session_id into meta for the NEXT turn. When the
    // provider returned no id we clear the pointer so the next turn replays
    // (mode 2) rather than resume a session that does not exist.
    meta.session_id = output.session_id.clone();
    meta.tool = Some(ctx.tool.to_string());
    meta.model = Some(ctx.model.to_string());
    meta.updated_at = now_rfc3339();
    store.save_meta(&meta)?;

    sink.emit(&ChatStreamEvent::TurnCompleted {
        conversation_id: ctx.conversation_id.to_string(),
        seq: assistant_seq,
        session_id: output.session_id.clone(),
    })?;

    Ok(assistant_seq)
}

/// Build the request, start the session, and drain it once. The `resumed`
/// flag selects the prompt shape and whether `extras.session_id` is set —
/// the two are coupled so we can never accidentally do both.
async fn drive_once(
    producer: &dyn TurnProducer,
    sink: &mut dyn ChatStreamSink,
    ctx: &TurnContext<'_>,
    prior_history: &[ChatMessage],
    resume_session_id: Option<&str>,
    resumed: bool,
) -> Result<TurnOutput> {
    // XOR: resumed => prompt is the new message only + session_id threaded
    //      through the backend's resume RPC (and mirrored into extras so a
    //      provider that only honors the param fallback still resumes).
    //      !resumed => prompt is full history + no session_id anywhere.
    let (prompt, mut extras, resume_id) = if resumed {
        let session_id =
            resume_session_id.ok_or_else(|| anyhow!("internal: resume requested without a session_id"))?.to_string();
        (ctx.user_message.to_string(), json!({ "session_id": session_id.clone() }), Some(session_id))
    } else {
        (render_history_prompt(prior_history, ctx.user_message), Value::Object(Default::default()), None)
    };

    // Provider reasoning/thinking effort rides on extras for the transport
    // to map to its own flag; applies to both the resume and replay paths.
    if let Some(level) = ctx.reasoning_effort {
        if let Value::Object(map) = &mut extras {
            map.insert("reasoning_effort".to_string(), Value::String(level.to_string()));
        }
    }

    // Per-agent MCP runtime contract rides on extras.runtime_contract so the
    // provider plugin wires the profile/skill-scoped MCP servers. Applies to
    // both the resume and replay paths so tool access is consistent across a
    // conversation.
    if let Some(contract) = ctx.mcp_contract {
        if let Value::Object(map) = &mut extras {
            map.insert("runtime_contract".to_string(), contract.clone());
            // Mirror the SAME resolved per-agent set onto the
            // plugin-protocol `mcp_servers` channel (forwarded verbatim to
            // the provider as `AgentRunRequest.mcp_servers`); an empty
            // resolved set populates nothing.
            let servers = crate::services::runtime::agent_mcp::contract_mcp_servers_for_wire(contract);
            if !servers.is_empty() {
                map.insert("mcp_servers".to_string(), Value::Object(servers));
            }
        }
    }

    let request = SessionRequest {
        tool: ctx.tool.to_string(),
        model: ctx.model.to_string(),
        prompt,
        cwd: ctx.cwd.clone(),
        project_root: Some(ctx.project_root.clone()),
        mcp_endpoint: None,
        permission_mode: ctx.permission_mode.map(ToOwned::to_owned),
        timeout_secs: None,
        env_vars: Vec::new(),
        extras,
    };

    let mut run = producer.start(request, resume_id.as_deref()).await?;
    drain(&mut run, sink).await
}

/// Drain a session to completion, translating events to the sink and
/// accumulating the assistant text + metadata.
/// Append text to the trailing `Text` block, or start a new one — mirrors the
/// desktop's `foldFrame` so persisted and live timelines match.
fn push_text_block(blocks: &mut Vec<TurnBlock>, chunk: &str) {
    if let Some(TurnBlock::Text { text }) = blocks.last_mut() {
        text.push_str(chunk);
    } else {
        blocks.push(TurnBlock::Text { text: chunk.to_string() });
    }
}

async fn drain(run: &mut SessionRun, sink: &mut dyn ChatStreamSink) -> Result<TurnOutput> {
    let mut text = String::new();
    // Ordered timeline mirroring what the live stream shows, persisted with the
    // assistant turn so reloads can reconstruct tool activity (not just prose).
    let mut blocks: Vec<TurnBlock> = Vec::new();
    // The plugin host emits TWO `Started` frames: the FIRST carries the host's
    // transient *control* id (a UUID minted per dispatch, removed from the host
    // session map when the run ends), and only LATER — at completion, and only
    // when the provider actually returned a native id — does it emit a SECOND
    // `Started` carrying the provider's real session id.
    //
    // Persisting the control id would make the next turn route `agent/resume`
    // with an id the provider never issued (codex round-2/3 P1). So we record
    // the first `Started` id as the control id and ignore it; we only capture a
    // session id from a SUBSEQUENT `Started` (whose id differs from control) or
    // from a `Metadata` frame. If no native id ever arrives, `session_id`
    // stays `None` and the next turn replays full history.
    let mut control_session_id: Option<String> = None;
    let mut session_id: Option<String> = None;
    let mut cost_usd = None;
    let mut usage = None;
    let mut stale_session = false;
    let mut fatal_error = None;
    let mut last_tool_name: Option<String> = None;
    let mut finished = false;

    while let Some(event) = run.events.recv().await {
        match event {
            SessionEvent::Started { session_id: started_id, .. } => {
                match (&control_session_id, started_id) {
                    // First Started frame: treat its id as the control id and
                    // do not capture it as a resumable native id.
                    (None, id) => control_session_id = id,
                    // A later Started frame with an id distinct from the control
                    // id is the provider's native session id.
                    (Some(control), Some(id)) if &id != control => session_id = Some(id),
                    _ => {}
                }
            }
            SessionEvent::TextDelta { text: t } => {
                text.push_str(&t);
                push_text_block(&mut blocks, &t);
                sink.emit(&ChatStreamEvent::TextDelta { text: t })?;
            }
            SessionEvent::FinalText { text: t } => {
                // FinalText is the aggregated text. Prefer it only when we
                // have not been accumulating deltas, to avoid duplicating the
                // body for providers that emit both.
                if text.is_empty() {
                    text.push_str(&t);
                    push_text_block(&mut blocks, &t);
                    sink.emit(&ChatStreamEvent::TextDelta { text: t })?;
                }
            }
            SessionEvent::Thinking { text: t } => {
                // Accumulate consecutive thinking frames into one block so the
                // reasoning text is preserved (and reloadable), not just an
                // indicator.
                match blocks.last_mut() {
                    Some(TurnBlock::Thinking { text }) => {
                        if !text.is_empty() && !t.is_empty() {
                            text.push('\n');
                        }
                        text.push_str(&t);
                    }
                    _ => blocks.push(TurnBlock::Thinking { text: t.clone() }),
                }
                sink.emit(&ChatStreamEvent::Thinking { text: t })?;
            }
            SessionEvent::ToolCall { tool_name, arguments, .. } => {
                last_tool_name = Some(tool_name.clone());
                blocks.push(TurnBlock::ToolCall {
                    tool_name: Some(tool_name.clone()),
                    arguments: Some(arguments.clone()),
                });
                sink.emit(&ChatStreamEvent::ToolCall { tool_name, arguments })?;
            }
            SessionEvent::ToolResult { tool_name, success, output } => {
                // Some providers (e.g. the claude parser) put the tool_use_id
                // in `tool_name` on results because the upstream tool_result
                // references the call by id, not name. Prefer the preceding
                // ToolCall's human name when this looks like an id so the
                // stream reads cleanly; fall back to whatever the provider gave.
                let display_name = match &last_tool_name {
                    Some(name) if tool_name.starts_with("toolu_") || tool_name.starts_with("call_") => name.clone(),
                    _ => tool_name,
                };
                blocks.push(TurnBlock::ToolResult {
                    tool_name: Some(display_name.clone()),
                    success: Some(success),
                    output: Some(output.clone()),
                });
                sink.emit(&ChatStreamEvent::ToolResult { tool_name: display_name, success, output })?;
            }
            SessionEvent::Artifact { .. } => {}
            SessionEvent::Metadata { metadata } => {
                // Provider plugins built on `animus-plugin-runtime` deliver
                // metadata as an ARRAY of individual frames
                // (e.g. `[{"type":"claude_usage","usage":{...}}]`), while the
                // in-tree path may deliver a single object. Flatten both shapes
                // and fold each frame so cost / usage / session_id are not
                // dropped for normal plugin-backed chats (codex round-3 P2).
                for frame in metadata_frames(&metadata) {
                    if let Some(c) = frame.get("cost").and_then(Value::as_f64) {
                        cost_usd = Some(c);
                    }
                    if let Some(u) = extract_token_usage(frame) {
                        usage = Some(u);
                    }
                    // Some providers report their native session id in a
                    // metadata frame rather than `Started`; capture it as a
                    // fallback.
                    if let Some(sid) = frame.get("session_id").and_then(Value::as_str).filter(|s| !s.trim().is_empty())
                    {
                        session_id = Some(sid.to_string());
                    }
                }
                sink.emit(&ChatStreamEvent::Metadata { cost_usd, tokens: usage.clone() })?;
            }
            SessionEvent::Error { message, recoverable } => {
                if recoverable {
                    sink.emit(&ChatStreamEvent::Warning { message })?;
                } else if is_stale_session_error(&message) {
                    // Do not surface as fatal — the loop will retry once
                    // with full history.
                    stale_session = true;
                } else {
                    fatal_error = Some(message);
                }
            }
            SessionEvent::Finished { exit_code } => {
                finished = true;
                if let Some(code) = exit_code {
                    if code != 0 && fatal_error.is_none() && !stale_session {
                        fatal_error = Some(format!("provider exited with code {code}"));
                    }
                }
                break;
            }
        }
    }

    // The event channel closing WITHOUT a terminal `Finished`/`Error` means
    // the provider died mid-turn (crash, kill, dropped sender). Treating that
    // as success would persist a partial/empty assistant message and clear
    // the session pointer as if the turn completed — fail it instead, leaving
    // the stored session_id intact so the next turn can still resume.
    if !finished && fatal_error.is_none() && !stale_session {
        fatal_error = Some("provider stream ended without a terminal event".to_string());
    }

    Ok(TurnOutput { text, blocks, session_id, cost_usd, usage, stale_session, fatal_error })
}

/// Heuristic for "the resumed native session is gone/invalid" so we can fall
/// back to a full-history replay. Conservative substring match on the
/// phrases the wrapped CLI tools emit when a `--resume <id>` target is
/// missing.
fn is_stale_session_error(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    const NEEDLES: &[&str] = &[
        "session not found",
        "no such session",
        "session does not exist",
        "unknown session",
        "session expired",
        "invalid session",
        "could not resume",
        "no conversation found",
    ];
    NEEDLES.iter().any(|needle| lower.contains(needle))
}

/// Flatten a `SessionEvent::Metadata` payload into the individual frames to
/// inspect. Plugin-runtime providers deliver an ARRAY of frames; the in-tree
/// path delivers a single object. Returns a borrowed slice-like iterator over
/// `&Value` frames in either case.
fn metadata_frames(metadata: &Value) -> Vec<&Value> {
    match metadata {
        Value::Array(items) => items.iter().collect(),
        Value::Null => Vec::new(),
        other => vec![other],
    }
}

/// Map a provider metadata frame into [`protocol::TokenUsage`]. Mirrors the
/// key precedence used by the agent-run path's `extract_token_usage`.
fn extract_token_usage(metadata: &Value) -> Option<protocol::TokenUsage> {
    if metadata.is_null() {
        return None;
    }
    const KEYS: &[&str] = &["token_usage", "tokens", "usage", "claude_usage", "codex_usage", "gemini_stats"];
    let payload = KEYS.iter().find_map(|key| metadata.get(*key)).unwrap_or(metadata);
    let read_u32 = |keys: &[&str]| -> Option<u32> {
        keys.iter().find_map(|key| payload.get(*key)).and_then(Value::as_u64).map(|n| n as u32)
    };
    let input = read_u32(&["input", "input_tokens", "prompt_tokens"])?;
    let output = read_u32(&["output", "output_tokens", "completion_tokens"])?;
    let reasoning = read_u32(&["reasoning", "reasoning_tokens"]);
    let cache_read = read_u32(&["cache_read", "cache_read_input_tokens", "cache_read_tokens"]);
    let cache_write = read_u32(&["cache_write", "cache_creation_input_tokens", "cache_write_tokens"]);
    Some(protocol::TokenUsage { input, output, reasoning, cache_read, cache_write })
}

fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::runtime::runtime_chat::sink::CapturingSink;
    use crate::services::runtime::runtime_chat::store::FileConversationStore;
    use std::sync::Mutex;
    use tokio::sync::mpsc;

    /// Records the SessionRequests it receives (plus the resume_session_id the
    /// loop passed) and replays a scripted sequence of SessionEvents per call.
    struct MockProducer {
        requests: Mutex<Vec<SessionRequest>>,
        resume_ids: Mutex<Vec<Option<String>>>,
        // One scripted event-list + session_id per successive call.
        scripts: Mutex<Vec<(Vec<SessionEvent>, Option<String>)>>,
        // Whether this mock backend advertises native resume support.
        supports_resume: bool,
    }

    impl MockProducer {
        fn new(scripts: Vec<(Vec<SessionEvent>, Option<String>)>) -> Self {
            Self {
                requests: Mutex::new(Vec::new()),
                resume_ids: Mutex::new(Vec::new()),
                scripts: Mutex::new(scripts),
                supports_resume: true,
            }
        }
        /// A mock backend that does NOT advertise native resume.
        fn new_without_resume(scripts: Vec<(Vec<SessionEvent>, Option<String>)>) -> Self {
            Self { supports_resume: false, ..Self::new(scripts) }
        }
        fn requests(&self) -> Vec<SessionRequest> {
            self.requests.lock().unwrap().clone()
        }
        fn resume_ids(&self) -> Vec<Option<String>> {
            self.resume_ids.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl TurnProducer for MockProducer {
        async fn start(&self, request: SessionRequest, resume_session_id: Option<&str>) -> Result<SessionRun> {
            self.requests.lock().unwrap().push(request);
            self.resume_ids.lock().unwrap().push(resume_session_id.map(ToOwned::to_owned));
            let (events, session_id) = {
                let mut scripts = self.scripts.lock().unwrap();
                if scripts.is_empty() {
                    (vec![SessionEvent::Finished { exit_code: Some(0) }], None)
                } else {
                    scripts.remove(0)
                }
            };
            let (tx, rx) = mpsc::channel(64);
            for ev in events {
                tx.send(ev).await.unwrap();
            }
            drop(tx);
            Ok(SessionRun { session_id, events: rx, selected_backend: "mock".into(), fallback_reason: None, pid: None })
        }

        fn supports_resume(&self, _tool: &str) -> bool {
            self.supports_resume
        }
    }

    /// Mirror the plugin host's real two-`Started`-frame contract: a control
    /// id FIRST (the host UUID, also set on `SessionRun.session_id`), then the
    /// provider's native id at the END. The loop must ignore the control id and
    /// capture only the provider id.
    fn text_turn(body: &str, provider_session_id: &str) -> (Vec<SessionEvent>, Option<String>) {
        let control_id = format!("control-{provider_session_id}");
        (
            vec![
                SessionEvent::Started { backend: "mock".into(), session_id: Some(control_id.clone()), pid: None },
                SessionEvent::TextDelta { text: body.into() },
                SessionEvent::Started {
                    backend: "mock".into(),
                    session_id: Some(provider_session_id.into()),
                    pid: None,
                },
                SessionEvent::Finished { exit_code: Some(0) },
            ],
            // SessionRun.session_id is the transient control id, as the host
            // sets it. The drain must NOT capture it.
            Some(control_id),
        )
    }

    fn store_for(tmp: &tempfile::TempDir) -> FileConversationStore {
        // FileConversationStore::root is private to the module; build via the
        // same crate so tests can poke at a temp dir.
        FileConversationStore::with_root_for_test(tmp.path().join("chat"))
    }

    fn ctx<'a>(id: &'a str, tool: &'a str, msg: &'a str, tmp: &tempfile::TempDir) -> TurnContext<'a> {
        TurnContext {
            conversation_id: id,
            tool,
            model: "claude-sonnet-4-6",
            user_message: msg,
            cwd: tmp.path().to_path_buf(),
            project_root: tmp.path().to_path_buf(),
            reasoning_effort: None,
            permission_mode: None,
            mcp_contract: None,
        }
    }

    fn ctx_with_effort<'a>(
        id: &'a str,
        tool: &'a str,
        msg: &'a str,
        tmp: &tempfile::TempDir,
        effort: &'a str,
    ) -> TurnContext<'a> {
        TurnContext { reasoning_effort: Some(effort), ..ctx(id, tool, msg, tmp) }
    }

    #[tokio::test]
    async fn new_conversation_replays_full_history_with_no_session_id() {
        let tmp = tempfile::tempdir().unwrap();
        let store = store_for(&tmp);
        store.create(Some("c1".into())).unwrap();
        let producer = MockProducer::new(vec![text_turn("hi there", "sess-1")]);
        let mut sink = CapturingSink::new();

        run_turn(&producer, &store, &mut sink, ctx("c1", "claude", "hello", &tmp)).await.unwrap();

        let reqs = producer.requests();
        assert_eq!(reqs.len(), 1);
        // No prior turns, so the rendered history is just the new user turn.
        assert!(reqs[0].prompt.contains("User: hello"), "prompt: {}", reqs[0].prompt);
        assert!(
            reqs[0].extras.get("session_id").is_none(),
            "new conversation must NOT carry a session_id; extras: {}",
            reqs[0].extras
        );
        // New conversation must NOT route through the resume seam.
        assert_eq!(producer.resume_ids(), vec![None], "first turn must not pass a resume_session_id");
        // turn_started.resumed == false for the first turn.
        let started = sink.events.iter().find_map(|e| match e {
            ChatStreamEvent::TurnStarted { resumed, .. } => Some(*resumed),
            _ => None,
        });
        assert_eq!(started, Some(false));
    }

    #[tokio::test]
    async fn reasoning_effort_absent_leaves_extras_without_the_key() {
        let tmp = tempfile::tempdir().unwrap();
        let store = store_for(&tmp);
        store.create(Some("c1".into())).unwrap();
        let producer = MockProducer::new(vec![text_turn("hi", "sess-1")]);
        let mut sink = CapturingSink::new();

        run_turn(&producer, &store, &mut sink, ctx("c1", "claude", "hello", &tmp)).await.unwrap();

        let reqs = producer.requests();
        assert!(
            reqs[0].extras.get("reasoning_effort").is_none(),
            "absent --reasoning-effort must not inject the key; extras: {}",
            reqs[0].extras
        );
    }

    #[tokio::test]
    async fn reasoning_effort_threaded_into_extras_on_replay_and_resume() {
        let tmp = tempfile::tempdir().unwrap();
        let store = store_for(&tmp);
        store.create(Some("c1".into())).unwrap();
        let producer = MockProducer::new(vec![text_turn("a1", "sess-1"), text_turn("a2", "sess-1")]);

        let mut sink = CapturingSink::new();
        run_turn(&producer, &store, &mut sink, ctx_with_effort("c1", "claude", "q1", &tmp, "high")).await.unwrap();
        let mut sink2 = CapturingSink::new();
        run_turn(&producer, &store, &mut sink2, ctx_with_effort("c1", "claude", "q2", &tmp, "high")).await.unwrap();

        let reqs = producer.requests();
        assert_eq!(reqs.len(), 2);
        // Replay (first) turn carries the effort.
        assert_eq!(
            reqs[0].extras.get("reasoning_effort").and_then(Value::as_str),
            Some("high"),
            "first turn (replay path) must carry reasoning_effort"
        );
        // Resume (second) turn carries BOTH the session_id and the effort.
        assert_eq!(reqs[1].extras.get("session_id").and_then(Value::as_str), Some("sess-1"));
        assert_eq!(
            reqs[1].extras.get("reasoning_effort").and_then(Value::as_str),
            Some("high"),
            "second turn (resume path) must carry reasoning_effort alongside session_id"
        );
    }

    #[tokio::test]
    async fn permission_mode_absent_leaves_request_field_unset() {
        let tmp = tempfile::tempdir().unwrap();
        let store = store_for(&tmp);
        store.create(Some("c1".into())).unwrap();
        let producer = MockProducer::new(vec![text_turn("hi", "sess-1")]);
        let mut sink = CapturingSink::new();

        run_turn(&producer, &store, &mut sink, ctx("c1", "claude", "hello", &tmp)).await.unwrap();

        let reqs = producer.requests();
        assert!(reqs[0].permission_mode.is_none(), "absent --permission-mode must leave the request field unset");
    }

    #[tokio::test]
    async fn permission_mode_threaded_into_request_on_replay_and_resume() {
        let tmp = tempfile::tempdir().unwrap();
        let store = store_for(&tmp);
        store.create(Some("c1".into())).unwrap();
        let producer = MockProducer::new(vec![text_turn("a1", "sess-1"), text_turn("a2", "sess-1")]);

        let ctx_with_mode =
            |msg: &'static str| TurnContext { permission_mode: Some("acceptEdits"), ..ctx("c1", "claude", msg, &tmp) };
        let mut sink = CapturingSink::new();
        run_turn(&producer, &store, &mut sink, ctx_with_mode("q1")).await.unwrap();
        let mut sink2 = CapturingSink::new();
        run_turn(&producer, &store, &mut sink2, ctx_with_mode("q2")).await.unwrap();

        let reqs = producer.requests();
        assert_eq!(reqs.len(), 2);
        assert_eq!(
            reqs[0].permission_mode.as_deref(),
            Some("acceptEdits"),
            "first turn (replay path) must carry permission_mode"
        );
        assert_eq!(reqs[1].extras.get("session_id").and_then(Value::as_str), Some("sess-1"));
        assert_eq!(
            reqs[1].permission_mode.as_deref(),
            Some("acceptEdits"),
            "second turn (resume path) must carry permission_mode alongside session_id"
        );
    }

    #[tokio::test]
    async fn second_turn_resumes_with_session_id_and_message_only() {
        let tmp = tempfile::tempdir().unwrap();
        let store = store_for(&tmp);
        store.create(Some("c1".into())).unwrap();
        let producer = MockProducer::new(vec![text_turn("a1", "sess-1"), text_turn("a2", "sess-1")]);
        let mut sink = CapturingSink::new();

        run_turn(&producer, &store, &mut sink, ctx("c1", "claude", "q1", &tmp)).await.unwrap();
        let mut sink2 = CapturingSink::new();
        run_turn(&producer, &store, &mut sink2, ctx("c1", "claude", "q2", &tmp)).await.unwrap();

        let reqs = producer.requests();
        assert_eq!(reqs.len(), 2);
        // Second turn: prompt is ONLY the new message, session_id present.
        assert_eq!(reqs[1].prompt, "q2", "second-turn prompt must be the new message only");
        assert!(!reqs[1].prompt.contains("q1"), "second-turn prompt must not replay prior history");
        assert!(!reqs[1].prompt.contains("a1"), "second-turn prompt must not replay prior assistant turn");
        assert_eq!(
            reqs[1].extras.get("session_id").and_then(Value::as_str),
            Some("sess-1"),
            "second turn must carry the stored session_id"
        );
        // The session_id must reach the producer through the resume seam, not
        // only buried in extras — otherwise a resume-aware backend would start
        // a fresh native session (codex P1).
        let resume_ids = producer.resume_ids();
        assert_eq!(resume_ids[0], None, "first turn starts fresh");
        assert_eq!(resume_ids[1].as_deref(), Some("sess-1"), "second turn must resume via the resume seam");
        let resumed = sink2.events.iter().find_map(|e| match e {
            ChatStreamEvent::TurnStarted { resumed, .. } => Some(*resumed),
            _ => None,
        });
        assert_eq!(resumed, Some(true));
    }

    #[tokio::test]
    async fn session_id_is_captured_into_meta_after_each_turn() {
        let tmp = tempfile::tempdir().unwrap();
        let store = store_for(&tmp);
        store.create(Some("c1".into())).unwrap();
        let producer = MockProducer::new(vec![text_turn("hi", "sess-xyz")]);
        let mut sink = CapturingSink::new();

        run_turn(&producer, &store, &mut sink, ctx("c1", "claude", "hello", &tmp)).await.unwrap();

        let meta = store.load_meta("c1").unwrap().unwrap();
        // The PROVIDER id is captured, not the host control id ("control-...").
        assert_eq!(meta.session_id.as_deref(), Some("sess-xyz"));
        assert_eq!(meta.tool.as_deref(), Some("claude"));
        assert_eq!(meta.message_count, 2);
    }

    #[tokio::test]
    async fn array_metadata_usage_and_cost_are_aggregated_onto_assistant_turn() {
        let tmp = tempfile::tempdir().unwrap();
        let store = store_for(&tmp);
        store.create(Some("c1".into())).unwrap();
        // Plugin-runtime delivers metadata as an ARRAY of frames; usage nested
        // under a typed frame. The drain must still capture tokens + cost.
        let array_metadata = serde_json::json!([
            { "type": "claude_usage", "usage": { "input": 100, "output": 40 } },
            { "cost": 0.0123 },
        ]);
        let turn = (
            vec![
                SessionEvent::Started { backend: "mock".into(), session_id: Some("control-x".into()), pid: None },
                SessionEvent::TextDelta { text: "hi".into() },
                SessionEvent::Metadata { metadata: array_metadata },
                SessionEvent::Started { backend: "mock".into(), session_id: Some("sess-1".into()), pid: None },
                SessionEvent::Finished { exit_code: Some(0) },
            ],
            Some("control-x".to_string()),
        );
        let producer = MockProducer::new(vec![turn]);
        let mut sink = CapturingSink::new();

        run_turn(&producer, &store, &mut sink, ctx("c1", "claude", "hello", &tmp)).await.unwrap();

        let messages = store.load_messages("c1").unwrap();
        let assistant = messages.iter().find(|m| matches!(m.role, ChatRole::Assistant)).unwrap();
        let usage = assistant.usage.as_ref().expect("usage must be captured from array metadata");
        assert_eq!(usage.input, 100);
        assert_eq!(usage.output, 40);
        assert_eq!(assistant.cost_usd, Some(0.0123));
    }

    #[tokio::test]
    async fn resume_failure_falls_back_once_to_full_history() {
        let tmp = tempfile::tempdir().unwrap();
        let store = store_for(&tmp);
        store.create(Some("c1".into())).unwrap();

        // Turn 1 establishes a session. Turn 2's first attempt (resume)
        // errors stale; the retry (full history) succeeds.
        let stale = (vec![SessionEvent::Error { message: "session not found".into(), recoverable: false }], None);
        let producer = MockProducer::new(vec![text_turn("a1", "sess-1"), stale, text_turn("a2", "sess-2")]);
        let mut sink = CapturingSink::new();

        run_turn(&producer, &store, &mut sink, ctx("c1", "claude", "q1", &tmp)).await.unwrap();
        let mut sink2 = CapturingSink::new();
        run_turn(&producer, &store, &mut sink2, ctx("c1", "claude", "q2", &tmp)).await.unwrap();

        let reqs = producer.requests();
        assert_eq!(reqs.len(), 3, "expected establish + failed-resume + retry");
        // Failed resume attempt carried session_id + message-only prompt.
        assert_eq!(reqs[1].extras.get("session_id").and_then(Value::as_str), Some("sess-1"));
        assert_eq!(reqs[1].prompt, "q2");
        // The failed resume attempt routed through the resume seam; the retry
        // dropped it entirely.
        let resume_ids = producer.resume_ids();
        assert_eq!(resume_ids[1].as_deref(), Some("sess-1"), "failed resume attempt must use the resume seam");
        assert_eq!(resume_ids[2], None, "retry must not pass a resume_session_id");
        // Retry replayed full history with NO session_id.
        assert!(reqs[2].extras.get("session_id").is_none(), "retry must drop the stale session_id");
        assert!(reqs[2].prompt.contains("User: q1"), "retry must replay prior user turn");
        assert!(reqs[2].prompt.contains("Assistant: a1"), "retry must replay prior assistant turn");
        assert!(reqs[2].prompt.trim_end().ends_with("User: q2"));
        // After the successful retry, meta points at the new session.
        let meta = store.load_meta("c1").unwrap().unwrap();
        assert_eq!(meta.session_id.as_deref(), Some("sess-2"));
    }

    #[tokio::test]
    async fn provider_without_resume_support_replays_full_history() {
        let tmp = tempfile::tempdir().unwrap();
        let store = store_for(&tmp);
        store.create(Some("c1".into())).unwrap();
        // A provider that captures a session_id but does NOT advertise resume.
        // The second turn must replay full history, not send a message-only
        // prompt that would drop context.
        let producer = MockProducer::new_without_resume(vec![text_turn("a1", "sess-1"), text_turn("a2", "sess-1")]);
        let mut sink = CapturingSink::new();

        run_turn(&producer, &store, &mut sink, ctx("c1", "claude", "q1", &tmp)).await.unwrap();
        let mut sink2 = CapturingSink::new();
        run_turn(&producer, &store, &mut sink2, ctx("c1", "claude", "q2", &tmp)).await.unwrap();

        let reqs = producer.requests();
        // Even though a session_id is stored, the resume-incapable backend gets
        // the full-history replay and no resume seam / extras.session_id.
        assert_eq!(producer.resume_ids()[1], None, "resume-incapable backend must not get a resume_session_id");
        assert!(reqs[1].extras.get("session_id").is_none(), "resume-incapable backend must not get extras.session_id");
        assert!(reqs[1].prompt.contains("User: q1"), "must replay prior user turn");
        assert!(reqs[1].prompt.contains("Assistant: a1"), "must replay prior assistant turn");
        assert!(reqs[1].prompt.trim_end().ends_with("User: q2"));
        let resumed = sink2.events.iter().find_map(|e| match e {
            ChatStreamEvent::TurnStarted { resumed, .. } => Some(*resumed),
            _ => None,
        });
        assert_eq!(resumed, Some(false), "resume-incapable backend must report resumed=false");
    }

    #[tokio::test]
    async fn tool_change_mid_conversation_replays_full_history() {
        let tmp = tempfile::tempdir().unwrap();
        let store = store_for(&tmp);
        store.create(Some("c1".into())).unwrap();
        // Turn 1 with claude, turn 2 with codex.
        let producer = MockProducer::new(vec![text_turn("a1", "claude-sess"), text_turn("a2", "codex-sess")]);
        let mut sink = CapturingSink::new();

        run_turn(&producer, &store, &mut sink, ctx("c1", "claude", "q1", &tmp)).await.unwrap();
        let mut sink2 = CapturingSink::new();
        run_turn(&producer, &store, &mut sink2, ctx("c1", "codex", "q2", &tmp)).await.unwrap();

        let reqs = producer.requests();
        // Second turn changed tool -> must replay, no session_id reuse.
        assert_eq!(producer.resume_ids()[1], None, "tool change must NOT pass a resume_session_id");
        assert!(reqs[1].extras.get("session_id").is_none(), "tool change must NOT reuse the prior tool's session_id");
        assert!(reqs[1].prompt.contains("User: q1"), "tool change must replay full history");
        assert!(reqs[1].prompt.contains("Assistant: a1"));
        let resumed = sink2.events.iter().find_map(|e| match e {
            ChatStreamEvent::TurnStarted { resumed, .. } => Some(*resumed),
            _ => None,
        });
        assert_eq!(resumed, Some(false), "tool change must report resumed=false");
    }

    #[tokio::test]
    async fn user_message_persisted_before_provider_call() {
        let tmp = tempfile::tempdir().unwrap();
        let store = store_for(&tmp);
        store.create(Some("c1".into())).unwrap();
        // Producer that fails fatally — user message must already be on disk.
        let producer =
            MockProducer::new(vec![(vec![SessionEvent::Error { message: "boom".into(), recoverable: false }], None)]);
        let mut sink = CapturingSink::new();

        let result = run_turn(&producer, &store, &mut sink, ctx("c1", "claude", "hello", &tmp)).await;
        assert!(result.is_err(), "fatal provider error should fail the turn");
        let messages = store.load_messages("c1").unwrap();
        assert_eq!(messages.len(), 1, "user message must persist before the provider call");
        assert_eq!(messages[0].content, "hello");
        assert_eq!(messages[0].role, ChatRole::User);
    }

    #[tokio::test]
    async fn stale_session_error_on_fresh_turn_fails_instead_of_persisting_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let store = store_for(&tmp);
        store.create(Some("c1".into())).unwrap();
        // First turn (no stored session_id -> fresh start) returns a
        // stale-session-matching error. There is nothing to retry, so the turn
        // must fail rather than persist an empty assistant message.
        let producer = MockProducer::new(vec![(
            vec![SessionEvent::Error { message: "session not found".into(), recoverable: false }],
            None,
        )]);
        let mut sink = CapturingSink::new();

        let result = run_turn(&producer, &store, &mut sink, ctx("c1", "claude", "hello", &tmp)).await;
        assert!(result.is_err(), "stale-session error on a fresh turn must fail the turn");
        // Only the user message is persisted — no empty assistant turn.
        let messages = store.load_messages("c1").unwrap();
        assert_eq!(messages.len(), 1, "no assistant turn should be persisted on failure");
        assert_eq!(messages[0].role, ChatRole::User);
    }

    #[test]
    fn concurrent_sends_to_one_conversation_serialize() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("chat");
        FileConversationStore::with_root_for_test(root.clone()).create(Some("c1".into())).unwrap();

        // Each thread mimics a separate `animus chat send` process: its own
        // store handle, producer, and runtime, racing the same conversation.
        let handles: Vec<_> = (0..4)
            .map(|index| {
                let root = root.clone();
                let dir = tmp.path().to_path_buf();
                std::thread::spawn(move || {
                    let store = FileConversationStore::with_root_for_test(root);
                    let producer = MockProducer::new(vec![text_turn(&format!("a{index}"), &format!("sess-{index}"))]);
                    let mut sink = CapturingSink::new();
                    let message = format!("q{index}");
                    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
                    runtime
                        .block_on(run_turn(
                            &producer,
                            &store,
                            &mut sink,
                            TurnContext {
                                conversation_id: "c1",
                                tool: "claude",
                                model: "claude-sonnet-4-6",
                                user_message: &message,
                                cwd: dir.clone(),
                                project_root: dir,
                                reasoning_effort: None,
                                permission_mode: None,
                                mcp_contract: None,
                            },
                        ))
                        .expect("concurrent send turn")
                })
            })
            .collect();
        for handle in handles {
            handle.join().expect("send thread");
        }

        let store = FileConversationStore::with_root_for_test(root);
        let messages = store.load_messages("c1").unwrap();
        assert_eq!(messages.len(), 8, "every user + assistant turn must persist");
        let mut seqs: Vec<u64> = messages.iter().map(|m| m.seq).collect();
        seqs.sort_unstable();
        assert_eq!(seqs, (0..8).collect::<Vec<u64>>(), "seqs must be distinct and contiguous");
        let meta = store.load_meta("c1").unwrap().unwrap();
        assert_eq!(meta.message_count, 8, "meta count must reflect every persisted turn");
    }

    #[tokio::test]
    async fn stream_end_without_terminal_event_fails_turn_and_keeps_session() {
        let tmp = tempfile::tempdir().unwrap();
        let store = store_for(&tmp);
        store.create(Some("c1".into())).unwrap();
        // Turn 1 establishes a session. Turn 2's event stream dies mid-turn —
        // the channel closes after a partial delta, with no Finished/Error.
        let dead_stream = (vec![SessionEvent::TextDelta { text: "partial".into() }], None);
        let producer = MockProducer::new(vec![text_turn("a1", "sess-1"), dead_stream]);
        let mut sink = CapturingSink::new();

        run_turn(&producer, &store, &mut sink, ctx("c1", "claude", "q1", &tmp)).await.unwrap();
        let mut sink2 = CapturingSink::new();
        let err = run_turn(&producer, &store, &mut sink2, ctx("c1", "claude", "q2", &tmp))
            .await
            .expect_err("stream end without a terminal event must fail the turn");
        assert!(err.to_string().contains("without a terminal event"), "{err}");

        // The partial turn is NOT persisted as a successful assistant message.
        let messages = store.load_messages("c1").unwrap();
        assert_eq!(messages.len(), 3, "no assistant turn persisted for the dead stream");
        assert_eq!(messages[2].role, ChatRole::User, "the user message itself must survive");
        // The continuity pointer survives so the next turn can still resume.
        let meta = store.load_meta("c1").unwrap().unwrap();
        assert_eq!(meta.session_id.as_deref(), Some("sess-1"), "session pointer must survive a dead stream");
    }

    #[tokio::test]
    async fn control_session_id_from_run_is_not_persisted_when_provider_returns_none() {
        let tmp = tempfile::tempdir().unwrap();
        let store = store_for(&tmp);
        store.create(Some("c1".into())).unwrap();
        // Provider streams text + Finished but NEVER a Started/Metadata with a
        // session_id. The transient SessionRun.session_id (the host control id)
        // must NOT be captured into meta — otherwise the next turn would wrongly
        // resume. We simulate the control id by setting it on the script's
        // SessionRun while emitting no provider session_id.
        let no_provider_sid = (
            vec![SessionEvent::TextDelta { text: "hi".into() }, SessionEvent::Finished { exit_code: Some(0) }],
            Some("control-transient-id".to_string()),
        );
        let producer = MockProducer::new(vec![no_provider_sid]);
        let mut sink = CapturingSink::new();

        run_turn(&producer, &store, &mut sink, ctx("c1", "claude", "hello", &tmp)).await.unwrap();

        let meta = store.load_meta("c1").unwrap().unwrap();
        assert_eq!(
            meta.session_id, None,
            "the transient control session id must not be captured as a resumable native id"
        );
    }
}
