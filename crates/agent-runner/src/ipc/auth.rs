use anyhow::Result;
use protocol::{Config, IpcAuthFailureCode, IpcAuthRequest, IpcAuthResult};
use tokio::io::{AsyncWrite, AsyncWriteExt};
use tracing::warn;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AuthResult {
    Accepted,
    Rejected,
}

pub(super) async fn authenticate_first_payload<W: AsyncWrite + Unpin>(
    payload: &str,
    writer: &mut W,
    connection_id: u64,
) -> Result<AuthResult> {
    let response = match validate_payload(payload) {
        Ok(()) => IpcAuthResult::ok(),
        Err(code) => {
            warn!(
                connection_id,
                failure_code = code.as_str(),
                "Rejected IPC connection during authentication"
            );
            IpcAuthResult::rejected(code, auth_failure_message(code))
        }
    };

    let line = serde_json::to_string(&response)?;
    writer.write_all(line.as_bytes()).await?;
    writer.write_all(b"\n").await?;
    writer.flush().await?;

    if response.ok {
        Ok(AuthResult::Accepted)
    } else {
        Ok(AuthResult::Rejected)
    }
}

fn validate_payload(payload: &str) -> std::result::Result<(), IpcAuthFailureCode> {
    let request = serde_json::from_str::<IpcAuthRequest>(payload)
        .map_err(|_| IpcAuthFailureCode::MalformedAuthPayload)?;
    let expected_token = Config::load_global()
        .and_then(|config| config.get_token())
        .map_err(|_| IpcAuthFailureCode::ServerTokenUnavailable)?;

    if request.token != expected_token {
        return Err(IpcAuthFailureCode::InvalidToken);
    }

    Ok(())
}

fn auth_failure_message(code: IpcAuthFailureCode) -> &'static str {
    match code {
        IpcAuthFailureCode::MalformedAuthPayload => "malformed auth payload",
        IpcAuthFailureCode::InvalidToken => "unauthorized",
        IpcAuthFailureCode::ServerTokenUnavailable => "server token unavailable",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn env_lock() -> &'static Mutex<()> {
        crate::test_env_lock()
    }

    use protocol::test_utils::EnvVarGuard;

    fn temp_config_dir(label: &str) -> String {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time should be after unix epoch")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "agent-runner-ipc-auth-{label}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("temp config dir should be created");
        dir.to_string_lossy().to_string()
    }

    fn write_config_with_token(config_dir: &str, token: Option<&str>) {
        let config = match token {
            Some(t) => serde_json::json!({ "agent_runner_token": t }),
            None => serde_json::json!({}),
        };
        let config_path = std::path::Path::new(config_dir).join("config.json");
        std::fs::write(config_path, config.to_string()).expect("write config.json");
    }

    #[test]
    fn validate_payload_accepts_matching_token() {
        let _lock = env_lock().lock().expect("env lock");
        let config_dir = temp_config_dir("matching-token");
        let _config_dir = EnvVarGuard::set("AO_CONFIG_DIR", Some(&config_dir));
        let _legacy_config_dir =
            EnvVarGuard::set("AGENT_ORCHESTRATOR_CONFIG_DIR", Some(&config_dir));
        let _token = EnvVarGuard::set("AGENT_RUNNER_TOKEN", None);
        write_config_with_token(&config_dir, Some("runner-secret"));

        let payload = r#"{"kind":"ipc_auth","token":"runner-secret"}"#;
        assert!(validate_payload(payload).is_ok());
    }

    #[test]
    fn validate_payload_rejects_malformed_request() {
        let _lock = env_lock().lock().expect("env lock");
        let config_dir = temp_config_dir("malformed");
        let _config_dir = EnvVarGuard::set("AO_CONFIG_DIR", Some(&config_dir));
        let _legacy_config_dir =
            EnvVarGuard::set("AGENT_ORCHESTRATOR_CONFIG_DIR", Some(&config_dir));
        let _token = EnvVarGuard::set("AGENT_RUNNER_TOKEN", None);
        write_config_with_token(&config_dir, Some("runner-secret"));

        let payload = r#"{"run_id":"run-123"}"#;
        let error = validate_payload(payload).expect_err("payload should be rejected");
        assert_eq!(error, IpcAuthFailureCode::MalformedAuthPayload);
    }

    #[test]
    fn validate_payload_rejects_incorrect_token() {
        let _lock = env_lock().lock().expect("env lock");
        let config_dir = temp_config_dir("incorrect-token");
        let _config_dir = EnvVarGuard::set("AO_CONFIG_DIR", Some(&config_dir));
        let _legacy_config_dir =
            EnvVarGuard::set("AGENT_ORCHESTRATOR_CONFIG_DIR", Some(&config_dir));
        let _token = EnvVarGuard::set("AGENT_RUNNER_TOKEN", None);
        write_config_with_token(&config_dir, Some("runner-secret"));

        let payload = r#"{"kind":"ipc_auth","token":"wrong-token"}"#;
        let error = validate_payload(payload).expect_err("token mismatch should be rejected");
        assert_eq!(error, IpcAuthFailureCode::InvalidToken);
    }

    #[test]
    fn validate_payload_rejects_missing_server_token() {
        let _lock = env_lock().lock().expect("env lock");
        let config_dir = temp_config_dir("missing-token");
        let _config_dir = EnvVarGuard::set("AO_CONFIG_DIR", Some(&config_dir));
        let _legacy_config_dir =
            EnvVarGuard::set("AGENT_ORCHESTRATOR_CONFIG_DIR", Some(&config_dir));
        let _token = EnvVarGuard::set("AGENT_RUNNER_TOKEN", None);
        write_config_with_token(&config_dir, None);

        let payload = r#"{"kind":"ipc_auth","token":"runner-secret"}"#;
        let error = validate_payload(payload).expect_err("missing token should fail closed");
        assert_eq!(error, IpcAuthFailureCode::ServerTokenUnavailable);
    }
}
