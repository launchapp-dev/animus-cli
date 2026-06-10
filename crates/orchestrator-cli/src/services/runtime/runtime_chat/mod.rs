//! `animus chat` — multi-turn conversations with a provider tool.
//!
//! See [`turn`] for the continuity model (providers own continuity; Animus
//! owns a thin portable/fallback layer) and `docs/reference/chat.md`.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};

use anyhow::Context;

use crate::shared::{canonicalize_cwd_in_project, print_ok, print_value};
use crate::{
    ChatCommand, ChatDeleteArgs, ChatExportArgs, ChatExportFormat, ChatGetArgs, ChatNewArgs, ChatRenameArgs,
    ChatSearchArgs, ChatSendArgs,
};
use serde::Serialize;

pub(crate) mod sink;
pub(crate) mod store;
pub(crate) mod turn;

use sink::{ChatStreamSink, JsonlStdoutSink, NullSink, TextStdoutSink};
use store::{ChatMessage, ChatRole, ConversationMeta, ConversationStore, FileConversationStore, TurnBlock};
use turn::{run_turn, ResolverTurnProducer, TurnContext};

pub(crate) async fn handle_chat(command: ChatCommand, project_root: &str, json: bool) -> Result<()> {
    match command {
        ChatCommand::New(args) => handle_chat_new(args, project_root, json),
        ChatCommand::Send(args) => handle_chat_send(args, project_root, json).await,
        ChatCommand::Get(args) => handle_chat_get(args, project_root, json),
        ChatCommand::List => handle_chat_list(project_root, json),
        ChatCommand::Rename(args) => handle_chat_rename(args, project_root, json),
        ChatCommand::Delete(args) => handle_chat_delete(args, project_root, json),
        ChatCommand::Export(args) => handle_chat_export(args, project_root, json),
        ChatCommand::Search(args) => handle_chat_search(args, project_root, json),
    }
}

#[derive(Debug, Serialize, PartialEq)]
struct SearchMatch {
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

/// Scan every conversation (newest-first) for `query`, collecting up to `limit`
/// matches with a preview snippet.
fn search_conversations(
    store: &impl ConversationStore,
    query: &str,
    case_insensitive: bool,
    limit: usize,
) -> Result<Vec<SearchMatch>> {
    let mut out = Vec::new();
    if query.is_empty() {
        return Ok(out);
    }
    for summary in store.list()? {
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
    let store = FileConversationStore::for_project(Path::new(project_root))?;
    let matches = search_conversations(&store, &args.query, !args.case_sensitive, args.limit)?;
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
    let store = FileConversationStore::for_project(Path::new(project_root))?;
    let meta = store.load_meta(&args.id)?.ok_or_else(|| anyhow!("conversation '{}' not found", args.id))?;
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
fn apply_conversation_title(store: &impl ConversationStore, id: &str, title: Option<&str>) -> Result<()> {
    let Some(title) = title else { return Ok(()) };
    let Some(mut meta) = store.load_meta(id)? else { return Ok(()) };
    let trimmed = title.trim();
    meta.title = if trimmed.is_empty() { None } else { Some(trimmed.to_string()) };
    store.save_meta(&meta)
}

fn handle_chat_rename(args: ChatRenameArgs, project_root: &str, json: bool) -> Result<()> {
    let store = FileConversationStore::for_project(Path::new(project_root))?;
    let mut meta = store.load_meta(&args.id)?.ok_or_else(|| anyhow!("conversation '{}' not found", args.id))?;
    let trimmed = args.title.trim();
    meta.title = if trimmed.is_empty() { None } else { Some(trimmed.to_string()) };
    store.save_meta(&meta)?;
    print_value(serde_json::json!({ "conversation_id": meta.id, "title": meta.title }), json)
}

fn handle_chat_delete(args: ChatDeleteArgs, project_root: &str, json: bool) -> Result<()> {
    let store = FileConversationStore::for_project(Path::new(project_root))?;
    let existed = store.load_meta(&args.id)?.is_some();
    store.delete(&args.id)?;
    print_value(serde_json::json!({ "conversation_id": args.id, "deleted": existed }), json)
}

fn handle_chat_new(args: ChatNewArgs, project_root: &str, json: bool) -> Result<()> {
    let store = FileConversationStore::for_project(Path::new(project_root))?;
    let mut meta = store.create(args.id)?;
    if args.title.is_some() {
        meta.title = args.title;
        store.save_meta(&meta)?;
    }
    print_value(serde_json::json!({ "conversation_id": meta.id, "title": meta.title }), json)
}

async fn handle_chat_send(args: ChatSendArgs, project_root: &str, json: bool) -> Result<()> {
    let project_root_path = PathBuf::from(project_root);
    let store = FileConversationStore::for_project(&project_root_path)?;

    // Resolve (or create) the target conversation.
    let (conversation_id, auto_created) = match args.conversation {
        Some(id) => {
            if store.load_meta(&id)?.is_none() {
                return Err(anyhow!("conversation '{id}' not found; create it with `animus chat new`"));
            }
            (id, false)
        }
        None => (store.create(None)?.id, true),
    };

    // Apply an optional title — names a freshly-created conversation or renames
    // the target one. Done before the turn so a crash mid-stream still leaves
    // the conversation named.
    apply_conversation_title(&store, &conversation_id, args.title.as_deref())?;

    // Surface an auto-created conversation id up front so the caller can pass
    // `--conversation <id>` on the next turn (codex round-4 P2). In `--json`
    // mode the id also rides on the `turn_completed` frame; we print it to
    // stderr here in the text modes so it never mixes into the assistant
    // content on stdout.
    if auto_created && !json {
        eprintln!("conversation: {conversation_id}");
    }

    let model = args
        .model
        .clone()
        .unwrap_or_else(|| protocol::default_model_for_tool(&args.tool).unwrap_or("claude-sonnet-4-6").to_string());

    let raw_cwd = args.cwd.clone().unwrap_or_else(|| project_root.to_string());
    let cwd = PathBuf::from(canonicalize_cwd_in_project(&raw_cwd, project_root)?);

    let producer = ResolverTurnProducer::for_project(&project_root_path);

    // Resolve the per-agent MCP server set (profile ∪ skill ∪ --mcp-server
    // additions − the built-in animus when --no-animus-mcp), then assemble
    // the runtime contract the provider receives so the chat agent sees the
    // MCP servers its profile/skill declares. Plain chat (no --agent/--skill)
    // defaults to the built-in `animus` server only.
    let scope = crate::services::runtime::agent_mcp::resolve_agent_scope(
        &project_root_path,
        &args.tool,
        args.agent.as_deref(),
        args.skill.as_deref(),
    )?;
    let scope_selected = args.agent.is_some() || args.skill.is_some();
    let mcp_contract = crate::services::runtime::agent_mcp::assemble_agent_mcp_contract(
        &project_root_path,
        &args.tool,
        &model,
        &scope.profile_servers,
        &scope.skill_servers,
        &args.mcp_server,
        &scope.tool_policy,
        scope_selected,
        args.no_animus_mcp,
    )?;

    // Provider CLIs that auto-discover a cwd-local `.mcp.json` (claude-code)
    // register MCP servers from that file, not the runtime contract, so the
    // per-agent set is also materialized there. The merge is additive — it
    // upserts only the resolved Animus-scoped names and preserves any
    // user-authored entries.
    if let Some(contract) = mcp_contract.as_ref() {
        crate::services::runtime::agent_mcp::materialize_mcp_json(&cwd, contract)?;
    }

    // Sink selection: --json => JSONL stdout; --stream (no json) => text;
    // neither => discard streaming and print the final transcript turn.
    let mut sink: Box<dyn ChatStreamSink> = if json {
        Box::new(JsonlStdoutSink)
    } else if args.stream {
        Box::new(TextStdoutSink)
    } else {
        Box::new(NullSink)
    };

    let ctx = TurnContext {
        conversation_id: &conversation_id,
        tool: &args.tool,
        model: &model,
        user_message: &args.message,
        cwd,
        project_root: project_root_path.clone(),
        reasoning_effort: args.reasoning_effort.map(|level| level.as_str()),
        mcp_contract: mcp_contract.as_ref(),
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

fn handle_chat_get(args: ChatGetArgs, project_root: &str, json: bool) -> Result<()> {
    let store = FileConversationStore::for_project(Path::new(project_root))?;
    let meta = store.load_meta(&args.id)?.ok_or_else(|| anyhow!("conversation '{}' not found", args.id))?;
    let messages = store.load_messages(&args.id)?;
    print_value(serde_json::json!({ "meta": meta, "messages": messages }), json)
}

fn handle_chat_list(project_root: &str, json: bool) -> Result<()> {
    let store = FileConversationStore::for_project(Path::new(project_root))?;
    let summaries = store.list()?;
    print_value(summaries, json)
}

#[cfg(test)]
mod export_tests {
    use super::*;

    fn sample_meta() -> ConversationMeta {
        ConversationMeta {
            id: "conv-x".into(),
            tool: Some("codex".into()),
            model: Some("gpt-5.5".into()),
            session_id: None,
            title: Some("My Chat".into()),
            created_at: "2026-06-09T00:00:00Z".into(),
            updated_at: "2026-06-09T01:00:00Z".into(),
            message_count: 2,
        }
    }

    fn msg(role: ChatRole, content: &str, blocks: Vec<TurnBlock>) -> ChatMessage {
        ChatMessage {
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
        assert_eq!(store.load_meta("conv-t").unwrap().unwrap().title.as_deref(), Some("Named"),);

        // Some(blank) → clear.
        apply_conversation_title(&store, "conv-t", Some("   ")).unwrap();
        assert!(store.load_meta("conv-t").unwrap().unwrap().title.is_none());

        // Missing conversation → no error.
        apply_conversation_title(&store, "missing", Some("x")).unwrap();
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
        let store = FileConversationStore::with_root_for_test(tmp.path().join("chat"));
        store.create(Some("conv-s".into())).unwrap();
        store.append_message("conv-s", &msg(ChatRole::User, "Please fix the AUTH bug", vec![])).unwrap();
        store.append_message("conv-s", &msg(ChatRole::Assistant, "done, no issues", vec![])).unwrap();

        // case-insensitive hit on the user message
        let m = search_conversations(&store, "auth", true, 20).unwrap();
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].conversation_id, "conv-s");
        assert_eq!(m[0].role, "user");
        assert!(m[0].snippet.to_lowercase().contains("auth"), "{}", m[0].snippet);

        // case-sensitive "auth" does not match "AUTH"
        assert!(search_conversations(&store, "auth", false, 20).unwrap().is_empty());

        // empty query → no matches
        assert!(search_conversations(&store, "", true, 20).unwrap().is_empty());

        // limit is respected
        store.append_message("conv-s", &msg(ChatRole::User, "auth again", vec![])).unwrap();
        assert_eq!(search_conversations(&store, "auth", true, 1).unwrap().len(), 1);
    }
}
