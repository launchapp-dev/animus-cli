use cli_wrapper::{extract_text_from_line, NormalizedTextEvent};

fn extract_all(raw: &str, tool: &str) -> String {
    let mut out = String::new();
    let mut json_accum = String::new();
    let mut depth: i32 = 0;
    let mut accumulating = false;

    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let is_json = serde_json::from_str::<serde_json::Value>(trimmed).is_ok();

        if !is_json && (trimmed == "{" || accumulating) {
            if trimmed == "{" && !accumulating {
                accumulating = true;
                json_accum.clear();
                depth = 0;
            }
            json_accum.push_str(trimmed);
            json_accum.push('\n');
            for ch in trimmed.chars() {
                match ch {
                    '{' => depth += 1,
                    '}' => depth -= 1,
                    _ => {}
                }
            }
            if depth <= 0 {
                accumulating = false;
                extract_into(&json_accum, tool, &mut out);
                json_accum.clear();
                depth = 0;
            }
            continue;
        }

        extract_into(trimmed, tool, &mut out);
    }
    out
}

fn extract_into(text: &str, tool: &str, out: &mut String) {
    match extract_text_from_line(text, tool) {
        NormalizedTextEvent::TextChunk { text } | NormalizedTextEvent::FinalResult { text } => {
            out.push_str(&text);
        }
        NormalizedTextEvent::Ignored => {}
    }
}

#[test]
fn claude_real_agent_output() {
    let raw = include_str!("fixtures/claude_real.jsonl");
    let text = extract_all(raw, "claude");
    assert!(text.contains("PINEAPPLE_42"), "claude: expected PINEAPPLE_42, got: {:?}", text);
}

#[test]
fn codex_real_agent_output() {
    let raw = include_str!("fixtures/codex_real.jsonl");
    let text = extract_all(raw, "codex");
    assert!(text.contains("PINEAPPLE_42"), "codex: expected PINEAPPLE_42, got: {:?}", text);
}

#[test]
fn gemini_real_agent_output() {
    let raw = include_str!("fixtures/gemini_real.jsonl");
    let text = extract_all(raw, "gemini");
    assert!(text.contains("PINEAPPLE_42"), "gemini: expected PINEAPPLE_42, got: {:?}", text);
}

#[test]
fn oai_runner_real_agent_output() {
    let raw = include_str!("fixtures/oai_runner_real.jsonl");
    let text = extract_all(raw, "oai-runner");
    assert!(text.contains("PINEAPPLE_42"), "oai-runner: expected PINEAPPLE_42, got: {:?}", text);
}
