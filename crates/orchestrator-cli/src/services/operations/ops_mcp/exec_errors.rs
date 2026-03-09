use super::CliExecutionResult;
use serde_json::{json, Value};

pub(super) fn extract_cli_success_data(stdout_json: Option<Value>) -> Value {
    stdout_json
        .map(|envelope| match envelope {
            Value::Object(mut map) => map.remove("data").unwrap_or(Value::Object(map)),
            other => other,
        })
        .unwrap_or(Value::Null)
}

pub(super) fn build_tool_error_payload(tool_name: &str, result: &CliExecutionResult) -> Value {
    let mut payload = json!({ "tool": tool_name });
    if let Some(envelope) = &result.stdout_json {
        if let Some(error) = envelope.get("error") {
            payload["error"] = error.clone();
        } else if let Some(data) = envelope.get("data") {
            payload["error"] = data.clone();
        }
    }
    payload["exit_code"] = json!(result.exit_code);
    let stderr = result.stderr.trim().to_string();
    if !stderr.is_empty() {
        payload["stderr"] = json!(stderr);
    }
    payload
}

pub(super) fn batch_item_error_from_result(result: &CliExecutionResult) -> Value {
    let mut payload = json!({ "exit_code": result.exit_code });
    if let Some(envelope) = &result.stdout_json {
        if let Some(error) = envelope.get("error") {
            payload["error"] = error.clone();
        } else if let Some(data) = envelope.get("data") {
            payload["error"] = data.clone();
        }
    }
    let stderr = result.stderr.trim().to_string();
    if !stderr.is_empty() {
        payload["stderr"] = json!(stderr);
    }
    payload
}

#[cfg(test)]
pub(super) fn build_cli_error_payload(tool_name: &str, result: &CliExecutionResult) -> Value {
    let mut payload = json!({
        "tool": tool_name,
        "exit_code": result.exit_code,
    });

    if let Some(envelope) = result.stderr_json.as_ref().or(result.stdout_json.as_ref()) {
        if let Some(error) = envelope.get("error") {
            payload["error"] = error.clone();
        } else if let Some(data) = envelope.get("data") {
            payload["error"] = data.clone();
        }
    }

    let stderr = result.stderr.trim().to_string();
    if !stderr.is_empty() {
        payload["stderr"] = json!(stderr);
    }

    payload
}
