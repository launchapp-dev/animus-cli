use protocol::*;

#[test]
fn test_agent_run_request_roundtrip() {
    let req = AgentRunRequest {
        protocol_version: PROTOCOL_VERSION.to_string(),
        run_id: RunId("run-123".into()),
        model: ModelId("claude-sonnet-4".into()),
        context: serde_json::json!({ "tool": "claude", "prompt": "Hello", "cwd": "/tmp" }),
        timeout_secs: Some(300),
    };

    let json = serde_json::to_string(&req).unwrap();
    let parsed: AgentRunRequest = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed.run_id.0, "run-123");
}

#[test]
fn test_agent_run_event_serialization() {
    let evt = AgentRunEvent::OutputChunk {
        run_id: RunId("run-123".into()),
        stream_type: OutputStreamType::Stdout,
        text: "chunk".into(),
    };
    let json = serde_json::to_string(&evt).unwrap();
    assert!(json.contains("output_chunk"));
    assert!(json.contains("stdout"));
}

#[test]
fn test_agent_control_request() {
    let req = AgentControlRequest {
        run_id: RunId("run-456".into()),
        action: AgentControlAction::Pause,
    };

    let json = serde_json::to_string(&req).unwrap();
    let parsed: AgentControlRequest = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed.action, AgentControlAction::Pause);
}

#[test]
fn test_agent_status_response() {
    let resp = AgentStatusResponse {
        run_id: RunId("run-789".into()),
        status: AgentStatus::Running,
        elapsed_ms: 45000,
        started_at: Timestamp::now(),
        completed_at: None,
    };

    let json = serde_json::to_string(&resp).unwrap();
    let parsed: AgentStatusResponse = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed.status, AgentStatus::Running);
    assert_eq!(parsed.elapsed_ms, 45000);
}

#[test]
fn test_agent_status_query_response_status_roundtrip() {
    let response = AgentStatusQueryResponse::Status(AgentStatusResponse {
        run_id: RunId("run-status".into()),
        status: AgentStatus::Completed,
        elapsed_ms: 1234,
        started_at: Timestamp::now(),
        completed_at: Some(Timestamp::now()),
    });

    let json = serde_json::to_string(&response).expect("serialize status query response");
    let parsed: AgentStatusQueryResponse =
        serde_json::from_str(&json).expect("deserialize status query response");

    match parsed {
        AgentStatusQueryResponse::Status(status) => {
            assert_eq!(status.run_id.0, "run-status");
            assert_eq!(status.status, AgentStatus::Completed);
            assert_eq!(status.elapsed_ms, 1234);
            assert!(status.completed_at.is_some());
        }
        AgentStatusQueryResponse::Error(_) => panic!("expected status variant"),
    }
}

#[test]
fn test_agent_status_query_response_not_found_roundtrip() {
    let response = AgentStatusQueryResponse::Error(AgentStatusErrorResponse {
        run_id: RunId("run-missing".into()),
        code: AgentStatusErrorCode::NotFound,
        message: "run not found: run-missing".to_string(),
    });

    let json = serde_json::to_string(&response).expect("serialize error query response");
    let parsed: AgentStatusQueryResponse =
        serde_json::from_str(&json).expect("deserialize error query response");

    match parsed {
        AgentStatusQueryResponse::Error(error) => {
            assert_eq!(error.run_id.0, "run-missing");
            assert_eq!(error.code, AgentStatusErrorCode::NotFound);
            assert_eq!(error.message, "run not found: run-missing");
        }
        AgentStatusQueryResponse::Status(_) => panic!("expected error variant"),
    }
}

#[test]
fn test_model_availability_enum() {
    let status = ModelStatus {
        model: ModelId("claude-sonnet-4".into()),
        availability: ModelAvailability::Available,
        details: None,
    };

    let json = serde_json::to_string(&status).unwrap();
    assert!(json.contains("available"));
}

#[test]
fn test_project_model_config() {
    let config = ProjectModelConfig {
        project_id: ProjectId("proj-123".into()),
        allowed_models: vec![
            ModelId("claude-sonnet-4".into()),
            ModelId("gpt-4-turbo".into()),
        ],
        phase_defaults: WorkflowPhaseModelDefaults {
            design: Some(ModelId("gemini-3-pro".into())),
            development: Some(ModelId("claude-sonnet-4".into())),
            quality_assurance: Some(ModelId("claude-sonnet-4".into())),
            review: Some(ModelId("gpt-4-turbo".into())),
            deploy: None,
        },
        fallback_model: Some(ModelId("claude-sonnet-4".into())),
    };

    let json = serde_json::to_string(&config).unwrap();
    let parsed: ProjectModelConfig = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed.allowed_models.len(), 2);
    assert!(parsed.phase_defaults.design.is_some());
}

#[test]
fn test_runner_status_request_rejects_unexpected_fields() {
    let parsed = serde_json::from_str::<RunnerStatusRequest>(
        r#"{"run_id":"run-cli-control","action":"terminate"}"#,
    );
    assert!(
        parsed.is_err(),
        "runner status request must reject control-shaped payloads"
    );
}

#[test]
fn test_runner_status_response_roundtrip_includes_protocol_metadata() {
    let response = RunnerStatusResponse {
        active_agents: 2,
        protocol_version: PROTOCOL_VERSION.to_string(),
        build_id: Some("1700000000.123-987654".to_string()),
    };

    let json = serde_json::to_string(&response).expect("serialize runner status");
    let parsed: RunnerStatusResponse =
        serde_json::from_str(&json).expect("deserialize runner status");

    assert_eq!(parsed.active_agents, 2);
    assert_eq!(parsed.protocol_version, PROTOCOL_VERSION);
    assert_eq!(parsed.build_id.as_deref(), Some("1700000000.123-987654"));
}

#[test]
fn test_runner_status_response_deserializes_legacy_shape() {
    let parsed: RunnerStatusResponse =
        serde_json::from_str(r#"{"active_agents":3}"#).expect("deserialize legacy status");

    assert_eq!(parsed.active_agents, 3);
    assert_eq!(parsed.protocol_version, PROTOCOL_VERSION);
    assert!(parsed.build_id.is_none());
}
