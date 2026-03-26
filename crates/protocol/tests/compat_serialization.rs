use protocol::{
    AgentControlAction, AgentControlRequest, AgentRunEvent, AgentRunRequest, ModelId, OutputStreamType,
    RequirementPriority, RunId, Timestamp, TokenUsage, ToolCallInfo, PROTOCOL_VERSION,
};
use serde_json::json;

#[test]
fn protocol_version_constant_is_stable() {
    assert_eq!(PROTOCOL_VERSION, "1.0.0");
}

#[test]
fn agent_run_request_serialization_shape_is_stable() {
    let request = AgentRunRequest {
        protocol_version: PROTOCOL_VERSION.to_string(),
        run_id: RunId("run-123".to_string()),
        model: ModelId("codex".to_string()),
        context: json!({"cwd":"/tmp/project","phase_id":"implement"}),
        timeout_secs: Some(900),
    };

    let value = serde_json::to_value(request).expect("serialize request");

    assert_eq!(value["protocol_version"], "1.0.0");
    assert_eq!(value["run_id"], "run-123");
    assert_eq!(value["model"], "codex");
    assert_eq!(value["timeout_secs"], 900);
    assert_eq!(value["context"]["phase_id"], "implement");
}

#[test]
fn agent_run_event_uses_kind_tag_and_snake_case_variants() {
    let event = AgentRunEvent::OutputChunk {
        run_id: RunId("run-123".to_string()),
        stream_type: OutputStreamType::Stdout,
        text: "hello".to_string(),
    };

    let value = serde_json::to_value(event).expect("serialize event");

    assert_eq!(value["kind"], "output_chunk");
    assert_eq!(value["run_id"], "run-123");
    assert_eq!(value["stream_type"], "stdout");
    assert_eq!(value["text"], "hello");
}

#[test]
fn agent_control_action_serialization_is_lowercase() {
    let action = AgentControlAction::Terminate;
    let value = serde_json::to_value(action).expect("serialize action");
    assert_eq!(value, json!("terminate"));
}

#[test]
fn metadata_tokens_shape_is_stable() {
    let event = AgentRunEvent::Metadata {
        run_id: RunId("run-123".to_string()),
        cost: Some(0.12),
        tokens: Some(TokenUsage { input: 120, output: 48, reasoning: Some(5), cache_read: Some(2), cache_write: None }),
        data: Some(json!({"provider":"oai-runner"})),
    };

    let value = serde_json::to_value(event).expect("serialize metadata event");

    assert_eq!(value["kind"], "metadata");
    assert_eq!(value["tokens"]["input"], 120);
    assert_eq!(value["tokens"]["output"], 48);
    assert_eq!(value["tokens"]["reasoning"], 5);
    assert_eq!(value["tokens"]["cache_read"], 2);
    assert!(value["tokens"]["cache_write"].is_null());
    assert_eq!(value["data"]["provider"], "oai-runner");
}

#[test]
fn tool_call_event_serialization_shape_is_stable() {
    let event = AgentRunEvent::ToolCall {
        run_id: RunId("run-123".to_string()),
        tool_info: ToolCallInfo {
            tool_name: "search_query".to_string(),
            parameters: json!({"q":"rust serde"}),
            timestamp: Timestamp::now(),
        },
    };

    let value = serde_json::to_value(event).expect("serialize tool call");

    assert_eq!(value["kind"], "tool_call");
    assert_eq!(value["run_id"], "run-123");
    assert_eq!(value["tool_info"]["tool_name"], "search_query");
    assert_eq!(value["tool_info"]["parameters"]["q"], "rust serde");
    assert!(value["tool_info"]["timestamp"].is_string(), "tool_call timestamp must remain string-encoded");
}

#[test]
fn control_request_roundtrip_shape_is_stable() {
    let request =
        AgentControlRequest { run_id: RunId("run-control-1".to_string()), action: AgentControlAction::Terminate };

    let value = serde_json::to_value(&request).expect("serialize control request");
    assert_eq!(value["run_id"], "run-control-1");
    assert_eq!(value["action"], "terminate");

    let decoded: AgentControlRequest = serde_json::from_value(value).expect("deserialize control request");
    assert_eq!(decoded.run_id.0, "run-control-1");
    assert_eq!(decoded.action, AgentControlAction::Terminate);
}

#[test]
fn requirement_priority_serialization_is_lowercase_and_stable() {
    let value = serde_json::to_value(RequirementPriority::Must).expect("serialize requirement priority");
    assert_eq!(value, json!("must"));

    let decoded: RequirementPriority = serde_json::from_value(json!("wont")).expect("deserialize requirement priority");
    assert_eq!(decoded, RequirementPriority::Wont);
}
