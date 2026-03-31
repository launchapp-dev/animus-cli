use std::path::PathBuf;
use std::time::Duration;

use cli_wrapper::{SessionBackend, SessionEvent, SessionRequest, SubprocessSessionBackend};
use serde_json::json;

fn structured_fixture() -> String {
    [
        r#"{"type":"tool_call","tool_name":"read_file","arguments":{"path":"alpha.txt"}}"#,
        r#"{"type":"metadata","tokens":{"input":1,"output":2}}"#,
        r#"{"type":"tool_result","tool_name":"read_file","output":{"content":"alpha"}}"#,
        r#"{"type":"result","text":"PINEAPPLE_42"}"#,
    ]
    .join("\n")
}

#[tokio::test]
#[cfg(unix)]
async fn subprocess_backend_preserves_oai_runner_structured_events() {
    let backend = SubprocessSessionBackend::new();
    let tempdir = tempfile::tempdir().expect("tempdir should exist");
    let fixture_path = tempdir.path().join("oai_runner_structured.jsonl");
    std::fs::write(&fixture_path, structured_fixture()).expect("fixture should write");

    let request = SessionRequest {
        tool: "oai-runner".to_string(),
        model: String::new(),
        prompt: String::new(),
        cwd: PathBuf::from("."),
        project_root: None,
        mcp_endpoint: None,
        permission_mode: None,
        timeout_secs: Some(5),
        env_vars: Vec::new(),
        extras: json!({
            "runtime_contract": {
                "cli": {
                    "launch": {
                        "command": "sh",
                        "args": ["-c", format!("cat {}", fixture_path.display())],
                        "prompt_via_stdin": false
                    }
                }
            }
        }),
    };

    let mut run = backend.start_session(request).await.expect("session should start");
    let mut saw_tool_call = false;
    let mut saw_metadata = false;
    let mut saw_tool_result = false;
    let mut saw_final_text = false;

    let deadline = tokio::time::sleep(Duration::from_secs(5));
    tokio::pin!(deadline);

    loop {
        tokio::select! {
            _ = &mut deadline => panic!("timed out waiting for structured oai-runner events"),
            maybe_event = run.events.recv() => {
                let Some(event) = maybe_event else {
                    break;
                };
                match event {
                    SessionEvent::ToolCall { tool_name, arguments, server } => {
                        saw_tool_call = true;
                        assert_eq!(tool_name, "read_file");
                        assert_eq!(arguments, json!({"path":"alpha.txt"}));
                        assert!(server.is_none());
                    }
                    SessionEvent::Metadata { metadata } => {
                        saw_metadata = true;
                        assert_eq!(metadata["type"], "metadata");
                    }
                    SessionEvent::ToolResult { tool_name, output, success } => {
                        saw_tool_result = true;
                        assert_eq!(tool_name, "read_file");
                        assert!(success);
                        assert_eq!(output, json!({"content":"alpha"}));
                    }
                    SessionEvent::FinalText { text } => {
                        saw_final_text = true;
                        assert_eq!(text, "PINEAPPLE_42");
                    }
                    SessionEvent::Finished { .. } => break,
                    _ => {}
                }
            }
        }
    }

    assert!(saw_tool_call, "expected oai-runner tool_call event");
    assert!(saw_metadata, "expected oai-runner metadata event");
    assert!(saw_tool_result, "expected oai-runner tool_result event");
    assert!(saw_final_text, "expected oai-runner final text event");
}
