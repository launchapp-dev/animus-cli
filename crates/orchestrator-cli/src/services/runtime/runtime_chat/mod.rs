//! `animus chat` — multi-turn conversations with a provider tool.
//!
//! See [`turn`] for the continuity model (providers own continuity; Animus
//! owns a thin portable/fallback layer) and `docs/reference/chat.md`.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};

use crate::shared::{canonicalize_cwd_in_project, print_ok, print_value};
use crate::{ChatCommand, ChatGetArgs, ChatNewArgs, ChatSendArgs};

pub(crate) mod sink;
pub(crate) mod store;
pub(crate) mod turn;

use sink::{ChatStreamSink, JsonlStdoutSink, NullSink, TextStdoutSink};
use store::{ConversationStore, FileConversationStore};
use turn::{run_turn, ResolverTurnProducer, TurnContext};

pub(crate) async fn handle_chat(command: ChatCommand, project_root: &str, json: bool) -> Result<()> {
    match command {
        ChatCommand::New(args) => handle_chat_new(args, project_root, json),
        ChatCommand::Send(args) => handle_chat_send(args, project_root, json).await,
        ChatCommand::Get(args) => handle_chat_get(args, project_root, json),
        ChatCommand::List => handle_chat_list(project_root, json),
    }
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
