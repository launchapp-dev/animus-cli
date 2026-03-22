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

fn build_message_groups(messages: &[ChatMessage], start: usize) -> Vec<Vec<usize>> {
    let mut groups: Vec<Vec<usize>> = Vec::new();
    let mut i = start;
    while i < messages.len() {
        if messages[i].role == "assistant" && messages[i].tool_calls.is_some() {
            let mut group = vec![i];
            let mut j = i + 1;
            while j < messages.len() && messages[j].role == "tool" {
                group.push(j);
                j += 1;
            }
            groups.push(group);
            i = j;
        } else {
            groups.push(vec![i]);
            i += 1;
        }
    }
    groups
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

        let groups = build_message_groups(messages, start);

        let mut to_remove = Vec::new();
        let mut freed = 0;
        for group in &groups {
            if freed >= excess_tokens {
                break;
            }
            let dominated_by_protected = group.iter().any(|&idx| {
                messages[idx].role == "system"
                    || (messages[idx].role == "user" && {
                        let remaining_users = messages[idx..].iter().filter(|m| m.role == "user").count();
                        remaining_users <= 1
                    })
            });
            if dominated_by_protected {
                continue;
            }
            for &idx in group {
                freed += estimate_message_tokens(&messages[idx]);
                to_remove.push(idx);
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
        ChatMessage {
            reasoning_content: None,
            role: role.to_string(),
            content: Some(content.to_string()),
            tool_calls: None,
            tool_call_id: None,
        }
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

    use crate::api::types::{FunctionCall, ToolCall};

    fn assistant_with_tool_calls(tool_call_ids: &[&str]) -> ChatMessage {
        ChatMessage {
            reasoning_content: None,
            role: "assistant".to_string(),
            content: None,
            tool_calls: Some(
                tool_call_ids
                    .iter()
                    .map(|id| ToolCall {
                        id: id.to_string(),
                        type_: "function".to_string(),
                        function: FunctionCall { name: "test_tool".to_string(), arguments: "{}".to_string() },
                    })
                    .collect(),
            ),
            tool_call_id: None,
        }
    }

    fn tool_response(tool_call_id: &str, content: &str) -> ChatMessage {
        ChatMessage {
            reasoning_content: None,
            role: "tool".to_string(),
            content: Some(content.to_string()),
            tool_calls: None,
            tool_call_id: Some(tool_call_id.to_string()),
        }
    }

    #[test]
    fn truncate_drops_tool_call_group_atomically() {
        let mut messages = vec![
            msg("system", "You are helpful"),
            msg("user", "First question"),
            assistant_with_tool_calls(&["call_1", "call_2"]),
            tool_response("call_1", &"x".repeat(5000)),
            tool_response("call_2", &"y".repeat(5000)),
            msg("assistant", "Here is the result"),
            msg("user", "Second question"),
        ];
        truncate_to_fit(&mut messages, 100, 50);
        for (i, m) in messages.iter().enumerate() {
            if m.role == "assistant" && m.tool_calls.is_some() {
                let tc_ids: Vec<&str> = m.tool_calls.as_ref().unwrap().iter().map(|tc| tc.id.as_str()).collect();
                for expected_id in &tc_ids {
                    assert!(
                        messages[i + 1..]
                            .iter()
                            .any(|m2| { m2.role == "tool" && m2.tool_call_id.as_deref() == Some(expected_id) }),
                        "assistant with tool_calls has orphaned tool call id '{}'",
                        expected_id
                    );
                }
            }
            if m.role == "tool" {
                if let Some(tc_id) = &m.tool_call_id {
                    assert!(
                        messages[..i].iter().any(|m2| {
                            m2.role == "assistant"
                                && m2.tool_calls.as_ref().is_some_and(|tcs| tcs.iter().any(|tc| tc.id == *tc_id))
                        }),
                        "tool response '{}' has no parent assistant message",
                        tc_id
                    );
                }
            }
        }
    }

    #[test]
    fn truncate_never_orphans_tool_messages() {
        let mut messages = vec![
            msg("system", "sys"),
            msg("user", "q1"),
            assistant_with_tool_calls(&["c1"]),
            tool_response("c1", &"a".repeat(3000)),
            msg("user", "q2"),
            assistant_with_tool_calls(&["c2"]),
            tool_response("c2", &"b".repeat(3000)),
            msg("user", "q3"),
        ];
        truncate_to_fit(&mut messages, 80, 30);
        let tool_msgs: Vec<_> = messages.iter().filter(|m| m.role == "tool").collect();
        for tm in &tool_msgs {
            let tc_id = tm.tool_call_id.as_deref().unwrap();
            assert!(
                messages.iter().any(|m| {
                    m.role == "assistant"
                        && m.tool_calls.as_ref().is_some_and(|tcs| tcs.iter().any(|tc| tc.id == tc_id))
                }),
                "orphaned tool message with id '{}'",
                tc_id
            );
        }
        let assistant_tc_msgs: Vec<_> =
            messages.iter().enumerate().filter(|(_, m)| m.role == "assistant" && m.tool_calls.is_some()).collect();
        for (i, atc) in &assistant_tc_msgs {
            for tc in atc.tool_calls.as_ref().unwrap() {
                assert!(
                    messages[i + 1..].iter().any(|m| { m.role == "tool" && m.tool_call_id.as_deref() == Some(&tc.id) }),
                    "assistant tool_call '{}' missing tool response",
                    tc.id
                );
            }
        }
    }

    #[test]
    fn build_message_groups_creates_atomic_tool_groups() {
        let messages = vec![
            msg("system", "sys"),
            msg("user", "q1"),
            assistant_with_tool_calls(&["c1", "c2"]),
            tool_response("c1", "r1"),
            tool_response("c2", "r2"),
            msg("user", "q2"),
        ];
        let groups = build_message_groups(&messages, 1);
        assert_eq!(groups.len(), 3);
        assert_eq!(groups[0], vec![1]);
        assert_eq!(groups[1], vec![2, 3, 4]);
        assert_eq!(groups[2], vec![5]);
    }
}
