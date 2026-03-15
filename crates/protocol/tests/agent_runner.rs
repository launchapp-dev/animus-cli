use protocol::test_utils::EnvVarGuard;
use protocol::*;
use std::collections::BTreeMap;
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

fn env_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn temp_config_dir(label: &str) -> std::path::PathBuf {
    let nanos =
        SystemTime::now().duration_since(UNIX_EPOCH).expect("system time should be after unix epoch").as_nanos();
    let dir = std::env::temp_dir().join(format!("protocol-config-{label}-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create temp config dir");
    dir
}

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
    let req = AgentControlRequest { run_id: RunId("run-456".into()), action: AgentControlAction::Pause };

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
    let parsed: AgentStatusQueryResponse = serde_json::from_str(&json).expect("deserialize status query response");

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
    let parsed: AgentStatusQueryResponse = serde_json::from_str(&json).expect("deserialize error query response");

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
        allowed_models: vec![ModelId("claude-sonnet-4".into()), ModelId("gpt-4-turbo".into())],
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
    let parsed = serde_json::from_str::<RunnerStatusRequest>(r#"{"run_id":"run-cli-control","action":"terminate"}"#);
    assert!(parsed.is_err(), "runner status request must reject control-shaped payloads");
}

#[test]
fn test_runner_status_response_roundtrip_includes_protocol_metadata() {
    let response = RunnerStatusResponse {
        active_agents: 2,
        protocol_version: PROTOCOL_VERSION.to_string(),
        build_id: Some("1700000000.123-987654".to_string()),
        metrics: None,
    };

    let json = serde_json::to_string(&response).expect("serialize runner status");
    let parsed: RunnerStatusResponse = serde_json::from_str(&json).expect("deserialize runner status");

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

#[test]
fn test_ipc_auth_request_roundtrip() {
    let request = IpcAuthRequest::new("secret-token");
    let json = serde_json::to_string(&request).expect("serialize auth request");
    assert_eq!(json, r#"{"kind":"ipc_auth","token":"secret-token"}"#);

    let parsed: IpcAuthRequest = serde_json::from_str(&json).expect("deserialize auth request");
    assert_eq!(parsed.kind, IpcAuthRequestKind::IpcAuth);
    assert_eq!(parsed.token, "secret-token");
}

#[test]
fn test_ipc_auth_request_rejects_unknown_fields() {
    let parsed = serde_json::from_str::<IpcAuthRequest>(r#"{"kind":"ipc_auth","token":"secret","extra":"value"}"#);
    assert!(parsed.is_err(), "auth request must reject unknown fields to keep handshake strict");
}

#[test]
fn test_ipc_auth_result_failure_roundtrip() {
    let result = IpcAuthResult::rejected(IpcAuthFailureCode::InvalidToken, "unauthorized");
    let json = serde_json::to_string(&result).expect("serialize auth failure");
    assert_eq!(json, r#"{"kind":"ipc_auth_result","ok":false,"code":"invalid_token","message":"unauthorized"}"#);

    let parsed: IpcAuthResult = serde_json::from_str(&json).expect("deserialize auth failure");
    assert!(!parsed.ok);
    assert_eq!(parsed.code, Some(IpcAuthFailureCode::InvalidToken));
    assert_eq!(parsed.message.as_deref(), Some("unauthorized"));
}

#[test]
fn test_config_get_token_returns_config_value() {
    let config = Config { agent_runner_token: Some("config-token".to_string()), mcp_servers: BTreeMap::new() };

    let token = config.get_token().expect("config token should resolve");
    assert_eq!(token, "config-token");
}

#[test]
fn test_config_get_token_rejects_blank_config_value() {
    let config = Config { agent_runner_token: Some("   ".to_string()), mcp_servers: BTreeMap::new() };

    let error = config.get_token().expect_err("blank config token should fail closed");
    assert!(error.to_string().contains("agent_runner_token"), "error should mention config token source");
}

#[test]
fn test_config_get_token_rejects_missing_token() {
    let config = Config { agent_runner_token: None, mcp_servers: BTreeMap::new() };

    let error = config.get_token().expect_err("missing token should fail closed");
    assert!(error.to_string().contains("agent_runner_token"), "error should mention missing config token");
}

#[test]
fn test_config_load_from_dir_creates_default_config_file() {
    let config_dir = temp_config_dir("load-default");
    let config_path = config_dir.join("config.json");
    let _ = std::fs::remove_file(&config_path);

    let loaded = Config::load_from_dir(&config_dir).expect("load scoped config");
    assert!(loaded.agent_runner_token.is_none());
    assert!(config_path.exists(), "loading from a fresh directory should create config.json");

    let _ = std::fs::remove_dir_all(config_dir);
}

#[test]
fn test_config_load_from_dir_reads_existing_token() {
    let config_dir = temp_config_dir("load-existing");
    let config_path = config_dir.join("config.json");
    std::fs::write(
        &config_path,
        r#"{
  "agent_runner_token": "scoped-token"
}"#,
    )
    .expect("write scoped config file");

    let loaded = Config::load_from_dir(&config_dir).expect("load scoped config");
    assert_eq!(loaded.agent_runner_token.as_deref(), Some("scoped-token"));

    let _ = std::fs::remove_dir_all(config_dir);
}

#[test]
fn test_ensure_token_exists_generates_token_when_missing() {
    let config_dir = temp_config_dir("ensure-missing");
    let config_path = config_dir.join("config.json");
    let _ = std::fs::remove_file(&config_path);

    Config::ensure_token_exists(&config_dir).expect("ensure_token_exists should succeed");

    let loaded = Config::load_from_dir(&config_dir).expect("reload config");
    let token = loaded.agent_runner_token.expect("token should be set");
    assert!(!token.is_empty(), "token should not be empty");
    assert!(uuid::Uuid::parse_str(&token).is_ok(), "token should be a valid UUID");

    let _ = std::fs::remove_dir_all(config_dir);
}

#[test]
fn test_ensure_token_exists_preserves_existing_token() {
    let config_dir = temp_config_dir("ensure-existing");
    let config_path = config_dir.join("config.json");
    std::fs::write(&config_path, r#"{ "agent_runner_token": "keep-me" }"#).expect("write config");

    Config::ensure_token_exists(&config_dir).expect("ensure_token_exists should succeed");

    let loaded = Config::load_from_dir(&config_dir).expect("reload config");
    assert_eq!(loaded.agent_runner_token.as_deref(), Some("keep-me"), "existing token should be preserved");

    let _ = std::fs::remove_dir_all(config_dir);
}

#[test]
fn test_ensure_token_exists_replaces_null_token() {
    let config_dir = temp_config_dir("ensure-null");
    let config_path = config_dir.join("config.json");
    std::fs::write(&config_path, r#"{ "agent_runner_token": null }"#).expect("write config");

    Config::ensure_token_exists(&config_dir).expect("ensure_token_exists should succeed");

    let loaded = Config::load_from_dir(&config_dir).expect("reload config");
    let token = loaded.agent_runner_token.expect("token should be set");
    assert!(!token.is_empty(), "null token should be replaced");

    let _ = std::fs::remove_dir_all(config_dir);
}
