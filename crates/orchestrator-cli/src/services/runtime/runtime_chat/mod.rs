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
    ChatSendArgs,
};

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
    }
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
}
