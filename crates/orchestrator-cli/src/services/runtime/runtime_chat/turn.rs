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

use std::path::{Path, PathBuf};
use std::sync::Arc;

use animus_session_backend::session::{SessionEvent, SessionRequest, SessionRun};
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use orchestrator_config::skill_definition::SkillApplicationResult;
use orchestrator_plugin_host::session::SessionBackendResolver;
use serde_json::{json, Value};

use crate::services::runtime::runtime_agent::provider_client::{graft_skill_launch_contract, skill_has_launch_extras};

#[cfg(test)]
use super::idempotency::ChatOperationAuthority;
use super::idempotency::ChatTurnOperation;
use super::sink::{ChatStreamEvent, ChatStreamSink};
use super::store::{render_history_prompt, ChatMessage, ChatRole, ConversationMeta, ConversationStore, TurnBlock};

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
            mcp_servers: None,
            extras: Value::Object(Default::default()),
            actor: None,
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
    /// Canonical configured profile this operation expects. `None` means the
    /// conversation must still be unbound when the turn lock is acquired.
    pub agent_id: Option<&'a str>,
    /// Optimistic-concurrency token observed by the application preflight.
    pub expected_revision: Option<u64>,
    /// Optional title mutation applied under the same revision check and lock.
    pub title_update: Option<&'a str>,
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
    /// Kernel-mediated approvals: when true, `extras.approvals = true` rides
    /// every turn's session request so the transport routes permission
    /// decisions through `animus.agent.request_approval`.
    pub approvals: bool,
    /// Bound profile persona, applied to fresh, resumed, and replay-fallback
    /// attempts (and composed before any explicit skill fragments).
    pub agent_system_prompt: Option<&'a str>,
    /// Provider-specific named profile from the bound agent config.
    pub agent_tool_profile: Option<&'a str>,
    /// Per-agent MCP runtime contract for this conversation, threaded into
    /// `extras.runtime_contract` so the provider wires the profile/skill-
    /// scoped MCP servers. `None` when the tool cannot speak MCP.
    pub mcp_contract: Option<&'a Value>,
    /// Path to a per-run ISOLATED, actor-pinned `.mcp.json` for actor-scoped
    /// runs. Threaded into `extras.mcp_config_path` so a provider that locates
    /// MCP servers by file auto-discovery can be pointed at this run-private
    /// file (e.g. claude-code's `--mcp-config`) instead of the actor-stripped
    /// shared cwd file. `None` for global / non-actor runs. Consuming this path
    /// is provider-launch plumbing tracked out-of-tree.
    pub isolated_mcp_config_path: Option<&'a Path>,
    /// The `--skill`'s full application for this conversation, resolved ONCE
    /// per `animus chat send` invocation (the same lifecycle as
    /// `mcp_contract`) and applied to every attempt within the turn: prompt
    /// prefixes/suffixes/directives wrap the outgoing prompt, system-prompt
    /// fragments ride `extras.system_prompt`, env rides
    /// `SessionRequest.env_vars`, and launch-affecting fields
    /// (`extra_args` / `codex_config_overrides` / `env`) are grafted onto the
    /// runtime contract's `cli.launch` block. `None` when no `--skill` is
    /// selected or the skill application is empty.
    pub skill: Option<&'a SkillApplicationResult>,
    /// Durable application-operation claim. Local interactive sends omit it.
    pub operation: Option<&'a mut ChatTurnOperation>,
    /// Fully resolved provider/profile execution snapshot for a keyed send.
    /// It is bound to the durable operation under the conversation lock.
    pub execution_hash: Option<&'a str>,
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
    mut ctx: TurnContext<'_>,
) -> Result<u64> {
    let operation = ctx.operation.take();
    let user_message = ctx.user_message.to_string();
    let Some(operation) = operation else {
        return run_turn_inner(producer, store, sink, ctx, None).await;
    };

    let result = run_turn_inner(producer, store, sink, ctx, Some(&mut *operation)).await;
    match result {
        Ok(assistant_seq) => Ok(assistant_seq),
        Err(turn_error) => {
            // Admission happens before every other keyed-send preparation and
            // turn effect. Reconcile every early-return path, including CAS,
            // append, acceptance, sink, and provider-start failures. Pending
            // operations with no canonical user are released immediately;
            // once the user row exists the operation becomes terminal and its
            // conversation reservation is cleared.
            match reconcile_pre_execution_failure(store, operation, &user_message).await {
                Ok(_) => Err(turn_error),
                Err(reconcile_error) => Err(turn_error
                    .context(format!("chat operation failure reconciliation also failed: {reconcile_error:#}"))),
            }
        }
    }
}

async fn run_turn_inner(
    producer: &dyn TurnProducer,
    store: &dyn ConversationStore,
    sink: &mut dyn ChatStreamSink,
    ctx: TurnContext<'_>,
    mut operation: Option<&mut ChatTurnOperation>,
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
    let _lock = acquire_conversation_lock(store, ctx.conversation_id).await?;

    let mut meta = store
        .load_meta(ctx.conversation_id)?
        .ok_or_else(|| anyhow!("conversation '{}' not found", ctx.conversation_id))?;

    // Repair a crash between append_message and save_meta before assigning a
    // new sequence. This is also what makes an idempotency retry safe after
    // the process stopped at that exact filesystem boundary.
    let existing = store.load_messages(ctx.conversation_id)?;
    let canonical_count = existing.iter().map(|message| message.seq.saturating_add(1)).max().unwrap_or(0);
    let operation_id = operation.as_ref().map(|op| op.claim().operation_id.clone());
    let user_message_id = operation
        .as_ref()
        .map(|op| op.user_message_id().to_string())
        .unwrap_or_else(|| format!("msg-{}", uuid::Uuid::new_v4()));

    let operation_reserved = match (meta.active_operation_id.as_deref(), operation_id.as_deref()) {
        (Some(active), Some(current)) if active == current => true,
        (Some(active), Some(current)) => {
            return Err(crate::conflict_error(format!(
                "idempotency_in_progress: conversation is reserved by operation '{active}', not '{current}'"
            )));
        }
        (Some(active), None) => {
            return Err(crate::conflict_error(format!(
                "idempotency_in_progress: conversation is reserved by application operation '{active}'"
            )));
        }
        (None, _) => false,
    };

    // A recovered pending claim may have crashed after appending the user row
    // but before advancing the SQLite journal. Stable ids are authoritative.
    // The staged external protocol omits message ids, so under this exact
    // operation reservation the pre-append message_count is also a canonical
    // seq/role locator; content is validated below before any provider runs.
    let allow_seq_fallback = operation.as_ref().is_some_and(|op| op.claim().recovered) && operation_reserved;
    let recovered_user = existing.iter().find(|message| {
        message.id.as_deref() == Some(&user_message_id)
            || (allow_seq_fallback
                && message.id.is_none()
                && message.seq == meta.message_count
                && message.role == ChatRole::User)
    });

    if let Some(op) = operation.as_mut() {
        let execution_hash = ctx
            .execution_hash
            .ok_or_else(|| anyhow!("internal: keyed chat operation is missing its resolved execution hash"))?;
        if matches!(op.bind_execution_hash(execution_hash)?, super::idempotency::ExecutionHashBinding::Drifted) {
            if !op.claim().recovered {
                return Err(crate::conflict_error(
                    "idempotency_conflict: a live chat operation changed its resolved execution snapshot",
                ));
            }
            if let Some(message) = recovered_user {
                if message.role != ChatRole::User || message.content != ctx.user_message {
                    return Err(crate::conflict_error(
                        "idempotency_conflict: recovered user message does not match the effective chat request",
                    ));
                }
                let receipt = op.interrupt_recovered_user(
                    message.seq,
                    "resolved execution changed after the user message was accepted; provider execution was not repeated",
                )?;
                clear_operation_reservation_locked(store, ctx.conversation_id, &receipt.operation_id)?;
                sink.emit(&ChatStreamEvent::TurnFailed {
                    status: receipt.status,
                    conversation_id: ctx.conversation_id.to_string(),
                    user_seq: receipt.user_seq.unwrap_or(message.seq),
                    user_message_id: receipt.user_message_id,
                    operation_id: Some(receipt.operation_id),
                    error_code: receipt.error_code.unwrap_or_else(|| "assistant_interrupted".to_string()),
                    error_message: receipt
                        .error_message
                        .unwrap_or_else(|| "assistant execution was interrupted".to_string()),
                })?;
                return Err(anyhow!(
                    "assistant_interrupted: resolved execution changed after canonical user-message acceptance"
                ));
            }

            // No canonical user row means no provider could have started. The
            // recovered lease and conversation lock exclude the old process,
            // so rebinding and continuing this same operation is safe.
            op.rebind_recovered_execution_hash(execution_hash)?;
        }
    }

    if let Some(expected) = ctx.expected_revision {
        // The persisted operation id is durable proof that this exact retry
        // consumed the original revision reservation—even if the process died
        // before appending its user row.
        if meta.revision != expected && !operation_reserved {
            return Err(crate::conflict_error(format!(
                "chat_precondition_failed:revision_conflict: conversation '{}' expected revision {}, found {}",
                ctx.conversation_id, expected, meta.revision
            )));
        }
    }

    // Re-check identity while holding the same lock that protects message
    // persistence. This prevents a local bind/rebind race between the CLI's
    // preflight and provider execution. The plugin contract carries the same
    // field; multi-host backends must make save_meta conditional/serialized.
    let mut meta_changed = false;
    if let Some(current) = operation_id.as_deref() {
        if !operation_reserved {
            meta.active_operation_id = Some(current.to_string());
            meta_changed = true;
        }
    }
    if meta.message_count < canonical_count {
        meta.message_count = canonical_count;
        meta_changed = true;
    }
    match (meta.agent_id.as_deref(), ctx.agent_id) {
        (Some(stored), Some(expected)) if stored == expected => {}
        (None, Some(expected)) => {
            meta.agent_id = Some(expected.to_string());
            meta_changed = true;
        }
        (None, None) => {}
        (Some(stored), Some(expected)) => {
            return Err(crate::conflict_error(format!(
                "chat_precondition_failed:binding_conflict: conversation agent binding changed before send (expected '{expected}', found '{stored}')"
            )));
        }
        (Some(stored), None) => {
            return Err(crate::conflict_error(format!(
                "chat_precondition_failed:binding_conflict: conversation became bound to agent '{stored}' before send"
            )));
        }
    }
    if let Some(title) = ctx.title_update {
        let title = super::normalize_title_update(Some(title)).expect("turn context supplied a title update");
        if meta.title != title {
            meta.title = title;
            meta_changed = true;
        }
    }
    // An application-supplied revision also reserves this operation with a
    // backend CAS before the first message append. Merely comparing the token
    // after load would leave a multi-host gap between that read and mutation.
    // Binding/title updates serve as the reservation when present; otherwise
    // advance the revision alone.
    if meta_changed || (ctx.expected_revision.is_some() && !operation_reserved) {
        meta.updated_at = now_rfc3339();
        save_meta_update(store, &mut meta)?;
        let persisted = store
            .load_meta(ctx.conversation_id)?
            .ok_or_else(|| anyhow!("conversation '{}' disappeared while binding", ctx.conversation_id))?;
        if persisted.agent_id != meta.agent_id
            || persisted.revision != meta.revision
            || persisted.active_operation_id != meta.active_operation_id
        {
            return Err(crate::conflict_error(format!(
                "chat_precondition_failed:reservation_lost: conversation store did not preserve canonical binding/revision/operation reservation for '{}'",
                ctx.conversation_id
            )));
        }
        meta = persisted;
    }

    // (1) Persist the user message FIRST — before the provider call — so a
    // crash mid-turn never loses the user's input. Recovered operations reuse
    // the preallocated id and canonical sequence instead of appending again.
    let user_seq = if let Some(message) = recovered_user {
        if message.role != ChatRole::User || message.content != ctx.user_message {
            return Err(crate::conflict_error(
                "idempotency_conflict: recovered user message does not match the effective chat request",
            ));
        }
        message.seq
    } else {
        let seq = meta.message_count;
        store.append_message(
            ctx.conversation_id,
            &ChatMessage {
                id: Some(user_message_id.clone()),
                seq,
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
        seq
    };

    if let Some(op) = operation.as_mut() {
        op.mark_user_accepted(user_seq)?;
    }
    meta.message_count = meta.message_count.max(user_seq.saturating_add(1));
    // Persist the bumped count BEFORE the provider call. If the provider
    // fails or the process crashes mid-turn, the user message and the count
    // stay consistent, so the next turn assigns a fresh seq instead of
    // colliding with — and the replay filter dropping — the prior user turn.
    meta.updated_at = now_rfc3339();
    save_meta_update(store, &mut meta)?;
    sink.emit(&ChatStreamEvent::UserMessageAccepted {
        status: orchestrator_core::ChatOperationStatus::UserAccepted,
        conversation_id: ctx.conversation_id.to_string(),
        seq: user_seq,
        message_id: user_message_id.clone(),
        operation_id: operation_id.clone(),
    })?;

    let assistant_message_id = operation
        .as_ref()
        .map(|op| op.assistant_message_id().to_string())
        .unwrap_or_else(|| format!("msg-{}", uuid::Uuid::new_v4()));
    let turn =
        finish_turn(producer, store, sink, &ctx, meta, user_seq, &assistant_message_id, operation_id.as_deref()).await;

    match turn {
        Ok((assistant_seq, session_id)) => {
            if let Some(op) = operation.as_mut() {
                op.complete(assistant_seq)?;
            }
            // Application-keyed replays come from the durable receipt, which
            // intentionally does not expose the provider continuity token.
            // Keep the first keyed response wire-identical; continuity remains
            // persisted in conversation metadata for the next turn.
            let response_session_id = operation_id.is_none().then_some(session_id).flatten();
            sink.emit(&ChatStreamEvent::TurnCompleted {
                status: orchestrator_core::ChatOperationStatus::Completed,
                conversation_id: ctx.conversation_id.to_string(),
                seq: assistant_seq,
                message_id: assistant_message_id,
                user_seq,
                user_message_id,
                operation_id,
                session_id: response_session_id,
            })?;
            Ok(assistant_seq)
        }
        Err(error) => {
            // append_message and metadata persistence are separate operations
            // for both the filesystem and plugin stores. If the assistant row
            // is already canonical, a later metadata/CAS error must not turn a
            // durable answer into a permanently replayed failure. Stable IDs
            // are preferred; current external stores can be reconciled by the
            // serialized canonical next sequence and assistant role.
            let canonical_assistant_seq = user_seq.checked_add(1);
            let persisted_assistant = store.load_messages(ctx.conversation_id)?.into_iter().find(|message| {
                message.id.as_deref() == Some(assistant_message_id.as_str())
                    || (message.id.is_none()
                        && Some(message.seq) == canonical_assistant_seq
                        && message.role == ChatRole::Assistant)
            });
            if let Some(message) = persisted_assistant {
                if let Some(op) = operation.as_mut() {
                    op.complete(message.seq)?;
                }
                if let Some(id) = operation_id.as_deref() {
                    clear_operation_reservation_locked(store, ctx.conversation_id, id)?;
                }
                sink.emit(&ChatStreamEvent::TurnCompleted {
                    status: orchestrator_core::ChatOperationStatus::Completed,
                    conversation_id: ctx.conversation_id.to_string(),
                    seq: message.seq,
                    message_id: assistant_message_id,
                    user_seq,
                    user_message_id,
                    operation_id,
                    session_id: None,
                })?;
                return Ok(message.seq);
            }
            if let Some(op) = operation.as_mut() {
                let receipt = op.fail("provider_failed", &error.to_string())?;
                clear_operation_reservation_locked(store, ctx.conversation_id, &receipt.operation_id)?;
                sink.emit(&ChatStreamEvent::TurnFailed {
                    status: receipt.status,
                    conversation_id: ctx.conversation_id.to_string(),
                    user_seq: receipt.user_seq.unwrap_or(user_seq),
                    user_message_id: receipt.user_message_id,
                    operation_id: Some(receipt.operation_id),
                    error_code: receipt.error_code.unwrap_or_else(|| "provider_failed".to_string()),
                    error_message: receipt.error_message.unwrap_or_else(|| "assistant failed".to_string()),
                })?;
            }
            Err(error)
        }
    }
}

async fn finish_turn(
    producer: &dyn TurnProducer,
    store: &dyn ConversationStore,
    sink: &mut dyn ChatStreamSink,
    ctx: &TurnContext<'_>,
    mut meta: super::store::ConversationMeta,
    user_seq: u64,
    assistant_message_id: &str,
    operation_id: Option<&str>,
) -> Result<(u64, Option<String>)> {
    // (2) Resume-vs-replay decision. A stored session_id is only valid for
    // the tool that issued it — a tool change forces replay. A backend that
    // does not advertise native resume must ALSO replay: handing it a
    // message-only prompt would silently drop all prior context, so its only
    // continuity is Animus's full-history replay (codex round-4 P2).
    let tool_unchanged = meta.tool.as_deref() == Some(ctx.tool);
    // A skill with launch-affecting fields (extra_args / codex overrides /
    // env) needs a grafted `cli.launch` block on EVERY turn's contract, and a
    // grafted launch carries no native resume args — so such skills force the
    // full-history replay path for consistent per-process behavior.
    //
    // Why `animus agent run` needs no analogous guard: run is single-shot.
    // `session_request_from_args` rebuilds the launch graft from the REAL
    // final prompt on every invocation — there is no resume seam to poison,
    // because run never re-attaches to a native session via the message-only
    // mode-1 prompt that this replay-forcing protects. Even run's one
    // continuation channel (`--context-json '{"session_id": ...}'`) forwards
    // the freshly-grafted `runtime_contract` alongside the session_id (see
    // `PluginSessionBackend::build_run_params`), so the launch flags still
    // apply. The asymmetry is therefore correct, not a hole. Documented at
    // `docs/reference/cli/index.md` (ad-hoc skills section).
    let skill_forces_replay = ctx.skill.is_some_and(skill_has_launch_extras);
    let can_resume =
        meta.session_id.is_some() && tool_unchanged && producer.supports_resume(ctx.tool) && !skill_forces_replay;

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

    let mut output = drive_once(producer, sink, ctx, &prior_history, resume_session_id.as_deref(), resumed).await?;

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
        output = drive_once(producer, sink, ctx, &prior_history, None, resumed).await?;
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
            id: Some(assistant_message_id.to_string()),
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
    if meta.active_operation_id.as_deref() == operation_id {
        meta.active_operation_id = None;
    }
    meta.updated_at = now_rfc3339();
    save_meta_update(store, &mut meta)?;

    Ok((assistant_seq, output.session_id))
}

async fn acquire_conversation_lock(
    store: &dyn ConversationStore,
    conversation_id: &str,
) -> Result<super::store::ConversationLock> {
    loop {
        if let Some(lock) = store.try_lock_conversation(conversation_id)? {
            return Ok(lock);
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
}

pub(crate) async fn reconcile_recovered_accepted(
    store: &dyn ConversationStore,
    operation: &mut ChatTurnOperation,
) -> Result<orchestrator_core::ChatOperationReceipt> {
    let claim = operation.claim();
    let conversation_id = &claim.request().conversation_id;
    let _lock = acquire_conversation_lock(store, conversation_id).await?;
    if !operation.renew_authority()? {
        let receipt = operation.receipt()?;
        if !receipt.status.is_terminal() {
            return Err(crate::conflict_error(
                "idempotency_in_progress: recovered chat operation authority moved while waiting for the conversation lock",
            ));
        }
        clear_operation_reservation_locked(store, conversation_id, &receipt.operation_id)?;
        return Ok(receipt);
    }
    let canonical_assistant_seq = claim.user_seq.and_then(|seq| seq.checked_add(1));
    let assistant_seq = store
        .load_messages(conversation_id)?
        .into_iter()
        .find(|message| {
            message.id.as_deref() == Some(&claim.assistant_message_id)
                || (message.id.is_none()
                    && Some(message.seq) == canonical_assistant_seq
                    && message.role == ChatRole::Assistant)
        })
        .map(|message| message.seq);
    let receipt = operation.reconcile_recovered_accepted(assistant_seq)?;
    if !receipt.status.is_terminal() {
        return Err(crate::conflict_error(
            "idempotency_in_progress: recovered chat operation authority moved before terminal reconciliation",
        ));
    }
    clear_operation_reservation_locked(store, conversation_id, &receipt.operation_id)?;
    Ok(receipt)
}

/// Resolve an error that occurred after a pending keyed admission but before
/// `run_turn` could bind/execute it. This prevents a deleted profile, invalid
/// MCP config, or other preparation failure from stranding the conversation.
pub(crate) async fn reconcile_pre_execution_failure(
    store: &dyn ConversationStore,
    operation: &mut ChatTurnOperation,
    user_message: &str,
) -> Result<Option<orchestrator_core::ChatOperationReceipt>> {
    let conversation_id = operation.claim().request().conversation_id.clone();
    let operation_id = operation.claim().operation_id.clone();
    let user_message_id = operation.user_message_id().to_string();
    let _lock = acquire_conversation_lock(store, &conversation_id).await?;

    if !operation.renew_authority()? {
        let receipt = operation.receipt()?;
        if !receipt.status.is_terminal() {
            return Err(crate::conflict_error(
                "idempotency_in_progress: pending chat operation authority moved during preparation failure reconciliation",
            ));
        }
        clear_operation_reservation_locked(store, &conversation_id, &operation_id)?;
        return Ok(Some(receipt));
    }

    let meta = store
        .load_meta(&conversation_id)?
        .ok_or_else(|| anyhow!("conversation '{conversation_id}' disappeared during operation reconciliation"))?;
    let allow_seq_fallback =
        operation.claim().recovered && meta.active_operation_id.as_deref() == Some(operation_id.as_str());
    let messages = store.load_messages(&conversation_id)?;
    let user = messages.iter().find(|message| {
        message.id.as_deref() == Some(user_message_id.as_str())
            || (allow_seq_fallback
                && message.id.is_none()
                && message.seq == meta.message_count
                && message.role == ChatRole::User)
    });

    if let Some(message) = user {
        if message.role != ChatRole::User || message.content != user_message {
            return Err(crate::conflict_error(
                "idempotency_conflict: recovered user message does not match the failed chat preparation",
            ));
        }
        let canonical_assistant_seq = message.seq.checked_add(1);
        let assistant_seq = messages
            .iter()
            .find(|candidate| {
                candidate.id.as_deref() == Some(operation.assistant_message_id())
                    || (candidate.id.is_none()
                        && Some(candidate.seq) == canonical_assistant_seq
                        && candidate.role == ChatRole::Assistant)
            })
            .map(|candidate| candidate.seq);
        let receipt = operation.reconcile_durable_user(
            message.seq,
            assistant_seq,
            "chat preparation failed after the user message was accepted; provider execution was not repeated",
        )?;
        if !receipt.status.is_terminal() {
            return Err(crate::conflict_error(
                "idempotency_in_progress: preparation failure could not record a terminal chat outcome",
            ));
        }
        clear_operation_reservation_locked(store, &conversation_id, &operation_id)?;
        return Ok(Some(receipt));
    }

    // No canonical user row means this admission has no externally visible
    // effect. Clear its conversation reservation first, then delete the
    // lease-owned pending journal row so the same caller key can retry after
    // the configuration problem is repaired.
    clear_operation_reservation_locked(store, &conversation_id, &operation_id)?;
    operation.release_pending()?;
    Ok(None)
}

pub(crate) async fn clear_operation_reservation(
    store: &dyn ConversationStore,
    conversation_id: &str,
    operation_id: &str,
) -> Result<()> {
    let _lock = acquire_conversation_lock(store, conversation_id).await?;
    clear_operation_reservation_locked(store, conversation_id, operation_id)
}

fn clear_operation_reservation_locked(
    store: &dyn ConversationStore,
    conversation_id: &str,
    operation_id: &str,
) -> Result<()> {
    let Some(mut meta) = store.load_meta(conversation_id)? else {
        return Ok(());
    };
    let canonical_count = store
        .load_messages(conversation_id)?
        .into_iter()
        .map(|message| message.seq.saturating_add(1))
        .max()
        .unwrap_or(0);
    let mut changed = false;
    if meta.active_operation_id.as_deref() == Some(operation_id) {
        meta.active_operation_id = None;
        changed = true;
    }
    if meta.message_count < canonical_count {
        meta.message_count = canonical_count;
        changed = true;
    }
    if !changed {
        return Ok(());
    }
    meta.updated_at = now_rfc3339();
    save_meta_update(store, &mut meta)
}

fn save_meta_update(store: &dyn ConversationStore, meta: &mut ConversationMeta) -> Result<()> {
    let expected = meta.revision;
    meta.revision =
        meta.revision.checked_add(1).ok_or_else(|| anyhow!("conversation '{}' revision exhausted", meta.id))?;
    store.save_meta_if_revision(meta, Some(expected))
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

    // Skill prompt fragments wrap EVERY turn's outgoing prompt (each turn is
    // an independent provider process): prefixes, "Skill directives:"
    // section, the turn body, suffixes — same ordering as workflow phases.
    let prompt = match ctx.skill {
        Some(skill) => animus_runtime_shared::apply_skill_prompt_to_body(&prompt, skill),
        None => prompt,
    };

    // The bound profile persona rides every attempt. Explicit skill fragments
    // compose after it using the same merge helper as ad-hoc agent runs.
    if let Value::Object(map) = &mut extras {
        let system_prompt = match ctx.skill {
            Some(skill) => animus_runtime_shared::merge_skill_system_prompt(ctx.agent_system_prompt, skill),
            None => ctx.agent_system_prompt.map(ToOwned::to_owned),
        };
        if let Some(system_prompt) = system_prompt {
            map.insert("system_prompt".to_string(), Value::String(system_prompt));
        }
        if let Some(tool_profile) = ctx.agent_tool_profile {
            map.insert("claude_profile".to_string(), Value::String(tool_profile.to_string()));
        }
    }

    // Provider reasoning/thinking effort rides on extras for the transport
    // to map to its own flag; applies to both the resume and replay paths.
    if let Some(level) = ctx.reasoning_effort {
        if let Value::Object(map) = &mut extras {
            map.insert("reasoning_effort".to_string(), Value::String(level.to_string()));
        }
    }

    // Kernel-mediated approvals ride on extras for the transport to wire
    // (claude `--permission-prompt-tool`; others system-prompt injection);
    // applies to both the resume and replay paths.
    if ctx.approvals {
        if let Value::Object(map) = &mut extras {
            map.insert("approvals".to_string(), Value::Bool(true));
        }
    }

    // Per-agent MCP runtime contract rides on extras.runtime_contract so the
    // provider plugin wires the profile/skill-scoped MCP servers. Applies to
    // both the resume and replay paths so tool access is consistent across a
    // conversation. A skill with launch-affecting fields additionally grafts
    // a `cli.launch` block built from this turn's final prompt (the same
    // mechanism the workflow path uses); `run_turn` already forced the
    // replay path for such skills, so a grafted launch never has to carry
    // native resume args.
    let mut contract = ctx.mcp_contract.cloned();
    if let Some(skill) = ctx.skill {
        if skill_has_launch_extras(skill) {
            if let Some(grafted) = graft_skill_launch_contract(
                contract.as_ref(),
                ctx.tool,
                ctx.model,
                &prompt,
                ctx.permission_mode,
                ctx.reasoning_effort,
                skill,
            ) {
                contract = Some(grafted);
            }
        }
    }
    if let Some(contract) = contract {
        if let Value::Object(map) = &mut extras {
            // Mirror the SAME resolved per-agent set onto the
            // plugin-protocol `mcp_servers` channel (forwarded verbatim to
            // the provider as `AgentRunRequest.mcp_servers`); an empty
            // resolved set populates nothing.
            let servers = crate::services::runtime::agent_mcp::contract_mcp_servers_for_wire(&contract);
            if !servers.is_empty() {
                map.insert("mcp_servers".to_string(), Value::Object(servers));
            }
            map.insert("runtime_contract".to_string(), contract);
        }
    }

    // Actor-scoped runs also expose a per-run ISOLATED, actor-pinned
    // `.mcp.json` path. A provider that auto-discovers MCP servers from a file
    // (rather than the runtime contract) can be pointed at this run-private
    // file via its MCP-config flag (e.g. claude-code's `--mcp-config`) so the
    // actor reaches that channel too — without the identity ever landing in
    // the shared cwd `.mcp.json`. Consuming this is provider-launch plumbing
    // (out-of-tree); contract-consuming providers stay scoped via
    // `extras.runtime_contract` above.
    if let Some(path) = ctx.isolated_mcp_config_path {
        if let Value::Object(map) = &mut extras {
            map.insert("mcp_config_path".to_string(), Value::String(path.to_string_lossy().into_owned()));
        }
    }

    // Skill env rides `SessionRequest.env_vars`; the plugin host still gates
    // the forwarded env against the provider plugin's manifest.
    let env_vars: Vec<(String, String)> = ctx
        .skill
        .map(|skill| skill.env.iter().map(|(key, value)| (key.clone(), value.clone())).collect())
        .unwrap_or_default();

    let request = SessionRequest {
        tool: ctx.tool.to_string(),
        model: ctx.model.to_string(),
        prompt,
        cwd: ctx.cwd.clone(),
        project_root: Some(ctx.project_root.clone()),
        mcp_endpoint: None,
        permission_mode: ctx.permission_mode.map(ToOwned::to_owned),
        // Chat has no explicit timeout flag; the skill's `timeout_secs`
        // preference applies when declared.
        timeout_secs: ctx.skill.and_then(|skill| skill.timeout_secs),
        env_vars,
        mcp_servers: extras.get("mcp_servers").cloned(),
        extras,
        // CLI-local chat turn: no authenticated control identity → no actor.
        actor: None,
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
            // HITL interaction frames: the decision itself flows through the
            // MCP request_approval / animus.agent.ask keystone. Surface them
            // in the chat stream so the operator sees that the agent paused.
            SessionEvent::InteractionRequested { id, kind } => {
                sink.emit(&ChatStreamEvent::Warning {
                    message: format!("agent requested {kind} interaction (id {id})"),
                })?;
            }
            SessionEvent::InteractionResolved { id, decision } => {
                sink.emit(&ChatStreamEvent::Warning { message: format!("interaction {id} resolved: {decision}") })?;
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
    use crate::services::runtime::runtime_chat::store::{ConversationLock, ConversationSummary, FileConversationStore};
    use std::sync::atomic::{AtomicUsize, Ordering};
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

    struct FailMetaSaveStore {
        inner: FileConversationStore,
        save_calls: AtomicUsize,
        fail_on_call: usize,
    }

    impl ConversationStore for FailMetaSaveStore {
        fn try_lock_conversation(&self, id: &str) -> Result<Option<ConversationLock>> {
            self.inner.try_lock_conversation(id)
        }

        fn create(&self, id: Option<String>) -> Result<ConversationMeta> {
            self.inner.create(id)
        }

        fn load_meta(&self, id: &str) -> Result<Option<ConversationMeta>> {
            self.inner.load_meta(id)
        }

        fn save_meta(&self, meta: &ConversationMeta) -> Result<()> {
            self.inner.save_meta(meta)
        }

        fn save_meta_if_revision(&self, meta: &ConversationMeta, expected: Option<u64>) -> Result<()> {
            let call = self.save_calls.fetch_add(1, Ordering::SeqCst) + 1;
            if call == self.fail_on_call {
                return Err(anyhow!("injected metadata persistence failure"));
            }
            self.inner.save_meta_if_revision(meta, expected)
        }

        fn append_message(&self, id: &str, message: &ChatMessage) -> Result<()> {
            self.inner.append_message(id, message)
        }

        fn load_messages(&self, id: &str) -> Result<Vec<ChatMessage>> {
            self.inner.load_messages(id)
        }

        fn list(&self) -> Result<Vec<ConversationSummary>> {
            self.inner.list()
        }

        fn delete(&self, id: &str) -> Result<()> {
            self.inner.delete(id)
        }
    }

    struct FailAppendOnceStore {
        inner: FileConversationStore,
        append_calls: AtomicUsize,
    }

    impl ConversationStore for FailAppendOnceStore {
        fn try_lock_conversation(&self, id: &str) -> Result<Option<ConversationLock>> {
            self.inner.try_lock_conversation(id)
        }

        fn create(&self, id: Option<String>) -> Result<ConversationMeta> {
            self.inner.create(id)
        }

        fn load_meta(&self, id: &str) -> Result<Option<ConversationMeta>> {
            self.inner.load_meta(id)
        }

        fn save_meta(&self, meta: &ConversationMeta) -> Result<()> {
            self.inner.save_meta(meta)
        }

        fn save_meta_if_revision(&self, meta: &ConversationMeta, expected: Option<u64>) -> Result<()> {
            self.inner.save_meta_if_revision(meta, expected)
        }

        fn append_message(&self, id: &str, message: &ChatMessage) -> Result<()> {
            if self.append_calls.fetch_add(1, Ordering::SeqCst) == 0 {
                return Err(anyhow!("injected user append failure"));
            }
            self.inner.append_message(id, message)
        }

        fn load_messages(&self, id: &str) -> Result<Vec<ChatMessage>> {
            self.inner.load_messages(id)
        }

        fn list(&self) -> Result<Vec<ConversationSummary>> {
            self.inner.list()
        }

        fn delete(&self, id: &str) -> Result<()> {
            self.inner.delete(id)
        }
    }

    fn ctx<'a>(id: &'a str, tool: &'a str, msg: &'a str, tmp: &tempfile::TempDir) -> TurnContext<'a> {
        TurnContext {
            conversation_id: id,
            agent_id: None,
            expected_revision: None,
            title_update: None,
            tool,
            model: "claude-sonnet-4-6",
            user_message: msg,
            cwd: tmp.path().to_path_buf(),
            project_root: tmp.path().to_path_buf(),
            reasoning_effort: None,
            permission_mode: None,
            approvals: false,
            agent_system_prompt: None,
            agent_tool_profile: None,
            mcp_contract: None,
            isolated_mcp_config_path: None,
            skill: None,
            operation: None,
            execution_hash: None,
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
    async fn canonical_agent_binding_and_persona_are_persisted_before_provider_execution() {
        let tmp = tempfile::tempdir().unwrap();
        let store = store_for(&tmp);
        store.create(Some("c1".into())).unwrap();
        let producer = MockProducer::new(vec![text_turn("hi", "sess-1")]);
        let mut sink = CapturingSink::new();
        let turn = TurnContext {
            agent_id: Some("researcher"),
            agent_system_prompt: Some("You are the research agent."),
            agent_tool_profile: Some("research-profile"),
            ..ctx("c1", "claude", "hello", &tmp)
        };

        run_turn(&producer, &store, &mut sink, turn).await.unwrap();

        let meta = store.load_meta("c1").unwrap().unwrap();
        assert_eq!(meta.agent_id.as_deref(), Some("researcher"));
        assert_eq!(meta.revision, 3, "bind + user acceptance + assistant completion each advance revision");
        let request = &producer.requests()[0];
        assert_eq!(request.extras.get("system_prompt").and_then(Value::as_str), Some("You are the research agent."));
        assert_eq!(request.extras.get("claude_profile").and_then(Value::as_str), Some("research-profile"));
    }

    #[tokio::test]
    async fn conflicting_binding_and_stale_revision_fail_before_message_or_provider() {
        let tmp = tempfile::tempdir().unwrap();
        let store = store_for(&tmp);
        let mut meta = store.create(Some("c1".into())).unwrap();
        meta.agent_id = Some("researcher".to_string());
        meta.revision = 1;
        store.save_meta(&meta).unwrap();
        let producer = MockProducer::new(vec![text_turn("unused", "sess-1")]);

        let mut sink = CapturingSink::new();
        let conflict = TurnContext { agent_id: Some("writer"), ..ctx("c1", "claude", "hello", &tmp) };
        let error = run_turn(&producer, &store, &mut sink, conflict).await.unwrap_err();
        assert!(error.to_string().contains("binding changed"), "unexpected error: {error}");

        let mut sink = CapturingSink::new();
        let stale = TurnContext {
            agent_id: Some("researcher"),
            expected_revision: Some(0),
            ..ctx("c1", "claude", "hello", &tmp)
        };
        let error = run_turn(&producer, &store, &mut sink, stale).await.unwrap_err();
        assert!(error.to_string().contains("chat_precondition_failed:revision_conflict:"), "unexpected error: {error}");
        assert!(store.load_messages("c1").unwrap().is_empty());
        assert!(producer.requests().is_empty(), "failed preconditions must not reach the provider");
    }

    #[tokio::test]
    async fn expected_revision_reserves_the_operation_before_message_append() {
        let tmp = tempfile::tempdir().unwrap();
        let store = store_for(&tmp);
        let mut meta = store.create(Some("c1".into())).unwrap();
        meta.agent_id = Some("researcher".to_string());
        meta.revision = 4;
        store.save_meta(&meta).unwrap();
        let producer = MockProducer::new(vec![text_turn("answer", "sess-1")]);
        let mut sink = CapturingSink::new();
        let turn = TurnContext {
            agent_id: Some("researcher"),
            expected_revision: Some(4),
            ..ctx("c1", "claude", "hello", &tmp)
        };

        run_turn(&producer, &store, &mut sink, turn).await.unwrap();

        let meta = store.load_meta("c1").unwrap().unwrap();
        assert_eq!(meta.revision, 7, "reservation + user acceptance + assistant completion");
        assert_eq!(store.load_messages("c1").unwrap().len(), 2);
    }

    #[tokio::test]
    async fn recovered_idempotent_user_row_reuses_consumed_revision_reservation() {
        let tmp = tempfile::tempdir().unwrap();
        let store = store_for(&tmp);
        let mut meta = store.create(Some("c1".into())).unwrap();
        meta.agent_id = Some("researcher".to_string());
        let operation_store =
            orchestrator_core::ChatOperationStore::at_path(tmp.path().join("chat-operations.db"), "test-repo");
        let request = operation_store.request("workspace-a", "alice", "c1", "key-recovered", "request-hash");
        let orchestrator_core::ChatOperationBegin::Acquired(mut claim) =
            operation_store.begin(request.clone()).unwrap()
        else {
            panic!("first operation should acquire");
        };
        claim.recovered = true;
        // Simulate the first process having consumed expected revision 4,
        // durably reserved it for this exact operation, then appended the
        // preallocated user row before its journal update.
        meta.revision = 5;
        meta.active_operation_id = Some(claim.operation_id.clone());
        store.save_meta(&meta).unwrap();
        store
            .append_message(
                "c1",
                &ChatMessage {
                    // Simulate the staged external conversation-store
                    // protocol, which round-trips canonical seq/role/content
                    // but not the additive message id.
                    id: None,
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
            )
            .unwrap();
        let mut operation = ChatTurnOperation::new(ChatOperationAuthority::Local(operation_store.clone()), claim);
        let producer = MockProducer::new(vec![text_turn("answer", "sess-1")]);
        let mut sink = CapturingSink::new();
        let mut context = TurnContext {
            agent_id: Some("researcher"),
            expected_revision: Some(4),
            ..ctx("c1", "claude", "hello", &tmp)
        };
        context.operation = Some(&mut operation);
        context.execution_hash = Some("execution-hash");

        assert_eq!(run_turn(&producer, &store, &mut sink, context).await.unwrap(), 1);
        let messages = store.load_messages("c1").unwrap();
        assert_eq!(messages.len(), 2, "recovery must not append the user row twice");
        assert_eq!(messages[0].id, None);
        let orchestrator_core::ChatOperationBegin::Replay(receipt) = operation_store.begin(request).unwrap() else {
            panic!("recovered operation should complete");
        };
        assert_eq!(receipt.status, orchestrator_core::ChatOperationStatus::Completed);
    }

    #[tokio::test]
    async fn recovered_operation_resumes_after_revision_reservation_before_user_append() {
        let tmp = tempfile::tempdir().unwrap();
        let store = store_for(&tmp);
        let mut meta = store.create(Some("c1".into())).unwrap();
        meta.agent_id = Some("researcher".to_string());
        let operation_store =
            orchestrator_core::ChatOperationStore::at_path(tmp.path().join("chat-operations.db"), "test-repo");
        let request = operation_store.request("workspace-a", "alice", "c1", "key-before-user", "request-hash");
        let orchestrator_core::ChatOperationBegin::Acquired(claim) = operation_store.begin(request.clone()).unwrap()
        else {
            panic!("first operation should acquire");
        };

        // This is the exact crash boundary: expected revision 4 has already
        // been consumed and tied to the operation, but no transcript row or
        // user_accepted journal transition exists yet.
        meta.revision = 5;
        meta.active_operation_id = Some(claim.operation_id.clone());
        store.save_meta(&meta).unwrap();
        let mut operation = ChatTurnOperation::new(ChatOperationAuthority::Local(operation_store.clone()), claim);
        let producer = MockProducer::new(vec![text_turn("answer", "sess-1")]);
        let mut sink = CapturingSink::new();
        let mut context = TurnContext {
            agent_id: Some("researcher"),
            expected_revision: Some(4),
            ..ctx("c1", "claude", "hello", &tmp)
        };
        context.operation = Some(&mut operation);
        context.execution_hash = Some("execution-hash");

        assert_eq!(run_turn(&producer, &store, &mut sink, context).await.unwrap(), 1);
        let messages = store.load_messages("c1").unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].content, "hello");
        assert_eq!(store.load_meta("c1").unwrap().unwrap().active_operation_id, None);
        let orchestrator_core::ChatOperationBegin::Replay(receipt) = operation_store.begin(request).unwrap() else {
            panic!("recovered operation should complete");
        };
        assert_eq!(receipt.status, orchestrator_core::ChatOperationStatus::Completed);
    }

    #[tokio::test]
    async fn recovered_pending_operation_rebinds_changed_execution_when_no_user_was_accepted() {
        let tmp = tempfile::tempdir().unwrap();
        let store = store_for(&tmp);
        let mut meta = store.create(Some("c1".into())).unwrap();
        meta.agent_id = Some("researcher".to_string());
        let operation_store =
            orchestrator_core::ChatOperationStore::at_path(tmp.path().join("chat-operations.db"), "test-repo");
        let request = operation_store.request("workspace-a", "alice", "c1", "key-drift-empty", "request-hash");
        let orchestrator_core::ChatOperationBegin::Acquired(mut claim) =
            operation_store.begin(request.clone()).unwrap()
        else {
            panic!("first operation should acquire");
        };
        assert!(operation_store.bind_execution_hash(&mut claim, "old-execution").unwrap());
        claim.recovered = true;
        meta.revision = 5;
        meta.active_operation_id = Some(claim.operation_id.clone());
        store.save_meta(&meta).unwrap();

        let mut operation = ChatTurnOperation::new(ChatOperationAuthority::Local(operation_store.clone()), claim);
        let producer = MockProducer::new(vec![text_turn("answer", "sess-1")]);
        let mut sink = CapturingSink::new();
        let mut context = TurnContext {
            agent_id: Some("researcher"),
            expected_revision: Some(4),
            ..ctx("c1", "claude", "hello", &tmp)
        };
        context.operation = Some(&mut operation);
        context.execution_hash = Some("new-execution");

        assert_eq!(run_turn(&producer, &store, &mut sink, context).await.unwrap(), 1);
        assert_eq!(producer.requests().len(), 1);
        assert_eq!(store.load_messages("c1").unwrap().len(), 2);
        assert_eq!(store.load_meta("c1").unwrap().unwrap().active_operation_id, None);
        let orchestrator_core::ChatOperationBegin::Replay(receipt) = operation_store.begin(request).unwrap() else {
            panic!("rebound recovered operation should complete");
        };
        assert_eq!(receipt.status, orchestrator_core::ChatOperationStatus::Completed);
    }

    #[tokio::test]
    async fn recovered_execution_drift_interrupts_a_durable_user_without_running_provider() {
        let tmp = tempfile::tempdir().unwrap();
        let store = store_for(&tmp);
        let mut meta = store.create(Some("c1".into())).unwrap();
        meta.agent_id = Some("researcher".to_string());
        let operation_store =
            orchestrator_core::ChatOperationStore::at_path(tmp.path().join("chat-operations.db"), "test-repo");
        let request = operation_store.request("workspace-a", "alice", "c1", "key-drift-user", "request-hash");
        let orchestrator_core::ChatOperationBegin::Acquired(mut claim) =
            operation_store.begin(request.clone()).unwrap()
        else {
            panic!("first operation should acquire");
        };
        assert!(operation_store.bind_execution_hash(&mut claim, "old-execution").unwrap());
        claim.recovered = true;
        meta.revision = 5;
        meta.active_operation_id = Some(claim.operation_id.clone());
        store.save_meta(&meta).unwrap();
        store
            .append_message(
                "c1",
                &ChatMessage {
                    id: Some(claim.user_message_id.clone()),
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
            )
            .unwrap();

        let mut operation = ChatTurnOperation::new(ChatOperationAuthority::Local(operation_store.clone()), claim);
        let producer = MockProducer::new(vec![text_turn("must-not-run", "sess-1")]);
        let mut sink = CapturingSink::new();
        let mut context = TurnContext {
            agent_id: Some("researcher"),
            expected_revision: Some(4),
            ..ctx("c1", "claude", "hello", &tmp)
        };
        context.operation = Some(&mut operation);
        context.execution_hash = Some("new-execution");

        assert!(run_turn(&producer, &store, &mut sink, context).await.is_err());
        assert!(producer.requests().is_empty(), "execution drift must not repeat provider effects");
        let meta = store.load_meta("c1").unwrap().unwrap();
        assert_eq!(meta.active_operation_id, None);
        assert_eq!(meta.message_count, 1);
        let orchestrator_core::ChatOperationBegin::Replay(receipt) = operation_store.begin(request).unwrap() else {
            panic!("interrupted recovered operation should replay");
        };
        assert_eq!(receipt.status, orchestrator_core::ChatOperationStatus::AssistantInterrupted);
        assert_eq!(receipt.user_seq, Some(0));
        assert!(sink.events.iter().any(|event| matches!(
            event,
            ChatStreamEvent::TurnFailed { status: orchestrator_core::ChatOperationStatus::AssistantInterrupted, .. }
        )));
    }

    #[tokio::test]
    async fn recovered_user_accepted_reconciliation_waits_for_the_conversation_lock() {
        let tmp = tempfile::tempdir().unwrap();
        let store = store_for(&tmp);
        let mut meta = store.create(Some("c1".into())).unwrap();
        let operation_store =
            orchestrator_core::ChatOperationStore::at_path(tmp.path().join("chat-operations.db"), "test-repo");
        let request = operation_store.request("workspace-a", "alice", "c1", "key-lock", "request-hash");
        let orchestrator_core::ChatOperationBegin::Acquired(mut claim) = operation_store.begin(request).unwrap() else {
            panic!("first operation should acquire");
        };
        assert!(operation_store.mark_user_accepted(&mut claim, 0).unwrap());
        claim.recovered = true;
        meta.active_operation_id = Some(claim.operation_id.clone());
        store.save_meta(&meta).unwrap();

        let held = store.try_lock_conversation("c1").unwrap().expect("test owns conversation lock");
        let mut operation = ChatTurnOperation::new(ChatOperationAuthority::Local(operation_store), claim);
        let mut reconciliation = Box::pin(reconcile_recovered_accepted(&store, &mut operation));
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(40), &mut reconciliation).await.is_err(),
            "recovery must not inspect or terminalize while another turn owns the lock"
        );
        drop(held);
        let receipt = tokio::time::timeout(std::time::Duration::from_secs(1), reconciliation)
            .await
            .expect("reconciliation should continue after lock release")
            .unwrap();
        assert_eq!(receipt.status, orchestrator_core::ChatOperationStatus::AssistantInterrupted);
        assert_eq!(store.load_meta("c1").unwrap().unwrap().active_operation_id, None);
    }

    #[tokio::test]
    async fn failed_preparation_releases_a_recovered_pending_operation_with_no_user() {
        let tmp = tempfile::tempdir().unwrap();
        let store = store_for(&tmp);
        let mut meta = store.create(Some("c1".into())).unwrap();
        let operation_store =
            orchestrator_core::ChatOperationStore::at_path(tmp.path().join("chat-operations.db"), "test-repo");
        let request = operation_store.request("workspace-a", "alice", "c1", "key-prep-empty", "request-hash");
        let orchestrator_core::ChatOperationBegin::Acquired(mut claim) =
            operation_store.begin(request.clone()).unwrap()
        else {
            panic!("first operation should acquire");
        };
        claim.recovered = true;
        meta.active_operation_id = Some(claim.operation_id.clone());
        store.save_meta(&meta).unwrap();
        let mut operation = ChatTurnOperation::new(ChatOperationAuthority::Local(operation_store.clone()), claim);

        assert!(reconcile_pre_execution_failure(&store, &mut operation, "hello").await.unwrap().is_none());
        assert_eq!(store.load_meta("c1").unwrap().unwrap().active_operation_id, None);
        assert!(matches!(operation_store.begin(request).unwrap(), orchestrator_core::ChatOperationBegin::Acquired(_)));
    }

    #[tokio::test]
    async fn failed_preparation_interrupts_an_external_user_row_without_message_id() {
        let tmp = tempfile::tempdir().unwrap();
        let store = store_for(&tmp);
        let mut meta = store.create(Some("c1".into())).unwrap();
        let operation_store =
            orchestrator_core::ChatOperationStore::at_path(tmp.path().join("chat-operations.db"), "test-repo");
        let request = operation_store.request("workspace-a", "alice", "c1", "key-prep-user", "request-hash");
        let orchestrator_core::ChatOperationBegin::Acquired(mut claim) =
            operation_store.begin(request.clone()).unwrap()
        else {
            panic!("first operation should acquire");
        };
        claim.recovered = true;
        meta.active_operation_id = Some(claim.operation_id.clone());
        store.save_meta(&meta).unwrap();
        store
            .append_message(
                "c1",
                &ChatMessage {
                    id: None,
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
            )
            .unwrap();
        let mut operation = ChatTurnOperation::new(ChatOperationAuthority::Local(operation_store.clone()), claim);

        let receipt = reconcile_pre_execution_failure(&store, &mut operation, "hello")
            .await
            .unwrap()
            .expect("durable user acceptance must become terminal");
        assert_eq!(receipt.status, orchestrator_core::ChatOperationStatus::AssistantInterrupted);
        assert_eq!(receipt.user_seq, Some(0));
        let meta = store.load_meta("c1").unwrap().unwrap();
        assert_eq!(meta.active_operation_id, None);
        assert_eq!(meta.message_count, 1);
        assert!(matches!(operation_store.begin(request).unwrap(), orchestrator_core::ChatOperationBegin::Replay(_)));
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
    async fn approvals_absent_leaves_extras_without_the_key() {
        let tmp = tempfile::tempdir().unwrap();
        let store = store_for(&tmp);
        store.create(Some("c1".into())).unwrap();
        let producer = MockProducer::new(vec![text_turn("hi", "sess-1")]);
        let mut sink = CapturingSink::new();

        run_turn(&producer, &store, &mut sink, ctx("c1", "claude", "hello", &tmp)).await.unwrap();

        let reqs = producer.requests();
        assert!(
            reqs[0].extras.get("approvals").is_none(),
            "approvals=false must not inject the key; extras: {}",
            reqs[0].extras
        );
    }

    #[tokio::test]
    async fn approvals_threaded_into_extras_on_replay_and_resume() {
        let tmp = tempfile::tempdir().unwrap();
        let store = store_for(&tmp);
        store.create(Some("c1".into())).unwrap();
        let producer = MockProducer::new(vec![text_turn("a1", "sess-1"), text_turn("a2", "sess-1")]);

        let mut sink = CapturingSink::new();
        let first = TurnContext { approvals: true, ..ctx("c1", "claude", "q1", &tmp) };
        run_turn(&producer, &store, &mut sink, first).await.unwrap();
        let mut sink2 = CapturingSink::new();
        let second = TurnContext { approvals: true, ..ctx("c1", "claude", "q2", &tmp) };
        run_turn(&producer, &store, &mut sink2, second).await.unwrap();

        let reqs = producer.requests();
        assert_eq!(reqs.len(), 2);
        for (index, request) in reqs.iter().enumerate() {
            assert_eq!(
                request.extras.get("approvals").and_then(Value::as_bool),
                Some(true),
                "turn {index} must carry extras.approvals; extras: {}",
                request.extras
            );
        }
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

        let bound = |message| TurnContext {
            agent_id: Some("researcher"),
            agent_system_prompt: Some("You are the research agent."),
            ..ctx("c1", "claude", message, &tmp)
        };
        run_turn(&producer, &store, &mut sink, bound("q1")).await.unwrap();
        let mut sink2 = CapturingSink::new();
        run_turn(&producer, &store, &mut sink2, bound("q2")).await.unwrap();

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

        let bound = |message| TurnContext {
            agent_id: Some("researcher"),
            agent_system_prompt: Some("You are the research agent."),
            ..ctx("c1", "claude", message, &tmp)
        };
        run_turn(&producer, &store, &mut sink, bound("q1")).await.unwrap();
        let mut sink2 = CapturingSink::new();
        run_turn(&producer, &store, &mut sink2, bound("q2")).await.unwrap();

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
        assert!(
            reqs.iter().all(|request| {
                request.extras.get("system_prompt").and_then(Value::as_str) == Some("You are the research agent.")
            }),
            "bound persona must survive start, native resume, and resume-loss replay fallback"
        );
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
    async fn idempotent_turn_persists_canonical_receipt_and_message_ids() {
        let tmp = tempfile::tempdir().unwrap();
        let store = store_for(&tmp);
        store.create(Some("c1".into())).unwrap();
        let operation_store =
            orchestrator_core::ChatOperationStore::at_path(tmp.path().join("chat-operations.db"), "test-repo");
        let request = operation_store.request("workspace-a", "alice", "c1", "key-1", "request-hash");
        let orchestrator_core::ChatOperationBegin::Acquired(claim) = operation_store.begin(request.clone()).unwrap()
        else {
            panic!("first operation should acquire");
        };
        let mut operation = ChatTurnOperation::new(ChatOperationAuthority::Local(operation_store.clone()), claim);
        let producer = MockProducer::new(vec![text_turn("answer", "sess-1")]);
        let mut sink = CapturingSink::new();
        let mut context = ctx("c1", "claude", "hello", &tmp);
        context.operation = Some(&mut operation);
        context.execution_hash = Some("execution-hash");

        assert_eq!(run_turn(&producer, &store, &mut sink, context).await.unwrap(), 1);
        let messages = store.load_messages("c1").unwrap();
        assert_eq!(messages.len(), 2);
        assert!(messages.iter().all(|message| message.id.is_some()));
        let orchestrator_core::ChatOperationBegin::Replay(receipt) = operation_store.begin(request).unwrap() else {
            panic!("exact retry should replay");
        };
        assert_eq!(receipt.status, orchestrator_core::ChatOperationStatus::Completed);
        assert_eq!(receipt.user_seq, Some(0));
        assert_eq!(receipt.assistant_seq, Some(1));
        assert_eq!(messages[0].id.as_deref(), Some(receipt.user_message_id.as_str()));
        assert_eq!(messages[1].id.as_deref(), Some(receipt.assistant_message_id.as_str()));
        assert!(sink.events.iter().any(|event| matches!(event, ChatStreamEvent::UserMessageAccepted { seq: 0, .. })));
        assert!(sink.events.iter().any(|event| matches!(
            event,
            ChatStreamEvent::TurnCompleted {
                status: orchestrator_core::ChatOperationStatus::Completed,
                user_seq: 0,
                seq: 1,
                session_id: None,
                ..
            }
        )));
    }

    #[tokio::test]
    async fn durable_assistant_row_wins_over_later_metadata_failure() {
        let tmp = tempfile::tempdir().unwrap();
        let store = FailMetaSaveStore {
            inner: store_for(&tmp),
            save_calls: AtomicUsize::new(0),
            // The operation reservation and user-count saves succeed; the
            // assistant metadata save fails after its row has been appended.
            fail_on_call: 3,
        };
        store.create(Some("c1".into())).unwrap();
        let operation_store =
            orchestrator_core::ChatOperationStore::at_path(tmp.path().join("chat-operations.db"), "test-repo");
        let request = operation_store.request("workspace-a", "alice", "c1", "key-meta-fail", "request-hash");
        let orchestrator_core::ChatOperationBegin::Acquired(claim) = operation_store.begin(request.clone()).unwrap()
        else {
            panic!("first operation should acquire");
        };
        let mut operation = ChatTurnOperation::new(ChatOperationAuthority::Local(operation_store.clone()), claim);
        let producer = MockProducer::new(vec![text_turn("answer", "sess-1")]);
        let mut sink = CapturingSink::new();
        let mut context = ctx("c1", "claude", "hello", &tmp);
        context.operation = Some(&mut operation);
        context.execution_hash = Some("execution-hash");

        assert_eq!(run_turn(&producer, &store, &mut sink, context).await.unwrap(), 1);
        let messages = store.load_messages("c1").unwrap();
        assert_eq!(messages.len(), 2, "both canonical rows survive the metadata failure");
        let orchestrator_core::ChatOperationBegin::Replay(receipt) = operation_store.begin(request).unwrap() else {
            panic!("the durable assistant must reconcile as completed");
        };
        assert_eq!(receipt.status, orchestrator_core::ChatOperationStatus::Completed);
        assert_eq!(receipt.assistant_seq, Some(1));
        assert!(sink.events.iter().any(|event| matches!(
            event,
            ChatStreamEvent::TurnCompleted { status: orchestrator_core::ChatOperationStatus::Completed, seq: 1, .. }
        )));
        assert!(!sink.events.iter().any(|event| matches!(event, ChatStreamEvent::TurnFailed { .. })));
    }

    #[tokio::test]
    async fn reservation_cas_failure_releases_pending_authority_immediately() {
        let tmp = tempfile::tempdir().unwrap();
        let store = FailMetaSaveStore { inner: store_for(&tmp), save_calls: AtomicUsize::new(0), fail_on_call: 1 };
        store.create(Some("c1".into())).unwrap();
        let operation_store =
            orchestrator_core::ChatOperationStore::at_path(tmp.path().join("chat-operations.db"), "test-repo");
        let request = operation_store.request("workspace-a", "alice", "c1", "key-cas-fail", "request-hash");
        let orchestrator_core::ChatOperationBegin::Acquired(claim) = operation_store.begin(request.clone()).unwrap()
        else {
            panic!("first operation should acquire");
        };
        let mut operation = ChatTurnOperation::new(ChatOperationAuthority::Local(operation_store.clone()), claim);
        let producer = MockProducer::new(vec![text_turn("must-not-run", "sess-1")]);
        let mut sink = CapturingSink::new();
        let mut context = ctx("c1", "claude", "hello", &tmp);
        context.operation = Some(&mut operation);
        context.execution_hash = Some("execution-hash");

        let error = run_turn(&producer, &store, &mut sink, context).await.unwrap_err();
        assert!(error.to_string().contains("injected metadata persistence failure"));
        assert!(producer.requests().is_empty());
        assert!(store.load_messages("c1").unwrap().is_empty());
        assert_eq!(store.load_meta("c1").unwrap().unwrap().active_operation_id, None);
        assert!(matches!(operation_store.begin(request).unwrap(), orchestrator_core::ChatOperationBegin::Acquired(_)));
    }

    #[tokio::test]
    async fn user_append_failure_clears_reservation_and_releases_pending_authority() {
        let tmp = tempfile::tempdir().unwrap();
        let store = FailAppendOnceStore { inner: store_for(&tmp), append_calls: AtomicUsize::new(0) };
        store.create(Some("c1".into())).unwrap();
        let operation_store =
            orchestrator_core::ChatOperationStore::at_path(tmp.path().join("chat-operations.db"), "test-repo");
        let request = operation_store.request("workspace-a", "alice", "c1", "key-append-fail", "request-hash");
        let orchestrator_core::ChatOperationBegin::Acquired(claim) = operation_store.begin(request.clone()).unwrap()
        else {
            panic!("first operation should acquire");
        };
        let mut operation = ChatTurnOperation::new(ChatOperationAuthority::Local(operation_store.clone()), claim);
        let producer = MockProducer::new(vec![text_turn("must-not-run", "sess-1")]);
        let mut sink = CapturingSink::new();
        let mut context = ctx("c1", "claude", "hello", &tmp);
        context.operation = Some(&mut operation);
        context.execution_hash = Some("execution-hash");

        let error = run_turn(&producer, &store, &mut sink, context).await.unwrap_err();
        assert!(error.to_string().contains("injected user append failure"));
        assert!(producer.requests().is_empty());
        assert!(store.load_messages("c1").unwrap().is_empty());
        assert_eq!(store.load_meta("c1").unwrap().unwrap().active_operation_id, None);
        assert!(matches!(operation_store.begin(request).unwrap(), orchestrator_core::ChatOperationBegin::Acquired(_)));
    }

    #[tokio::test]
    async fn lost_accept_response_terminalizes_durable_user_without_provider_repetition() {
        let tmp = tempfile::tempdir().unwrap();
        let store = store_for(&tmp);
        store.create(Some("c1".into())).unwrap();
        let operation_store =
            orchestrator_core::ChatOperationStore::at_path(tmp.path().join("chat-operations.db"), "test-repo");
        let request = operation_store.request("workspace-a", "alice", "c1", "key-accept-fail", "request-hash");
        let orchestrator_core::ChatOperationBegin::Acquired(claim) = operation_store.begin(request.clone()).unwrap()
        else {
            panic!("first operation should acquire");
        };
        let mut operation = ChatTurnOperation::new(ChatOperationAuthority::Local(operation_store.clone()), claim);
        operation.inject_lost_user_accept_response();
        let producer = MockProducer::new(vec![text_turn("must-not-run", "sess-1")]);
        let mut sink = CapturingSink::new();
        let mut context = ctx("c1", "claude", "hello", &tmp);
        context.operation = Some(&mut operation);
        context.execution_hash = Some("execution-hash");

        let error = run_turn(&producer, &store, &mut sink, context).await.unwrap_err();
        assert!(error.to_string().contains("injected lost user-accept response"));
        assert!(producer.requests().is_empty(), "ambiguous acceptance must not start the provider");
        let messages = store.load_messages("c1").unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].role, ChatRole::User);
        assert_eq!(store.load_meta("c1").unwrap().unwrap().active_operation_id, None);
        let orchestrator_core::ChatOperationBegin::Replay(receipt) = operation_store.begin(request).unwrap() else {
            panic!("accepted user must have a terminal receipt");
        };
        assert_eq!(receipt.status, orchestrator_core::ChatOperationStatus::AssistantInterrupted);
        assert_eq!(receipt.user_seq, Some(0));
    }

    #[tokio::test]
    async fn idempotent_provider_failure_is_terminal_partial_success() {
        let tmp = tempfile::tempdir().unwrap();
        let store = store_for(&tmp);
        store.create(Some("c1".into())).unwrap();
        let operation_store =
            orchestrator_core::ChatOperationStore::at_path(tmp.path().join("chat-operations.db"), "test-repo");
        let request = operation_store.request("workspace-a", "alice", "c1", "key-fail", "request-hash");
        let orchestrator_core::ChatOperationBegin::Acquired(claim) = operation_store.begin(request.clone()).unwrap()
        else {
            panic!("first operation should acquire");
        };
        let mut operation = ChatTurnOperation::new(ChatOperationAuthority::Local(operation_store.clone()), claim);
        let producer = MockProducer::new(vec![(
            vec![SessionEvent::Error { message: "provider exploded".into(), recoverable: false }],
            None,
        )]);
        let mut sink = CapturingSink::new();
        let mut context = ctx("c1", "claude", "hello", &tmp);
        context.operation = Some(&mut operation);
        context.execution_hash = Some("execution-hash");

        assert!(run_turn(&producer, &store, &mut sink, context).await.is_err());
        let messages = store.load_messages("c1").unwrap();
        assert_eq!(messages.len(), 1, "the accepted user row remains canonical");
        let orchestrator_core::ChatOperationBegin::Replay(receipt) = operation_store.begin(request).unwrap() else {
            panic!("failed operation should replay its terminal receipt");
        };
        assert_eq!(receipt.status, orchestrator_core::ChatOperationStatus::AssistantFailed);
        assert_eq!(receipt.user_seq, Some(0));
        assert_eq!(receipt.assistant_seq, None);
        assert!(sink.events.iter().any(|event| matches!(
            event,
            ChatStreamEvent::TurnFailed {
                status: orchestrator_core::ChatOperationStatus::AssistantFailed,
                user_seq: 0,
                ..
            }
        )));
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
                                agent_id: None,
                                expected_revision: None,
                                title_update: None,
                                tool: "claude",
                                model: "claude-sonnet-4-6",
                                user_message: &message,
                                cwd: dir.clone(),
                                project_root: dir,
                                reasoning_effort: None,
                                permission_mode: None,
                                approvals: false,
                                agent_system_prompt: None,
                                agent_tool_profile: None,
                                mcp_contract: None,
                                isolated_mcp_config_path: None,
                                skill: None,
                                operation: None,
                                execution_hash: None,
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

    fn prompt_skill() -> SkillApplicationResult {
        SkillApplicationResult {
            system_prompt_fragments: vec!["You are skill-guided.".to_string()],
            prompt_prefixes: vec!["PREFIX-TEXT".to_string()],
            ..Default::default()
        }
    }

    fn launch_skill() -> SkillApplicationResult {
        SkillApplicationResult {
            extra_args: vec!["--strict-mcp-config".to_string()],
            env: std::collections::BTreeMap::from([("SKILL_MODE".to_string(), "on".to_string())]),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn skill_prompt_and_system_prompt_apply_on_every_turn_without_breaking_resume() {
        let tmp = tempfile::tempdir().unwrap();
        let store = store_for(&tmp);
        store.create(Some("c1".into())).unwrap();
        let producer = MockProducer::new(vec![text_turn("a1", "sess-1"), text_turn("a2", "sess-1")]);
        let skill = prompt_skill();

        let mut sink = CapturingSink::new();
        let first = TurnContext { skill: Some(&skill), ..ctx("c1", "claude", "q1", &tmp) };
        run_turn(&producer, &store, &mut sink, first).await.unwrap();
        let mut sink2 = CapturingSink::new();
        let second = TurnContext { skill: Some(&skill), ..ctx("c1", "claude", "q2", &tmp) };
        run_turn(&producer, &store, &mut sink2, second).await.unwrap();

        let reqs = producer.requests();
        assert_eq!(reqs.len(), 2);
        for (index, request) in reqs.iter().enumerate() {
            assert!(
                request.prompt.starts_with("PREFIX-TEXT\n\n"),
                "turn {index} prompt must carry the skill prefix; prompt: {}",
                request.prompt
            );
            assert_eq!(
                request.extras.pointer("/system_prompt").and_then(Value::as_str),
                Some("You are skill-guided."),
                "turn {index} must carry the skill system prompt"
            );
        }
        // A prompt-only skill does NOT disturb continuity: the second turn
        // still resumes with the (wrapped) new message only.
        assert_eq!(producer.resume_ids()[1].as_deref(), Some("sess-1"));
        assert!(reqs[1].prompt.ends_with("q2"), "resumed prompt is the wrapped message only: {}", reqs[1].prompt);
        assert!(!reqs[1].prompt.contains("q1"), "resumed prompt must not replay history");
    }

    #[tokio::test]
    async fn launch_affecting_skill_forces_replay_and_grafts_the_launch_contract() {
        let tmp = tempfile::tempdir().unwrap();
        let store = store_for(&tmp);
        store.create(Some("c1".into())).unwrap();
        let producer = MockProducer::new(vec![text_turn("a1", "sess-1"), text_turn("a2", "sess-1")]);
        let skill = launch_skill();

        let mut sink = CapturingSink::new();
        let first = TurnContext { skill: Some(&skill), ..ctx("c1", "claude", "q1", &tmp) };
        run_turn(&producer, &store, &mut sink, first).await.unwrap();
        let mut sink2 = CapturingSink::new();
        let second = TurnContext { skill: Some(&skill), ..ctx("c1", "claude", "q2", &tmp) };
        run_turn(&producer, &store, &mut sink2, second).await.unwrap();

        let reqs = producer.requests();
        assert_eq!(reqs.len(), 2);
        for (index, request) in reqs.iter().enumerate() {
            let args: Vec<&str> = request
                .extras
                .pointer("/runtime_contract/cli/launch/args")
                .and_then(Value::as_array)
                .unwrap_or_else(|| panic!("turn {index} must graft cli.launch; extras: {}", request.extras))
                .iter()
                .filter_map(Value::as_str)
                .collect();
            assert!(args.contains(&"--strict-mcp-config"), "turn {index} launch args: {args:?}");
            assert_eq!(args.last().copied(), Some(request.prompt.as_str()), "launch carries this turn's prompt");
            assert_eq!(
                request.env_vars,
                vec![("SKILL_MODE".to_string(), "on".to_string())],
                "turn {index} must carry the skill env"
            );
        }
        // Launch-affecting skills force the replay path: no resume seam, no
        // extras.session_id, full history in the second turn's prompt.
        assert_eq!(producer.resume_ids()[1], None, "launch-affecting skill must not resume");
        assert!(reqs[1].extras.get("session_id").is_none());
        assert!(reqs[1].prompt.contains("User: q1"), "second turn must replay history: {}", reqs[1].prompt);
        let resumed = sink2.events.iter().find_map(|e| match e {
            ChatStreamEvent::TurnStarted { resumed, .. } => Some(*resumed),
            _ => None,
        });
        assert_eq!(resumed, Some(false));
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
