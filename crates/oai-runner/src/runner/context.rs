use crate::api::types::ChatMessage;

static TOKENIZER: std::sync::OnceLock<tiktoken_rs::CoreBPE> = std::sync::OnceLock::new();

fn get_tokenizer() -> &'static tiktoken_rs::CoreBPE {
    TOKENIZER.get_or_init(|| tiktoken_rs::cl100k_base().expect("failed to load cl100k_base tokenizer"))
}

pub fn estimate_message_tokens(msg: &ChatMessage) -> usize {
    let bpe = get_tokenizer();
    let mut tokens = 4; // every message has <im_start>, role, \n, <im_end>
    tokens += bpe.encode_ordinary(&msg.role).len();
    if let Some(content) = &msg.content {
        tokens += bpe.encode_ordinary(content).len();
    }
    if let Some(tool_calls) = &msg.tool_calls {
        for tc in tool_calls {
            tokens += bpe.encode_ordinary(&tc.function.name).len();
            tokens += bpe.encode_ordinary(&tc.function.arguments).len();
            tokens += 3; // function call overhead
        }
    }
    tokens
}

pub fn estimate_total_tokens(messages: &[ChatMessage]) -> usize {
    let mut total = 3; // <im_start>assistant prefix
    for msg in messages {
        total += estimate_message_tokens(msg);
    }
    total
}

pub fn truncate_to_fit(messages: &mut Vec<ChatMessage>, context_limit: usize, reserve_for_output: usize) {
    let target = context_limit.saturating_sub(reserve_for_output);
    let total = estimate_total_tokens(messages);

    if total <= target {
        return;
    }

    let system_idx = messages.iter().position(|m| m.role == "system");

    let mut removed = 0;
    let mut i = system_idx.map_or(0, |s| s + 1);

    while i < messages.len() && estimate_total_tokens(messages) > target {
        let is_recent_user = messages[i].role == "user" && {
            let remaining_users = messages[i..].iter().filter(|m| m.role == "user").count();
            remaining_users <= 2
        };

        if is_recent_user || messages[i].role == "system" {
            i += 1;
            continue;
        }

        if messages[i].role == "tool" {
            if let Some(content) = &messages[i].content {
                if content.len() > 500 {
                    messages[i].content = Some(format!(
                        "{}...\n[truncated from {} chars to save context]",
                        &content.chars().take(200).collect::<String>(),
                        content.len()
                    ));
                    removed += 1;
                    i += 1;
                    continue;
                }
            }
        }

        if messages[i].role == "assistant" && messages[i].tool_calls.is_none() {
            if let Some(content) = &messages[i].content {
                if content.len() > 500 {
                    messages[i].content =
                        Some(format!("{}...\n[truncated]", &content.chars().take(200).collect::<String>()));
                    removed += 1;
                    i += 1;
                    continue;
                }
            }
        }

        i += 1;
    }

    if removed > 0 {
        let new_total = estimate_total_tokens(messages);
        eprintln!(
            "[oai-runner] Context management: truncated {} messages ({} -> {} estimated tokens, limit {})",
            removed, total, new_total, target
        );
    }

    if estimate_total_tokens(messages) > target {
        let excess_tokens = estimate_total_tokens(messages) - target;
        let keep_system = system_idx.is_some();
        let start = if keep_system { 1 } else { 0 };

        let mut to_remove = Vec::new();
        let mut freed = 0;
        for idx in start..messages.len() {
            if messages[idx].role == "user" {
                let remaining_users = messages[idx..].iter().filter(|m| m.role == "user").count();
                if remaining_users <= 1 {
                    continue;
                }
            }
            freed += estimate_message_tokens(&messages[idx]);
            to_remove.push(idx);
            if freed >= excess_tokens {
                break;
            }
        }

        if !to_remove.is_empty() {
            let count = to_remove.len();
            for idx in to_remove.into_iter().rev() {
                messages.remove(idx);
            }
            let new_total = estimate_total_tokens(messages);
            eprintln!(
                "[oai-runner] Context management: dropped {} old messages ({} estimated tokens, limit {})",
                count, new_total, target
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(role: &str, content: &str) -> ChatMessage {
        ChatMessage { reasoning_content: None, role: role.to_string(), content: Some(content.to_string()), tool_calls: None, tool_call_id: None }
    }

    #[test]
    fn estimate_tokens_basic() {
        let tokens = estimate_message_tokens(&msg("user", "Hello world"));
        assert!(tokens > 2 && tokens < 20);
    }

    #[test]
    fn estimate_total_includes_overhead() {
        let messages = vec![msg("system", "You are helpful"), msg("user", "Hi")];
        let total = estimate_total_tokens(&messages);
        assert!(total > 5);
    }

    #[test]
    fn truncate_preserves_system_and_recent() {
        let mut messages = vec![
            msg("system", "You are helpful"),
            msg("user", "First question"),
            msg("tool", &"x".repeat(10000)),
            msg("assistant", "first answer"),
            msg("user", "Second question"),
        ];
        truncate_to_fit(&mut messages, 100, 50);
        assert_eq!(messages[0].role, "system");
        assert!(messages.iter().any(|m| m.content.as_deref() == Some("Second question")));
    }

    #[test]
    fn no_truncation_when_under_limit() {
        let mut messages = vec![msg("user", "Hi"), msg("assistant", "Hello")];
        let original_len = messages.len();
        truncate_to_fit(&mut messages, 100000, 16384);
        assert_eq!(messages.len(), original_len);
    }
}
