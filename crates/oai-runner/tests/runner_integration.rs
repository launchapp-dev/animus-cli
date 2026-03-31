use std::collections::VecDeque;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use assert_cmd::prelude::*;
use serde_json::{json, Value};

#[derive(Clone)]
enum ResponseSpec {
    Sse(Vec<Value>),
    Error { status: u16, body: String },
}

struct TestServer {
    base_url: String,
    recorded_requests: Arc<Mutex<Vec<Value>>>,
    handle: Option<thread::JoinHandle<()>>,
}

impl TestServer {
    fn spawn(responses: Vec<ResponseSpec>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("test server should bind");
        let addr = listener.local_addr().expect("listener should have a local address");
        let recorded_requests = Arc::new(Mutex::new(Vec::new()));
        let recorded_requests_for_thread = Arc::clone(&recorded_requests);
        let response_queue = Arc::new(Mutex::new(VecDeque::from(responses)));
        let response_queue_for_thread = Arc::clone(&response_queue);

        let handle = thread::spawn(move || {
            while let Some(response) = response_queue_for_thread.lock().expect("queue lock").pop_front() {
                let (mut stream, _) = listener.accept().expect("server should accept request");
                let body = read_http_request_body(&mut stream);
                let parsed: Value = serde_json::from_slice(&body).expect("request body should be valid JSON");
                recorded_requests_for_thread.lock().expect("requests lock").push(parsed);
                write_http_response(&mut stream, response);
            }
        });

        Self { base_url: format!("http://{}", addr), recorded_requests, handle: Some(handle) }
    }

    fn recorded_requests(&self) -> Vec<Value> {
        self.recorded_requests.lock().expect("requests lock").clone()
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            handle.join().expect("test server thread should join cleanly");
        }
    }
}

fn read_http_request_body(stream: &mut TcpStream) -> Vec<u8> {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 4096];
    let header_end = loop {
        let bytes_read = stream.read(&mut chunk).expect("request should be readable");
        assert!(bytes_read > 0, "request ended before headers completed");
        buffer.extend_from_slice(&chunk[..bytes_read]);
        if let Some(idx) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
            break idx;
        }
    };

    let headers = String::from_utf8_lossy(&buffer[..header_end]);
    let content_length = headers
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.trim().eq_ignore_ascii_case("content-length").then(|| value.trim().parse::<usize>().ok()).flatten()
        })
        .unwrap_or(0);

    let body_start = header_end + 4;
    while buffer.len() < body_start + content_length {
        let bytes_read = stream.read(&mut chunk).expect("request body should be readable");
        assert!(bytes_read > 0, "request ended before body completed");
        buffer.extend_from_slice(&chunk[..bytes_read]);
    }

    buffer[body_start..body_start + content_length].to_vec()
}

fn write_http_response(stream: &mut TcpStream, response: ResponseSpec) {
    match response {
        ResponseSpec::Sse(chunks) => {
            let mut body = String::new();
            for chunk in chunks {
                body.push_str("data: ");
                body.push_str(&chunk.to_string());
                body.push_str("\n\n");
            }
            body.push_str("data: [DONE]\n\n");
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(response.as_bytes()).expect("SSE response should write");
        }
        ResponseSpec::Error { status, body } => {
            let response = format!(
                "HTTP/1.1 {} ERROR\r\ncontent-type: text/plain\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                status,
                body.len(),
                body
            );
            stream.write_all(response.as_bytes()).expect("error response should write");
        }
    }
    stream.flush().expect("response should flush");
}

fn run_oai_runner(
    server: &TestServer,
    working_dir: &Path,
    prompt: &str,
    extra_args: &[&str],
    extra_env: &[(&str, &str)],
) -> std::process::Output {
    let mut command = std::process::Command::cargo_bin("ao-oai-runner").expect("oai runner binary should exist");
    command
        .arg("run")
        .arg("-m")
        .arg("openrouter/minimax/minimax-m2.7")
        .arg("--api-base")
        .arg(&server.base_url)
        .arg("--api-key")
        .arg("test-key")
        .arg("--format")
        .arg("json")
        .arg("--working-dir")
        .arg(working_dir)
        .arg("--max-turns")
        .arg("4");
    command.args(extra_args);
    for (key, value) in extra_env {
        command.env(key, value);
    }
    command.arg(prompt);
    command.output().expect("oai runner command should execute")
}

fn stdout_events(output: &std::process::Output) -> Vec<Value> {
    let stdout = String::from_utf8(output.stdout.clone()).expect("stdout should be valid UTF-8");
    stdout
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str::<Value>(line).expect("stdout line should be valid JSON event"))
        .collect()
}

fn save_session(config_dir: &Path, session_id: &str, messages: &[Value]) {
    let sessions_dir = config_dir.join("sessions");
    std::fs::create_dir_all(&sessions_dir).expect("sessions dir should exist");
    std::fs::write(
        sessions_dir.join(format!("{session_id}.json")),
        serde_json::to_vec_pretty(messages).expect("session should serialize"),
    )
    .expect("session should be written");
}

#[test]
fn runner_executes_contiguous_read_only_tools_through_the_main_loop() {
    let tempdir = tempfile::tempdir().expect("tempdir should exist");
    std::fs::write(tempdir.path().join("alpha.txt"), "alpha").expect("alpha file should exist");
    std::fs::write(tempdir.path().join("beta.txt"), "beta").expect("beta file should exist");

    let server = TestServer::spawn(vec![
        ResponseSpec::Sse(vec![json!({
            "choices": [{
                "delta": {
                    "tool_calls": [
                        {
                            "index": 0,
                            "id": "call_alpha",
                            "function": {
                                "name": "read_file",
                                "arguments": "{\"path\":\"alpha.txt\"}"
                            }
                        },
                        {
                            "index": 1,
                            "id": "call_beta",
                            "function": {
                                "name": "read_file",
                                "arguments": "{\"path\":\"beta.txt\"}"
                            }
                        }
                    ]
                }
            }]
        })]),
        ResponseSpec::Sse(vec![json!({
            "choices": [{
                "delta": {}
            }]
        })]),
        ResponseSpec::Sse(vec![json!({
            "choices": [{
                "delta": {}
            }]
        })]),
        ResponseSpec::Sse(vec![json!({
            "choices": [{
                "delta": { "content": "batched done" }
            }]
        })]),
    ]);

    let output = run_oai_runner(&server, tempdir.path(), "read both files", &[], &[]);
    assert!(output.status.success(), "runner should succeed: {}", String::from_utf8_lossy(&output.stderr));

    let events = stdout_events(&output);
    let tool_calls: Vec<&Value> = events.iter().filter(|event| event["type"] == "tool_call").collect();
    let tool_results: Vec<&Value> = events.iter().filter(|event| event["type"] == "tool_result").collect();
    let results: Vec<&Value> = events.iter().filter(|event| event["type"] == "result").collect();

    assert_eq!(tool_calls.len(), 2);
    assert_eq!(tool_results.len(), 2);
    assert_eq!(results.last().and_then(|event| event["text"].as_str()), Some("batched done"));
    assert_eq!(tool_calls[0]["tool_name"], "read_file");
    assert_eq!(tool_calls[1]["tool_name"], "read_file");
    assert!(tool_results[0]["output"].as_str().unwrap_or_default().contains("alpha"));
    assert!(tool_results[1]["output"].as_str().unwrap_or_default().contains("beta"));

    let requests = server.recorded_requests();
    assert_eq!(requests.len(), 4);
    let second_messages = requests[1]["messages"].as_array().expect("messages should be an array");
    let tool_messages = second_messages.iter().filter(|message| message["role"] == "tool").count();
    assert_eq!(tool_messages, 2, "second request should include both tool results");
}

#[test]
fn runner_retries_after_context_pressure_api_failure_with_a_smaller_request() {
    let tempdir = tempfile::tempdir().expect("tempdir should exist");
    let ao_config_dir = tempdir.path().join("ao-config");
    let session_id = "context-recovery";

    let mut session_messages = vec![json!({
        "role": "system",
        "content": "You are helpful."
    })];
    for index in 0..8 {
        session_messages.push(json!({
            "role": "user",
            "content": format!("message-{index} {}", "x ".repeat(140))
        }));
    }
    save_session(&ao_config_dir, session_id, &session_messages);

    let server = TestServer::spawn(vec![
        ResponseSpec::Error {
            status: 400,
            body: "maximum context length exceeded".to_string(),
        },
        ResponseSpec::Sse(vec![json!({
            "choices": [{
                "delta": { "content": "summary" }
            }]
        })]),
        ResponseSpec::Sse(vec![json!({
            "choices": [{
                "delta": {}
            }]
        })]),
        ResponseSpec::Sse(vec![json!({
            "choices": [{
                "delta": {}
            }]
        })]),
        ResponseSpec::Sse(vec![json!({
            "choices": [{
                "delta": {}
            }]
        })]),
        ResponseSpec::Sse(vec![json!({
            "choices": [{
                "delta": { "content": "Recovered after retry" }
            }]
        })]),
    ]);

    let config_dir_string = ao_config_dir.to_string_lossy().to_string();
    let output = run_oai_runner(
        &server,
        tempdir.path(),
        "recover the request",
        &["--session-id", session_id, "--context-limit", "4000", "--max-tokens", "512"],
        &[("AO_CONFIG_DIR", &config_dir_string)],
    );
    assert!(output.status.success(), "runner should recover: {}", String::from_utf8_lossy(&output.stderr));

    let events = stdout_events(&output);
    let final_result = events
        .iter()
        .rev()
        .find(|event| event["type"] == "result")
        .and_then(|event| event["text"].as_str());
    assert_eq!(
        final_result,
        Some("Recovered after retry"),
        "stdout events: {}",
        String::from_utf8_lossy(&output.stdout)
    );

    let requests = server.recorded_requests();
    assert_eq!(requests.len(), 6);
    let first_messages = requests[0]["messages"].as_array().expect("messages should be an array");
    let second_messages = requests[2]["messages"].as_array().expect("messages should be an array");
    assert!(
        second_messages.len() < first_messages.len(),
        "recovery should retry with fewer messages ({} -> {})",
        first_messages.len(),
        second_messages.len()
    );
}

#[test]
fn runner_surfaces_shell_timeout_cleanup_through_the_main_loop() {
    let tempdir = tempfile::tempdir().expect("tempdir should exist");
    let leaked_path = tempdir.path().join("leaked.txt");
    let leaked_path_shell = format!("'{}'", leaked_path.display().to_string().replace('\'', r#"'"'"'"#));
    let command = format!("sh -c '(sleep 2; echo leaked > \"$1\") & wait' sh {}", leaked_path_shell);

    let server = TestServer::spawn(vec![
        ResponseSpec::Sse(vec![json!({
            "choices": [{
                "delta": {
                    "tool_calls": [{
                        "index": 0,
                        "id": "call_shell",
                        "function": {
                            "name": "execute_command",
                            "arguments": serde_json::json!({
                                "command": command,
                                "timeout_secs": 1
                            }).to_string()
                        }
                    }]
                }
            }]
        })]),
        ResponseSpec::Sse(vec![json!({
            "choices": [{
                "delta": {}
            }]
        })]),
        ResponseSpec::Sse(vec![json!({
            "choices": [{
                "delta": {}
            }]
        })]),
        ResponseSpec::Sse(vec![json!({
            "choices": [{
                "delta": { "content": "shell done" }
            }]
        })]),
    ]);

    let output = run_oai_runner(&server, tempdir.path(), "run the shell command", &[], &[]);
    assert!(output.status.success(), "runner should succeed: {}", String::from_utf8_lossy(&output.stderr));

    let events = stdout_events(&output);
    let shell_result = events
        .iter()
        .find(|event| event["type"] == "tool_result" && event["tool_name"] == "execute_command")
        .expect("expected execute_command tool result");
    assert!(
        shell_result["output"]
            .as_str()
            .unwrap_or_default()
            .contains("Command timed out after 1s")
    );
    assert_eq!(events.last().and_then(|event| event["text"].as_str()), Some("shell done"));

    thread::sleep(Duration::from_millis(1500));
    assert!(!leaked_path.exists(), "timed out command should not leave background work running");
}
