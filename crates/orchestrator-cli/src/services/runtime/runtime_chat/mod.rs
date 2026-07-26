//! `animus chat` — multi-turn conversations with a provider tool.
//!
//! See [`turn`] for the continuity model (providers own continuity; Animus
//! owns a thin portable/fallback layer) and `docs/reference/chat.md`.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};

use anyhow::Context;

use crate::shared::{canonicalize_cwd_in_project, format_age, print_ok, print_value, render_table};
use crate::{
    ChatCommand, ChatDeleteArgs, ChatExportArgs, ChatExportFormat, ChatGetArgs, ChatListArgs, ChatNewArgs,
    ChatRenameArgs, ChatSearchArgs, ChatSendArgs, ChatVisibilityArg,
};
use serde::Serialize;

pub(crate) mod client;
pub(crate) mod idempotency;
pub(crate) mod sink;
pub(crate) mod store;
pub(crate) mod turn;

use client::ConversationStoreClient;
use sink::{ChatStreamEvent, ChatStreamSink, JsonlStdoutSink, NullSink, TextStdoutSink};
use store::{ChatMessage, ChatRole, ConversationMeta, ConversationStore, TurnBlock, Visibility};
use turn::{run_turn, ResolverTurnProducer, TurnContext};

pub(crate) async fn handle_chat(command: ChatCommand, project_root: &str, json: bool) -> Result<()> {
    match command {
        ChatCommand::Capabilities => handle_chat_capabilities(json),
        ChatCommand::New(args) => handle_chat_new(args, project_root, json),
        ChatCommand::Send(args) => handle_chat_send(args, project_root, json).await,
        ChatCommand::Get(args) => handle_chat_get(args, project_root, json),
        ChatCommand::List(args) => handle_chat_list(args, project_root, json),
        ChatCommand::Rename(args) => handle_chat_rename(args, project_root, json),
        ChatCommand::Delete(args) => handle_chat_delete(args, project_root, json),
        ChatCommand::Export(args) => handle_chat_export(args, project_root, json),
        ChatCommand::Search(args) => handle_chat_search(args, project_root, json),
    }
}

fn handle_chat_capabilities(json: bool) -> Result<()> {
    print_value(
        serde_json::json!({
            "schema": "animus.chat.capabilities.v1",
            "send": {
                "durable_idempotency": {
                    "supported": true,
                    "flag": "--idempotency-key",
                    "max_key_bytes": orchestrator_core::MAX_CHAT_IDEMPOTENCY_KEY_BYTES,
                    "requires": ["--conversation", "--actor-json", "--as-user"],
                    "scope": ["repository", "workspace", "actor", "conversation"],
                },
                "identity_binding": {
                    "supported": true,
                    "agent_field": "agent_id",
                    "revision_field": "revision",
                    "agent_flag": "--agent",
                    "expected_revision_flag": "--expected-revision",
                    "client_selectable": false,
                },
                "partial_success": {
                    "supported": true,
                    "jsonl_events": ["user_message_accepted", "turn_completed", "turn_failed"],
                    "terminal_statuses": ["completed", "assistant_failed", "assistant_interrupted"],
                    "canonical_fields": [
                        "operation_id", "conversation_id", "user_message_id", "user_seq",
                        "message_id", "seq"
                    ],
                }
            }
        }),
        json,
    )
}

#[derive(Debug, Serialize, PartialEq)]
pub(crate) struct SearchMatch {
    conversation_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    title: Option<String>,
    role: &'static str,
    seq: u64,
    snippet: String,
}

fn role_str(role: ChatRole) -> &'static str {
    match role {
        ChatRole::User => "user",
        ChatRole::Assistant => "assistant",
    }
}

/// Return a trimmed, ellipsized preview window around the first match of
/// `query` in `content`, or `None` when there is no match. Case-insensitive
/// matching maps the folded match offset back to an original-content char
/// index by walking chars and counting their lowercase expansions, so the
/// preview window stays anchored on the match even when case-folding changes
/// lengths — never panics (slice bounds are clamped).
fn snippet_around(content: &str, query: &str, case_insensitive: bool) -> Option<String> {
    if query.is_empty() {
        return None;
    }
    let (hay, needle) = if case_insensitive {
        (content.to_lowercase(), query.to_lowercase())
    } else {
        (content.to_string(), query.to_string())
    };
    let byte_pos = hay.find(&needle)?;
    let chars: Vec<char> = content.chars().collect();
    let folded_idx = hay[..byte_pos].chars().count();
    let char_idx = if case_insensitive {
        let mut folded_seen = 0usize;
        let mut mapped = chars.len();
        for (i, c) in chars.iter().enumerate() {
            if folded_seen >= folded_idx {
                mapped = i;
                break;
            }
            folded_seen += c.to_lowercase().count();
        }
        mapped
    } else {
        folded_idx.min(chars.len())
    };
    const PAD: usize = 30;
    let end = (char_idx + query.chars().count() + PAD).min(chars.len());
    let start = char_idx.saturating_sub(PAD).min(end);
    let core: String = chars[start..end].iter().collect();
    let mut s = core.split_whitespace().collect::<Vec<_>>().join(" ");
    if start > 0 {
        s.insert(0, '…');
    }
    if end < chars.len() {
        s.push('…');
    }
    Some(s)
}

/// Scan conversations (newest-first) for `query`, collecting up to `limit`
/// matches with a preview snippet. When `as_user` is `Some`, only that user's
/// own conversations plus shared ones are searched, so a multi-user
/// `conversation_store` backend never leaks snippets from another user's
/// private conversations through search.
fn search_conversations(
    store: &ConversationStoreClient,
    query: &str,
    case_insensitive: bool,
    limit: usize,
    as_user: Option<&str>,
) -> Result<Vec<SearchMatch>> {
    let mut out = Vec::new();
    if query.is_empty() {
        return Ok(out);
    }
    for summary in store.list_for_user(as_user)? {
        if out.len() >= limit {
            break;
        }
        for m in store.load_messages(&summary.id)? {
            if out.len() >= limit {
                break;
            }
            if let Some(snippet) = snippet_around(&m.content, query, case_insensitive) {
                out.push(SearchMatch {
                    conversation_id: summary.id.clone(),
                    title: summary.title.clone(),
                    role: role_str(m.role),
                    seq: m.seq,
                    snippet,
                });
            }
        }
    }
    Ok(out)
}

fn handle_chat_search(args: ChatSearchArgs, project_root: &str, json: bool) -> Result<()> {
    let store = ConversationStoreClient::for_project_as_user(Path::new(project_root), args.as_user.as_deref())?;
    let matches = store.search(&args.query, !args.case_sensitive, args.limit, args.as_user.as_deref())?;
    print_value(matches, json)
}

/// Render a conversation transcript as Markdown — title, a metadata line, then
/// each turn with a role heading, its prose, and a compact "Tools" summary.
fn render_markdown(meta: &ConversationMeta, messages: &[ChatMessage]) -> String {
    use std::fmt::Write;
    let mut out = String::new();
    let title = meta.title.clone().unwrap_or_else(|| meta.id.clone());
    let tool = meta.tool.as_deref().unwrap_or("?");
    let model = meta.model.as_deref().unwrap_or("?");
    let _ = writeln!(out, "# {title}\n");
    let _ = writeln!(
        out,
        "> tool: {tool} · model: {model} · {} messages · updated {}\n",
        meta.message_count, meta.updated_at
    );
    for m in messages {
        match m.role {
            ChatRole::User => {
                let _ = writeln!(out, "### 🧑 You\n");
            }
            ChatRole::Assistant => {
                let t = m.tool.as_deref().unwrap_or(tool);
                let md = m.model.as_deref().unwrap_or(model);
                let _ = writeln!(out, "### 🤖 Assistant · {t}/{md}\n");
            }
        }
        let _ = writeln!(out, "{}\n", m.content.trim());
        let tools: Vec<&str> = m
            .blocks
            .iter()
            .filter_map(|b| match b {
                TurnBlock::ToolCall { tool_name, .. } => tool_name.as_deref(),
                _ => None,
            })
            .collect();
        if !tools.is_empty() {
            let _ = writeln!(out, "_Tools: {}_\n", tools.join(", "));
        }
        // Per-turn token usage + cost footer, when the provider reported it.
        let mut footer: Vec<String> = Vec::new();
        if let Some(usage) = &m.usage {
            footer.push(format!("{} in · {} out tokens", usage.input, usage.output));
        }
        if let Some(cost) = m.cost_usd {
            if cost > 0.0 {
                footer.push(format!("${cost:.4}"));
            }
        }
        if !footer.is_empty() {
            let _ = writeln!(out, "_{}_\n", footer.join(" · "));
        }
    }
    out
}

fn handle_chat_export(args: ChatExportArgs, project_root: &str, json: bool) -> Result<()> {
    let store = ConversationStoreClient::for_project_as_user(Path::new(project_root), args.as_user.as_deref())?;
    let meta = store.load_meta(&args.id)?.ok_or_else(|| anyhow!("conversation '{}' not found", args.id))?;
    client::ensure_user_may_access(&meta, &args.id, args.as_user.as_deref())?;
    let messages = store.load_messages(&args.id)?;
    let (content, format_label) = match args.format {
        ChatExportFormat::Json => {
            (serde_json::to_string_pretty(&serde_json::json!({ "meta": meta, "messages": messages }))?, "json")
        }
        ChatExportFormat::Markdown => (render_markdown(&meta, &messages), "markdown"),
    };
    match args.output.as_deref() {
        Some(path) => {
            std::fs::write(path, &content).with_context(|| format!("writing {path}"))?;
            print_value(
                serde_json::json!({
                    "conversation_id": meta.id,
                    "format": format_label,
                    "output": path,
                    "bytes": content.len(),
                }),
                json,
            )
        }
        None => {
            // Raw transcript to stdout — the content IS the output here, so we
            // bypass the `animus.cli.v1` envelope regardless of `--json`.
            println!("{content}");
            Ok(())
        }
    }
}

/// Set (or clear) a conversation's title if `title` is `Some`. A no-op when
/// `title` is `None` or the conversation is missing. An empty/whitespace title
/// clears it back to `None`.
#[cfg(test)]
fn apply_conversation_title(store: &impl ConversationStore, id: &str, title: Option<&str>) -> Result<()> {
    let Some(title) = normalize_title_update(title) else { return Ok(()) };
    let Some(mut meta) = store.load_meta(id)? else { return Ok(()) };
    meta.title = title;
    save_meta_update(store, &mut meta)
}

pub(super) fn normalize_title_update(title: Option<&str>) -> Option<Option<String>> {
    title.map(|value| {
        let trimmed = value.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_string())
    })
}

fn save_meta_update(store: &impl ConversationStore, meta: &mut ConversationMeta) -> Result<()> {
    let expected = meta.revision;
    meta.revision =
        meta.revision.checked_add(1).ok_or_else(|| anyhow!("conversation '{}' revision exhausted", meta.id))?;
    store.save_meta_if_revision(meta, Some(expected))
}

fn handle_chat_rename(args: ChatRenameArgs, project_root: &str, json: bool) -> Result<()> {
    let store = ConversationStoreClient::for_project_as_user(Path::new(project_root), args.as_user.as_deref())?;
    let _lock = loop {
        if let Some(lock) = store.try_lock_conversation(&args.id)? {
            break lock;
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    };
    let mut meta = store.load_meta(&args.id)?.ok_or_else(|| anyhow!("conversation '{}' not found", args.id))?;
    client::ensure_user_may_access(&meta, &args.id, args.as_user.as_deref())?;
    meta.title = normalize_title_update(Some(&args.title)).expect("rename always supplies a title update");
    save_meta_update(&store, &mut meta)?;
    print_value(serde_json::json!({ "conversation_id": meta.id, "title": meta.title }), json)
}

fn handle_chat_delete(args: ChatDeleteArgs, project_root: &str, json: bool) -> Result<()> {
    let store = ConversationStoreClient::for_project_as_user(Path::new(project_root), args.as_user.as_deref())?;
    // Authorize against the loaded meta before deleting: a user-scoped delete of
    // another user's private conversation is rejected as "not found". A missing
    // (or auth-hidden) conversation stays idempotent: with no meta there is
    // nothing authorized to remove, so we skip the mutation entirely rather than
    // issue a delete the backend might surface as a forbidden error.
    let existed = match store.load_meta(&args.id)? {
        Some(meta) => {
            client::ensure_user_may_access(&meta, &args.id, args.as_user.as_deref())?;
            store.delete(&args.id)?;
            true
        }
        None => false,
    };
    print_value(serde_json::json!({ "conversation_id": args.id, "deleted": existed }), json)
}

/// Map the CLI visibility flag to the store's [`Visibility`].
fn visibility_from_arg(arg: ChatVisibilityArg) -> Visibility {
    match arg {
        ChatVisibilityArg::Private => Visibility::Private,
        ChatVisibilityArg::Shared => Visibility::Shared,
    }
}

fn handle_chat_new(args: ChatNewArgs, project_root: &str, json: bool) -> Result<()> {
    // Build the client with the acting user so a plugin backend authorizes the
    // follow-up title `save_meta` as the same owner the create stamped, not as
    // an unscoped/admin mutation.
    let store = ConversationStoreClient::for_project_as_user(Path::new(project_root), args.as_user.as_deref())?;
    let mut meta = store.create_with_ownership(args.id, args.as_user.clone(), visibility_from_arg(args.visibility))?;
    if let Some(title) = normalize_title_update(args.title.as_deref()) {
        meta.title = title;
        save_meta_update(&store, &mut meta)?;
    }
    print_value(
        serde_json::json!({
            "conversation_id": meta.id,
            "title": meta.title,
            "owner": meta.owner,
            "visibility": meta.visibility,
        }),
        json,
    )
}

fn resolve_chat_agent(
    project_root: &Path,
    actor: Option<&animus_actor::Actor>,
    stored_agent_id: Option<&str>,
    requested_agent_id: Option<&str>,
) -> Result<Option<(String, orchestrator_config::agent_runtime_config::AgentProfile)>> {
    let resolve = |id: &str| {
        crate::services::runtime::agent_mcp::resolve_canonical_agent_profile(project_root, id, actor).map_err(|error| {
            crate::conflict_error(format!(
                "chat_precondition_failed:binding_unavailable: canonical agent '{id}' is unavailable: {error}"
            ))
        })
    };

    match stored_agent_id {
        Some(stored) => {
            let (canonical, profile) = resolve(stored)?;
            if canonical != stored {
                return Err(crate::conflict_error(format!(
                    "chat_precondition_failed:binding_conflict: conversation has non-canonical agent binding '{stored}' (configured id is '{canonical}')"
                )));
            }
            if let Some(requested) = requested_agent_id {
                let (requested_canonical, _) = resolve(requested)?;
                if requested_canonical != canonical {
                    return Err(crate::conflict_error(format!(
                        "chat_precondition_failed:binding_conflict: conversation is bound to agent '{canonical}', not '{requested_canonical}'"
                    )));
                }
            }
            Ok(Some((canonical, profile)))
        }
        None => requested_agent_id.map(resolve).transpose(),
    }
}

async fn handle_chat_send(args: ChatSendArgs, project_root: &str, json: bool) -> Result<()> {
    let project_root_path = PathBuf::from(project_root);

    // Transport-asserted authz identity for this turn. The transport (e.g. the
    // portal) authenticates the user, then passes `--actor-json`; the kernel
    // relays it (it does not validate the claims). This is the authz identity,
    // distinct from `--as-user` (conversation ownership). Parsed FIRST: a
    // malformed value must fail closed BEFORE any store write (conversation
    // create / title rename), so an invalid assertion leaves no state behind.
    // Omitted => `None` => global scope.
    let actor = crate::shared::parse_actor_json_flag(args.actor_json.as_deref())?;

    if args.idempotency_key.is_some() {
        let asserted_user = actor
            .as_ref()
            .map(|value| value.user_id.as_str())
            .ok_or_else(|| crate::invalid_input_error("idempotent chat send requires --actor-json"))?;
        let acting_user = args
            .as_user
            .as_deref()
            .ok_or_else(|| crate::invalid_input_error("idempotent chat send requires --as-user"))?;
        if asserted_user != acting_user {
            return Err(crate::invalid_input_error(
                "idempotent chat send requires --as-user to equal actor_json.user_id",
            ));
        }
    }

    let store = ConversationStoreClient::for_project_as_user(&project_root_path, args.as_user.as_deref())?;

    // Read and authorize existing metadata before resolving any profile. A
    // hidden conversation stays indistinguishable from a missing one.
    let existing_meta = match args.conversation.as_deref() {
        Some(id) => match store.load_meta(id)? {
            Some(meta) => {
                client::ensure_user_may_access(&meta, id, args.as_user.as_deref())?;
                Some(meta)
            }
            None => return Err(anyhow!("conversation '{id}' not found; create it with `animus chat new`")),
        },
        None => None,
    };

    let raw_cwd = args.cwd.clone().unwrap_or_else(|| project_root.to_string());
    let cwd = PathBuf::from(canonicalize_cwd_in_project(&raw_cwd, project_root)?);
    let normalized_title = normalize_title_update(args.title.as_deref());

    // Build the response sink before consulting the journal so a terminal
    // replay never depends on mutable agent profiles, skills, or MCP config.
    let mut sink: Box<dyn ChatStreamSink> = if json {
        Box::new(JsonlStdoutSink)
    } else if args.stream {
        Box::new(TextStdoutSink)
    } else {
        Box::new(NullSink)
    };

    // Admit keyed application calls from caller-controlled, normalized inputs
    // before resolving mutable execution configuration. Terminal outcomes can
    // therefore replay even if the bound profile was later removed. Pending
    // claims bind a second resolved-execution hash below.
    let mut turn_operation = if let Some(caller_key) = args.idempotency_key.clone() {
        let actor =
            actor.as_ref().ok_or_else(|| crate::invalid_input_error("idempotent chat send requires --actor-json"))?;
        let conversation_id = args
            .conversation
            .as_deref()
            .ok_or_else(|| crate::invalid_input_error("idempotent chat send requires --conversation"))?;
        let caller_hash = idempotency::effective_request_hash(serde_json::json!({
            "version": 3,
            "conversation_id": conversation_id,
            "message": args.message,
            "tool_override": args.tool,
            "model_override": args.model,
            "cwd": cwd,
            "reasoning_effort_override": args.reasoning_effort.map(|value| value.as_str()),
            "permission_mode_override": args.permission_mode,
            "approvals": args.approvals,
            "title_update": normalized_title,
            "requested_agent_id": args.agent,
            "expected_revision": args.expected_revision,
            "skill": args.skill,
            "mcp_servers": args.mcp_server,
            "no_animus_mcp": args.no_animus_mcp,
            "as_user": args.as_user,
        }))?;
        let (operation_store, begin) =
            idempotency::begin(&project_root_path, actor, conversation_id, caller_key, caller_hash)?;
        match begin {
            orchestrator_core::ChatOperationBegin::Conflict => {
                return Err(crate::conflict_error(
                    "idempotency_conflict: key was already used for a different chat request",
                ));
            }
            orchestrator_core::ChatOperationBegin::InProgress => {
                return Err(crate::conflict_error(
                    "idempotency_in_progress: a chat send with this key is still pending",
                ));
            }
            orchestrator_core::ChatOperationBegin::Replay(receipt) => {
                turn::clear_operation_reservation(&store, conversation_id, &receipt.operation_id).await?;
                emit_operation_receipt(sink.as_mut(), &receipt)?;
                return replay_result(&receipt);
            }
            orchestrator_core::ChatOperationBegin::Acquired(claim)
                if claim.recovered && claim.status == orchestrator_core::ChatOperationStatus::UserAccepted =>
            {
                let receipt = turn::reconcile_recovered_accepted(&store, &operation_store, &claim).await?;
                emit_operation_receipt(sink.as_mut(), &receipt)?;
                return replay_result(&receipt);
            }
            orchestrator_core::ChatOperationBegin::Acquired(claim) => {
                Some(idempotency::ChatTurnOperation::new(operation_store, claim))
            }
        }
    } else {
        None
    };

    // Once a keyed caller has acquired a durable admission, every fallible
    // preparation step must reconcile it under the conversation lock. This
    // prevents a deleted profile or invalid MCP configuration from leaving a
    // stale active_operation_id that blocks unrelated future sends.
    macro_rules! prepare_chat_turn {
        ($expression:expr) => {
            match $expression {
                Ok(value) => value,
                Err(error) => {
                    if let Some(operation) = turn_operation.as_mut() {
                        if let Some(receipt) =
                            turn::reconcile_pre_execution_failure(&store, operation, &args.message).await?
                        {
                            emit_operation_receipt(sink.as_mut(), &receipt)?;
                        }
                    }
                    return Err(error);
                }
            }
        };
    }

    // A bound conversation always re-resolves its persisted canonical profile,
    // even when this invocation omits --agent. Deleted/renamed/actor-hidden
    // profiles therefore fail closed instead of silently becoming a default
    // agent. An explicit different profile is a hard conflict.
    let resolved_agent = prepare_chat_turn!(resolve_chat_agent(
        &project_root_path,
        actor.as_ref(),
        existing_meta.as_ref().and_then(|meta| meta.agent_id.as_deref()),
        args.agent.as_deref(),
    ));
    let agent_id = resolved_agent.as_ref().map(|(id, _)| id.as_str());
    let agent_profile = resolved_agent.as_ref().map(|(_, profile)| profile);

    // Explicit provider/model flags remain per-turn overrides. Otherwise a
    // bound conversation uses its canonical profile config on every start,
    // native resume, and full-history fallback.
    let tool = args
        .tool
        .clone()
        .or_else(|| agent_profile.and_then(|profile| profile.tool.clone()))
        .unwrap_or_else(|| "claude".to_string());

    // Resolve the per-agent MCP server set (profile ∪ skill ∪ --mcp-server
    // additions − the built-in animus when --no-animus-mcp) ONCE for this
    // send invocation. The same resolution also carries the skill's full
    // application (prompt fragments, env, extra_args, model preference, ...).
    let scope = prepare_chat_turn!(crate::services::runtime::agent_mcp::resolve_agent_scope(
        &project_root_path,
        &tool,
        agent_id,
        args.skill.as_deref(),
        actor.as_ref(),
    ));
    // The skill's FULL application binds to this `chat send` invocation and
    // is applied per turn by the turn loop (same lifecycle as the MCP
    // contract below).
    let skill_application = scope.skill_application.as_ref().filter(|skill| !skill.is_empty());

    // Model precedence: explicit --model > skill preference > bound profile >
    // compiled tool default.
    let model = args
        .model
        .clone()
        .or_else(|| skill_application.and_then(|skill| skill.model.clone()))
        .or_else(|| agent_profile.and_then(|profile| profile.model.clone()))
        .unwrap_or_else(|| protocol::default_model_for_tool(&tool).unwrap_or("claude-sonnet-4-6").to_string());

    // Permission mode: the `--permission-mode` flag wins over the selected
    // `--agent` profile's `permission_mode`. Provider-specific and forwarded
    // verbatim; an unknown value only warns (stderr), never blocks.
    let permission_mode =
        args.permission_mode.clone().or_else(|| agent_profile.and_then(|profile| profile.permission_mode.clone()));
    if let Some(mode) = permission_mode.as_deref() {
        crate::services::runtime::runtime_agent::provider_client::warn_unknown_permission_mode(mode);
    }

    // Kernel-mediated approvals: the `--approvals` flag or an
    // `approval_policy` on the selected `--agent` profile sets
    // `extras.approvals = true` on every turn's session request.
    let approvals = args.approvals || agent_profile.is_some_and(|profile| profile.approval_policy.is_some());
    crate::services::runtime::runtime_agent::provider_client::warn_if_claude_autoapprove_bypass(&tool, approvals);

    let producer = ResolverTurnProducer::for_project(&project_root_path);

    // Assemble the runtime contract the provider receives so the chat agent
    // sees the MCP servers its profile/skill declares. Plain chat (no
    // --agent/--skill) defaults to the built-in `animus` server only. When an
    // actor is asserted, the spawned `animus mcp serve` child is bound to it.
    let scope_selected = agent_id.is_some() || args.skill.is_some();
    let mcp_contract = prepare_chat_turn!(crate::services::runtime::agent_mcp::assemble_agent_mcp_contract_with_actor(
        &project_root_path,
        &tool,
        &model,
        &scope.profile_servers,
        &scope.skill_servers,
        &args.mcp_server,
        &scope.tool_policy,
        scope_selected,
        args.no_animus_mcp,
        agent_id,
        actor.as_ref(),
    ));

    // Provider CLIs that auto-discover a cwd-local `.mcp.json` (claude-code)
    // register MCP servers from that file, not the runtime contract, so the
    // per-agent set is also materialized there. The merge is additive — it
    // upserts only the resolved Animus-scoped names and preserves any
    // user-authored entries.
    //
    // The transport-asserted actor is NEVER written to this shared, persisted,
    // auto-discovered file: a concurrent invocation in the same cwd, or a crash
    // before any cleanup, would let another caller inherit the per-turn identity
    // (incl. `admin` claims) and run MCP tools as that user. The actor instead
    // rides the ephemeral `extras.runtime_contract` (below) for the turn's own
    // provider session, so contract-consuming providers are still scoped.
    //
    // Providers that scope ONLY off the cwd `.mcp.json` (and ignore the
    // runtime contract's `mcp` block) would otherwise run the auto-discovered
    // `animus` server unscoped. We close that gap WITHOUT persisting the
    // identity into the shared file: when an actor is asserted we ALSO
    // materialize the FULL (actor-pinned) contract into a per-run ISOLATED
    // directory and surface its `.mcp.json` path on `extras.mcp_config_path`
    // (see [`run_turn`]). A provider that locates servers by file
    // auto-discovery can be pointed at this run-private file (e.g.
    // claude-code's `--mcp-config`) so the actor reaches that channel too. The
    // isolated dir is held alive (`_isolated_mcp_dir`) for the whole turn and
    // cleaned on drop.
    //
    // Consuming `extras.mcp_config_path` (passing `--mcp-config <path>` to the
    // provider CLI) is provider-launch plumbing that lives in the provider
    // plugin; until a plugin honors it, contract-consuming providers remain
    // scoped via `extras.runtime_contract` and the gap is closed only for
    // plugins that adopt the path. This is the documented out-of-tree tail.
    let mut isolated_mcp_config_path: Option<PathBuf> = None;
    let mut _isolated_mcp_dir: Option<tempfile::TempDir> = None;
    if let Some(contract) = mcp_contract.as_ref() {
        let for_disk = if actor.is_some() {
            std::borrow::Cow::Owned(crate::services::runtime::agent_mcp::strip_actor_from_contract(contract))
        } else {
            std::borrow::Cow::Borrowed(contract)
        };
        prepare_chat_turn!(crate::services::runtime::agent_mcp::materialize_mcp_json(&cwd, &for_disk));

        if actor.is_some() {
            let dir = prepare_chat_turn!(tempfile::Builder::new()
                .prefix("animus-mcp-actor-")
                .tempdir()
                .context("failed to create isolated MCP config dir for actor-scoped run"));
            isolated_mcp_config_path = prepare_chat_turn!(
                crate::services::runtime::agent_mcp::materialize_isolated_mcp_json(dir.path(), contract)
            );
            _isolated_mcp_dir = Some(dir);
        }
    }

    // Create only after the selected/bound profile and runtime contract have
    // validated. The plugin create RPC stamps agent_id atomically; an explicit
    // existing unbound conversation is bound under the turn lock below.
    let (conversation_id, auto_created) = match args.conversation.as_ref() {
        Some(id) => (id.clone(), false),
        None => (
            prepare_chat_turn!(store.create_with_ownership_and_agent(
                None,
                args.as_user.clone(),
                visibility_from_arg(args.visibility),
                agent_id.map(ToOwned::to_owned),
            ))
            .id,
            true,
        ),
    };

    if auto_created && !json {
        eprintln!("conversation: {conversation_id}");
    }

    let reasoning_effort = args
        .reasoning_effort
        .map(|level| level.as_str().to_string())
        .or_else(|| agent_profile.and_then(|profile| profile.reasoning_effort.clone()));
    let agent_system_prompt =
        agent_profile.map(|profile| profile.system_prompt.trim()).filter(|prompt| !prompt.is_empty());
    let agent_tool_profile = agent_profile
        .and_then(|profile| profile.tool_profile.as_deref())
        .map(str::trim)
        .filter(|profile| !profile.is_empty());

    // The durable caller hash above protects caller intent. Bind the admitted
    // operation to the fully resolved execution snapshot before reserving the
    // conversation revision or writing a transcript row. A recovered pending
    // operation can only resume when both snapshots are unchanged.
    let execution_hash = if turn_operation.is_some() {
        Some(prepare_chat_turn!(idempotency::effective_request_hash(serde_json::json!({
            "version": 1,
            "conversation_id": conversation_id,
            "agent_id": agent_id,
            "tool": tool,
            "model": model,
            "cwd": cwd,
            "reasoning_effort": reasoning_effort,
            "permission_mode": permission_mode,
            "approvals": approvals,
            "title_update": normalized_title,
            "agent_system_prompt": agent_system_prompt,
            "agent_tool_profile": agent_tool_profile,
            "runtime_contract": mcp_contract,
            "skill_application": skill_application,
        }))))
    } else {
        None
    };

    let ctx = TurnContext {
        conversation_id: &conversation_id,
        agent_id,
        expected_revision: args.expected_revision,
        title_update: args.title.as_deref(),
        tool: &tool,
        model: &model,
        user_message: &args.message,
        cwd,
        project_root: project_root_path.clone(),
        reasoning_effort: reasoning_effort.as_deref(),
        permission_mode: permission_mode.as_deref(),
        approvals,
        agent_system_prompt,
        agent_tool_profile,
        mcp_contract: mcp_contract.as_ref(),
        isolated_mcp_config_path: isolated_mcp_config_path.as_deref(),
        skill: skill_application,
        operation: turn_operation.as_mut(),
        execution_hash: execution_hash.as_deref(),
    };

    let assistant_seq = run_turn(&producer, &store, sink.as_mut(), ctx).await?;

    // Non-streaming, non-json: print the persisted assistant turn so the
    // caller sees the reply.
    if !json && !args.stream {
        let messages = store.load_messages(&conversation_id)?;
        if let Some(message) = messages.iter().find(|m| m.seq == assistant_seq) {
            print_ok(&message.content, json);
        }
    }
    Ok(())
}

fn emit_operation_receipt(
    sink: &mut dyn ChatStreamSink,
    receipt: &orchestrator_core::ChatOperationReceipt,
) -> Result<()> {
    let user_seq =
        receipt.user_seq.ok_or_else(|| anyhow!("canonical chat operation receipt is missing its user sequence"))?;
    sink.emit(&ChatStreamEvent::UserMessageAccepted {
        status: orchestrator_core::ChatOperationStatus::UserAccepted,
        conversation_id: receipt.conversation_id.clone(),
        seq: user_seq,
        message_id: receipt.user_message_id.clone(),
        operation_id: Some(receipt.operation_id.clone()),
    })?;
    match receipt.status {
        orchestrator_core::ChatOperationStatus::Completed => sink.emit(&ChatStreamEvent::TurnCompleted {
            status: receipt.status,
            conversation_id: receipt.conversation_id.clone(),
            seq: receipt
                .assistant_seq
                .ok_or_else(|| anyhow!("completed chat operation receipt is missing its assistant sequence"))?,
            message_id: receipt.assistant_message_id.clone(),
            user_seq,
            user_message_id: receipt.user_message_id.clone(),
            operation_id: Some(receipt.operation_id.clone()),
            session_id: None,
        }),
        orchestrator_core::ChatOperationStatus::AssistantFailed
        | orchestrator_core::ChatOperationStatus::AssistantInterrupted => sink.emit(&ChatStreamEvent::TurnFailed {
            status: receipt.status,
            conversation_id: receipt.conversation_id.clone(),
            user_seq,
            user_message_id: receipt.user_message_id.clone(),
            operation_id: Some(receipt.operation_id.clone()),
            error_code: receipt.error_code.clone().unwrap_or_else(|| "assistant_failed".to_string()),
            error_message: receipt.error_message.clone().unwrap_or_else(|| "assistant failed".to_string()),
        }),
        status => Err(anyhow!("cannot replay non-terminal chat operation status {status:?}")),
    }
}

fn replay_result(receipt: &orchestrator_core::ChatOperationReceipt) -> Result<()> {
    match receipt.status {
        orchestrator_core::ChatOperationStatus::Completed => Ok(()),
        orchestrator_core::ChatOperationStatus::AssistantFailed
        | orchestrator_core::ChatOperationStatus::AssistantInterrupted => Err(anyhow!(
            "{}: user message accepted at seq {}; {}",
            receipt.error_code.as_deref().unwrap_or("assistant_failed"),
            receipt.user_seq.unwrap_or_default(),
            receipt.error_message.as_deref().unwrap_or("assistant failed")
        )),
        status => Err(anyhow!("cannot replay non-terminal chat operation status {status:?}")),
    }
}

fn handle_chat_get(args: ChatGetArgs, project_root: &str, json: bool) -> Result<()> {
    let store = ConversationStoreClient::for_project_as_user(Path::new(project_root), args.as_user.as_deref())?;
    let meta = store.load_meta(&args.id)?.ok_or_else(|| anyhow!("conversation '{}' not found", args.id))?;
    client::ensure_user_may_access(&meta, &args.id, args.as_user.as_deref())?;
    // The current conversation-store protocol returns the full transcript.
    // Bound the caller-visible result here until the protocol grows cursors.
    let messages = page(store.load_messages(&args.id)?, args.offset, args.limit);
    print_value(serde_json::json!({ "meta": meta, "messages": messages }), json)
}

fn handle_chat_list(args: ChatListArgs, project_root: &str, json: bool) -> Result<()> {
    let store = ConversationStoreClient::for_project(Path::new(project_root))?;
    // Ownership filtering must precede pagination or inaccessible rows would
    // consume page slots and leak information through page shape.
    let summaries = page(store.list_for_user(args.as_user.as_deref())?, args.offset, args.limit);
    if !json {
        if summaries.is_empty() {
            println!("No chat conversations yet. Start one with: animus chat new");
            return Ok(());
        }
        let rows: Vec<Vec<String>> = summaries
            .iter()
            .map(|s| {
                vec![
                    s.id.clone(),
                    s.agent_id.clone().unwrap_or_else(|| "--".to_string()),
                    s.title.clone().unwrap_or_else(|| "--".to_string()),
                    s.tool.clone().unwrap_or_else(|| "--".to_string()),
                    s.model.clone().unwrap_or_else(|| "--".to_string()),
                    s.message_count.to_string(),
                    format_age(&s.updated_at),
                ]
            })
            .collect();
        render_table(&["ID", "AGENT", "TITLE", "TOOL", "MODEL", "MSGS", "UPDATED"], &rows);
        return Ok(());
    }
    print_value(summaries, json)
}

fn page<T>(items: Vec<T>, offset: usize, limit: Option<usize>) -> Vec<T> {
    let iter = items.into_iter().skip(offset);
    match limit {
        Some(limit) => iter.take(limit).collect(),
        None => iter.collect(),
    }
}

#[cfg(test)]
mod export_tests {
    use super::*;
    use store::FileConversationStore;

    fn seed_agents(root: &Path, agents_yaml: &str, phase_agent: &str) {
        std::fs::create_dir_all(root.join(".animus")).unwrap();
        std::fs::write(
            root.join(".animus/workflows.yaml"),
            format!(
                "tools_allowlist:\n  - cargo\nagents:\n{agents_yaml}\nphases:\n  work:\n    mode: agent\n    agent_id: {phase_agent}\n"
            ),
        )
        .unwrap();
    }

    fn sample_meta() -> ConversationMeta {
        ConversationMeta {
            id: "conv-x".into(),
            agent_id: Some("researcher".into()),
            revision: 3,
            active_operation_id: None,
            tool: Some("codex".into()),
            model: Some("gpt-5.5".into()),
            session_id: None,
            title: Some("My Chat".into()),
            created_at: "2026-06-09T00:00:00Z".into(),
            updated_at: "2026-06-09T01:00:00Z".into(),
            message_count: 2,
            owner: None,
            visibility: Visibility::Private,
        }
    }

    fn msg(role: ChatRole, content: &str, blocks: Vec<TurnBlock>) -> ChatMessage {
        ChatMessage {
            id: None,
            seq: 0,
            role,
            content: content.into(),
            recorded_at: "2026-06-09T00:30:00Z".into(),
            tool: matches!(role, ChatRole::Assistant).then(|| "codex".to_string()),
            model: matches!(role, ChatRole::Assistant).then(|| "gpt-5.5".to_string()),
            usage: None,
            cost_usd: None,
            blocks,
        }
    }

    #[test]
    fn markdown_renders_title_meta_roles_and_tools() {
        let messages = vec![
            msg(ChatRole::User, "hello", vec![]),
            msg(
                ChatRole::Assistant,
                "hi there",
                vec![TurnBlock::ToolCall { tool_name: Some("Read".into()), arguments: None }],
            ),
        ];
        let md = render_markdown(&sample_meta(), &messages);
        assert!(md.contains("# My Chat"), "{md}");
        assert!(md.contains("tool: codex · model: gpt-5.5 · 2 messages"), "{md}");
        assert!(md.contains("### 🧑 You"), "{md}");
        assert!(md.contains("hello"), "{md}");
        assert!(md.contains("### 🤖 Assistant · codex/gpt-5.5"), "{md}");
        assert!(md.contains("hi there"), "{md}");
        assert!(md.contains("_Tools: Read_"), "{md}");
    }

    #[test]
    fn markdown_falls_back_to_id_when_untitled() {
        let mut meta = sample_meta();
        meta.title = None;
        let md = render_markdown(&meta, &[]);
        assert!(md.contains("# conv-x"), "{md}");
    }

    #[test]
    fn markdown_includes_usage_and_cost_footer() {
        let mut with_usage = msg(ChatRole::Assistant, "done", vec![]);
        with_usage.usage = Some(protocol::TokenUsage {
            input: 1200,
            output: 340,
            reasoning: None,
            cache_read: None,
            cache_write: None,
        });
        with_usage.cost_usd = Some(0.0123);
        let md = render_markdown(&sample_meta(), &[with_usage]);
        assert!(md.contains("_1200 in · 340 out tokens · $0.0123_"), "{md}");

        // No usage/cost → no footer line at all.
        let plain = msg(ChatRole::Assistant, "done", vec![]);
        let md2 = render_markdown(&sample_meta(), &[plain]);
        assert!(!md2.contains("tokens"), "{md2}");
        assert!(!md2.contains('$'), "{md2}");
    }

    #[test]
    fn apply_title_sets_clears_and_noops() {
        let tmp = tempfile::tempdir().unwrap();
        let store = FileConversationStore::with_root_for_test(tmp.path().join("chat"));
        store.create(Some("conv-t".into())).unwrap();

        // None → leaves the title untouched.
        apply_conversation_title(&store, "conv-t", None).unwrap();
        assert!(store.load_meta("conv-t").unwrap().unwrap().title.is_none());

        // Some(trimmed) → set.
        apply_conversation_title(&store, "conv-t", Some("  Named  ")).unwrap();
        let named = store.load_meta("conv-t").unwrap().unwrap();
        assert_eq!(named.title.as_deref(), Some("Named"));
        assert_eq!(named.revision, 1);

        // Some(blank) → clear.
        apply_conversation_title(&store, "conv-t", Some("   ")).unwrap();
        let cleared = store.load_meta("conv-t").unwrap().unwrap();
        assert!(cleared.title.is_none());
        assert_eq!(cleared.revision, 2);

        // Missing conversation → no error.
        apply_conversation_title(&store, "missing", Some("x")).unwrap();
    }

    #[test]
    fn canonical_agent_resolution_reuses_binding_and_rejects_conflicts() {
        let tmp = tempfile::tempdir().unwrap();
        seed_agents(
            tmp.path(),
            "  researcher:\n    system_prompt: Research exactly.\n    tool: codex\n    model: gpt-5.6\n  writer:\n    system_prompt: Write exactly.",
            "researcher",
        );
        let _seam =
            orchestrator_config::workflow_config::config_source_client::install_yaml_config_source_base(tmp.path());

        let selected = resolve_chat_agent(tmp.path(), None, None, Some("ReSeArChEr"))
            .unwrap()
            .expect("requested profile resolves");
        assert_eq!(selected.0, "researcher", "caller casing must canonicalize to the configured key");
        assert_eq!(selected.1.tool.as_deref(), Some("codex"));
        assert_eq!(selected.1.model.as_deref(), Some("gpt-5.6"));

        let continued = resolve_chat_agent(tmp.path(), None, Some("researcher"), None)
            .unwrap()
            .expect("persisted profile resolves without --agent");
        assert_eq!(continued.0, "researcher");
        assert_eq!(continued.1.system_prompt, "Research exactly.");

        let error = resolve_chat_agent(tmp.path(), None, Some("researcher"), Some("writer")).unwrap_err();
        assert!(error.to_string().contains("chat_precondition_failed:binding_conflict:"), "unexpected error: {error}");
        assert!(resolve_chat_agent(tmp.path(), None, None, None).unwrap().is_none(), "legacy chat stays unbound");
    }

    #[test]
    fn renamed_deleted_hidden_and_cross_scope_profiles_fail_closed() {
        let visible = tempfile::tempdir().unwrap();
        seed_agents(visible.path(), "  researcher:\n    system_prompt: Research exactly.", "researcher");
        let _visible_seam =
            orchestrator_config::workflow_config::config_source_client::install_yaml_config_source_base(visible.path());
        assert!(resolve_chat_agent(visible.path(), None, Some("researcher"), None).unwrap().is_some());

        // A different repository/tenant config contains no such canonical id:
        // a persisted binding cannot drift across scope or follow a renamed /
        // deleted profile to a default.
        let other_scope = tempfile::tempdir().unwrap();
        seed_agents(other_scope.path(), "  writer:\n    system_prompt: Write exactly.", "writer");
        let _other_seam = orchestrator_config::workflow_config::config_source_client::install_yaml_config_source_base(
            other_scope.path(),
        );
        let actor = animus_actor::Actor {
            user_id: "other-user".to_string(),
            claims: Vec::new(),
            tenant_id: Some("other-tenant".to_string()),
        };
        let error = resolve_chat_agent(other_scope.path(), Some(&actor), Some("researcher"), None).unwrap_err();
        assert!(error.to_string().contains("not visible in this project and actor scope"), "unexpected error: {error}");
    }

    #[test]
    fn snippet_around_matches_and_ellipsizes() {
        assert!(snippet_around("hello AUTH world", "auth", true).unwrap().to_lowercase().contains("auth"));
        assert_eq!(snippet_around("hello world", "xyz", true), None);
        assert_eq!(snippet_around("hello", "", true), None);
        // case-sensitive miss
        assert_eq!(snippet_around("AUTH", "auth", false), None);
        // ellipsis on both ends when the match is in the middle of a long body
        let long = format!("{} needle {}", "a".repeat(100), "b".repeat(100));
        let snip = snippet_around(&long, "needle", true).unwrap();
        assert!(snip.starts_with('…') && snip.ends_with('…'), "{snip}");
        assert!(snip.contains("needle"));
    }

    #[test]
    fn snippet_around_survives_case_folding_length_changes() {
        // 'İ' lowercases to two chars ("i\u{307}"), so the char index
        // computed against the lowercased haystack can exceed the
        // original content's char count.
        let content = format!("{}NEEDLE", "İ".repeat(40));
        let snip = snippet_around(&content, "needle", true).unwrap();
        assert!(snip.contains("NEEDLE"), "{snip}");

        let trailing = format!("straße {} NEEDLE", "İ".repeat(40));
        let snip = snippet_around(&trailing, "needle", true).unwrap();
        assert!(snip.contains("NEEDLE"), "{snip}");

        let padded = format!("{}NEEDLE{}", "İ".repeat(100), "x".repeat(40));
        let snip = snippet_around(&padded, "needle", true).unwrap();
        assert!(snip.contains("NEEDLE"), "{snip}");
    }

    #[test]
    fn search_conversations_finds_limits_and_respects_case() {
        let tmp = tempfile::tempdir().unwrap();
        let store = ConversationStoreClient::with_root_for_test(tmp.path().join("chat"));
        store.create(Some("conv-s".into())).unwrap();
        store.append_message("conv-s", &msg(ChatRole::User, "Please fix the AUTH bug", vec![])).unwrap();
        store.append_message("conv-s", &msg(ChatRole::Assistant, "done, no issues", vec![])).unwrap();

        // case-insensitive hit on the user message
        let m = search_conversations(&store, "auth", true, 20, None).unwrap();
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].conversation_id, "conv-s");
        assert_eq!(m[0].role, "user");
        assert!(m[0].snippet.to_lowercase().contains("auth"), "{}", m[0].snippet);

        // case-sensitive "auth" does not match "AUTH"
        assert!(search_conversations(&store, "auth", false, 20, None).unwrap().is_empty());

        // empty query → no matches
        assert!(search_conversations(&store, "", true, 20, None).unwrap().is_empty());

        // limit is respected
        store.append_message("conv-s", &msg(ChatRole::User, "auth again", vec![])).unwrap();
        assert_eq!(search_conversations(&store, "auth", true, 1, None).unwrap().len(), 1);
    }

    #[test]
    fn page_applies_offset_then_limit_without_reordering() {
        assert_eq!(page(vec![1, 2, 3, 4], 1, Some(2)), vec![2, 3]);
        assert_eq!(page(vec![1, 2], 5, Some(2)), Vec::<i32>::new());
        assert_eq!(page(vec![1, 2], 0, None), vec![1, 2]);
        assert_eq!(page(vec![1, 2], 0, Some(0)), Vec::<i32>::new());
    }
}
