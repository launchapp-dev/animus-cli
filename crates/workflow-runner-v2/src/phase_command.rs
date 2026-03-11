use anyhow::{anyhow, Result};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use std::path::{Component, Path};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::process::Command as TokioCommand;
use tokio::time::timeout;
use std::process::Stdio;

use crate::config_context::RuntimeConfigContext;
use crate::payload_traversal::parse_phase_decision_from_text;
use crate::phase_output::{format_output_chunk_for_display, format_tool_call_for_display};

#[derive(Debug, Clone, Copy)]
pub(crate) struct CommandExecutionContext<'a> {
    pub project_root: &'a str,
    pub execution_cwd: &'a str,
    pub workflow_id: &'a str,
    pub phase_id: &'a str,
    pub workflow_ref: &'a str,
    pub subject_id: &'a str,
    pub subject_title: &'a str,
    pub subject_description: &'a str,
    pub pipeline_vars: Option<&'a HashMap<String, String>>,
    pub dispatch_input: Option<&'a str>,
    pub schedule_input: Option<&'a str>,
}

#[derive(Debug, Clone)]
pub(crate) struct CommandExecutionResult {
    pub exit_code: i32,
    pub program: String,
    pub args: Vec<String>,
    pub stdout: String,
    pub stderr: String,
    pub cwd: String,
    pub duration_ms: u64,
    pub parsed_payload: Option<Value>,
    pub phase_decision: Option<orchestrator_core::PhaseDecision>,
    pub failure_summary: Option<String>,
}

pub(crate) fn build_command_template_vars(
    context: &CommandExecutionContext<'_>,
) -> HashMap<String, String> {
    let mut vars = HashMap::from([
        ("project_root".to_string(), context.project_root.to_string()),
        ("execution_cwd".to_string(), context.execution_cwd.to_string()),
        ("workflow_id".to_string(), context.workflow_id.to_string()),
        ("phase_id".to_string(), context.phase_id.to_string()),
        ("workflow_ref".to_string(), context.workflow_ref.to_string()),
        ("subject_id".to_string(), context.subject_id.to_string()),
        ("subject_title".to_string(), context.subject_title.to_string()),
        ("subject_description".to_string(), context.subject_description.to_string()),
    ]);

    if let Some(pipeline_vars) = context.pipeline_vars {
        for (key, value) in pipeline_vars {
            vars.entry(key.clone()).or_insert_with(|| value.clone());
        }
    }

    if let Some(dispatch_input) = context.dispatch_input.filter(|value| !value.is_empty()) {
        vars.entry("dispatch_input".to_string())
            .or_insert_with(|| dispatch_input.to_string());
        if context.subject_id.starts_with("schedule:") {
            vars.entry("schedule_input".to_string())
                .or_insert_with(|| dispatch_input.to_string());
        }
    } else if let Some(schedule_input) = context.schedule_input.filter(|value| !value.is_empty()) {
        vars.entry("schedule_input".to_string())
            .or_insert_with(|| schedule_input.to_string());
        vars.entry("dispatch_input".to_string())
            .or_insert_with(|| schedule_input.to_string());
    }

    vars
}

fn resolve_command_cwd(
    context: &CommandExecutionContext<'_>,
    command: &orchestrator_core::PhaseCommandDefinition,
    template_vars: &HashMap<String, String>,
) -> Result<String> {
    match command.cwd_mode {
        orchestrator_core::CommandCwdMode::ProjectRoot => Ok(context.project_root.to_string()),
        orchestrator_core::CommandCwdMode::TaskRoot => Ok(context.execution_cwd.to_string()),
        orchestrator_core::CommandCwdMode::Path => {
            let expanded = command
                .cwd_path
                .as_deref()
                .map(|value| orchestrator_config::expand_variables(value, template_vars))
                .ok_or_else(|| anyhow!("command.cwd_path is required when cwd_mode='path'"))?;
            let raw = expanded.trim();
            if raw.is_empty() {
                return Err(anyhow!("command.cwd_path is required when cwd_mode='path'"));
            }
            let relative = Path::new(raw);
            if relative.is_absolute() {
                return Err(anyhow!("command.cwd_path must be relative when cwd_mode='path'"));
            }
            if relative
                .components()
                .any(|component| matches!(component, Component::ParentDir))
            {
                return Err(anyhow!("command.cwd_path cannot contain '..' components"));
            }
            Ok(Path::new(context.project_root)
                .join(relative)
                .display()
                .to_string())
        }
    }
}

fn is_program_allowlisted(program: &str, allowlist: &[String]) -> bool {
    let command = Path::new(program)
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or(program)
        .trim()
        .to_ascii_lowercase();
    if command.is_empty() {
        return false;
    }
    allowlist
        .iter()
        .map(String::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .any(|candidate| candidate.eq_ignore_ascii_case(command.as_str()))
}

fn command_phase_category(
    command: &orchestrator_core::PhaseCommandDefinition,
    phase_id: &str,
) -> String {
    if let Some(category) = command.category.as_deref() {
        return category.to_string();
    }

    let normalized_phase = phase_id.to_ascii_lowercase();
    let normalized_program = command.program.to_ascii_lowercase();
    let normalized_args: Vec<String> = command
        .args
        .iter()
        .map(|v| v.to_ascii_lowercase())
        .collect();

    if normalized_phase.contains("test")
        || normalized_program.contains("cargo")
            && normalized_args.iter().any(|arg| arg == "test")
    {
        "test".to_string()
    } else if normalized_phase.contains("lint")
        || normalized_program.contains("clippy")
        || normalized_program.contains("rustfmt")
        || normalized_args
            .iter()
            .any(|arg| arg.contains("clippy") || arg.contains("fmt"))
    {
        "lint".to_string()
    } else if normalized_phase.contains("build")
        || normalized_program.contains("cargo")
            && normalized_args.iter().any(|arg| arg == "build")
    {
        "build".to_string()
    } else {
        "command".to_string()
    }
}

fn command_phase_evidence_kind(
    command: &orchestrator_core::PhaseCommandDefinition,
    phase_id: &str,
    success: bool,
) -> orchestrator_core::PhaseEvidenceKind {
    let category = command_phase_category(command, phase_id);
    if category == "test" {
        if success {
            orchestrator_core::PhaseEvidenceKind::TestsPassed
        } else {
            orchestrator_core::PhaseEvidenceKind::TestsFailed
        }
    } else {
        orchestrator_core::PhaseEvidenceKind::Custom
    }
}

fn extract_failing_tests(
    command: &orchestrator_core::PhaseCommandDefinition,
    stdout: &str,
    stderr: &str,
) -> Vec<String> {
    let pattern_str = command
        .failure_pattern
        .as_deref()
        .unwrap_or(r"test (.+) \.\.\. FAILED");

    let re = match regex::Regex::new(pattern_str) {
        Ok(re) => re,
        Err(_) => return Vec::new(),
    };

    let mut failing = Vec::new();
    for text in [stdout, stderr] {
        for line in text.lines() {
            if let Some(captures) = re.captures(line.trim()) {
                let candidate = captures
                    .get(1)
                    .map(|m| m.as_str().trim().to_string())
                    .unwrap_or_default();
                if !candidate.is_empty() && !failing.contains(&candidate) {
                    failing.push(candidate);
                }
            }
        }
    }
    failing
}

fn summarize_output_excerpt(
    text: &str,
    max_len: usize,
) -> Option<String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    let excerpt = if trimmed.chars().count() > max_len {
        let mut shortened = trimmed.chars().take(max_len).collect::<String>();
        shortened.push_str("...");
        shortened
    } else {
        trimmed.to_string()
    };
    Some(excerpt)
}

pub(crate) fn build_command_phase_decision(
    command: &orchestrator_core::PhaseCommandDefinition,
    phase_id: &str,
    exit_code: i32,
    failure_summary: Option<&str>,
) -> orchestrator_core::PhaseDecision {
    let success = failure_summary.is_none();
    let kind = command_phase_evidence_kind(command, phase_id, success);
    let reason = failure_summary
        .map(str::to_string)
        .unwrap_or_else(|| format!("Command `{}` completed successfully", command.program));

    let verdict = if success {
        match command.on_success_verdict.as_deref() {
            Some("rework") => orchestrator_core::PhaseDecisionVerdict::Rework,
            Some("fail") => orchestrator_core::PhaseDecisionVerdict::Fail,
            Some("skip") => orchestrator_core::PhaseDecisionVerdict::Skip,
            _ => orchestrator_core::PhaseDecisionVerdict::Advance,
        }
    } else {
        match command.on_failure_verdict.as_deref() {
            Some("advance") => orchestrator_core::PhaseDecisionVerdict::Advance,
            Some("fail") => orchestrator_core::PhaseDecisionVerdict::Fail,
            Some("skip") => orchestrator_core::PhaseDecisionVerdict::Skip,
            _ => orchestrator_core::PhaseDecisionVerdict::Rework,
        }
    };

    let confidence = command.confidence.unwrap_or(1.0);

    let risk = if success {
        orchestrator_core::WorkflowDecisionRisk::Low
    } else {
        match command.failure_risk.as_deref() {
            Some("low") => orchestrator_core::WorkflowDecisionRisk::Low,
            Some("high") => orchestrator_core::WorkflowDecisionRisk::High,
            _ => orchestrator_core::WorkflowDecisionRisk::Medium,
        }
    };

    orchestrator_core::PhaseDecision {
        kind: "phase_decision".to_string(),
        phase_id: phase_id.to_string(),
        verdict,
        confidence,
        risk,
        reason: reason.clone(),
        evidence: vec![orchestrator_core::PhaseEvidence {
            kind,
            description: format!("Command `{}` exited with code {exit_code}", command.program),
            file_path: None,
            value: Some(serde_json::json!({
                "program": command.program,
                "args": command.args,
                "exit_code": exit_code
            })),
        }],
        guardrail_violations: vec![],
        commit_message: None,
        target_phase: None,
    }
}

pub(crate) fn build_command_result_payload(
    command: &orchestrator_core::PhaseCommandDefinition,
    phase_id: &str,
    contract_kind: Option<&str>,
    command_result: &CommandExecutionResult,
    phase_decision: &orchestrator_core::PhaseDecision,
) -> Value {
    let mut payload = match command_result.parsed_payload.clone() {
        Some(Value::Object(map)) => Value::Object(map),
        Some(other) => serde_json::json!({ "raw_payload": other }),
        None => serde_json::json!({}),
    };

    payload["kind"] = Value::String(
        payload
            .get("kind")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .unwrap_or(contract_kind.unwrap_or("phase_result"))
            .to_string(),
    );
    payload["phase_id"] = Value::String(phase_id.to_string());
    payload["verdict"] =
        Value::String(format!("{:?}", phase_decision.verdict).to_ascii_lowercase());
    payload["reason"] = Value::String(phase_decision.reason.clone());
    payload["confidence"] = serde_json::json!(phase_decision.confidence);
    payload["risk"] = Value::String(format!("{:?}", phase_decision.risk).to_ascii_lowercase());
    payload["evidence"] =
        serde_json::to_value(&phase_decision.evidence).unwrap_or(Value::Array(vec![]));
    payload["exit_code"] = serde_json::json!(command_result.exit_code);
    payload["command"] = serde_json::json!({
        "program": command_result.program,
        "args": command_result.args
    });
    payload["duration_ms"] = serde_json::json!(command_result.duration_ms);

    let excerpt_max = command.excerpt_max_chars.unwrap_or(800);
    let category = command_phase_category(command, phase_id);

    if let Some(summary) = command_result.failure_summary.as_deref() {
        payload["failure_summary"] = Value::String(summary.to_string());
        payload["failure_category"] = Value::String(format!("{category}_failed"));
        let failing_tests = extract_failing_tests(command, &command_result.stdout, &command_result.stderr);
        if !failing_tests.is_empty() {
            payload["failing_tests"] = Value::Array(
                failing_tests
                    .into_iter()
                    .map(Value::String)
                    .collect::<Vec<_>>(),
            );
        }
    }

    if let Some(stdout_excerpt) = summarize_output_excerpt(&command_result.stdout, excerpt_max) {
        payload["stdout_excerpt"] = Value::String(stdout_excerpt);
    }
    if let Some(stderr_excerpt) = summarize_output_excerpt(&command_result.stderr, excerpt_max) {
        payload["stderr_excerpt"] = Value::String(stderr_excerpt);
    }

    payload
}

#[derive(Debug)]
struct CommandStreamCapture {
    text: String,
    phase_decision: Option<orchestrator_core::PhaseDecision>,
}

async fn capture_command_stream<R>(
    reader: R,
    phase_id: &str,
    stream_output: bool,
    stream_verbose: bool,
    use_colors: bool,
) -> Result<CommandStreamCapture>
where
    R: AsyncRead + Unpin,
{
    let mut lines = BufReader::new(reader).lines();
    let mut text = String::new();
    let mut phase_decision = None;

    while let Some(line) = lines.next_line().await? {
        text.push_str(&line);
        text.push('\n');

        if phase_decision.is_none() {
            phase_decision = parse_phase_decision_from_text(&line, phase_id);
        }

        if stream_output {
            use std::io::Write as _;
            let display =
                format_output_chunk_for_display(&line, stream_verbose, use_colors, "command")
                    .unwrap_or_else(|| format!("{line}\n"));
            let _ = write!(std::io::stderr(), "{}", display);
        }
    }

    Ok(CommandStreamCapture {
        text,
        phase_decision,
    })
}

pub(crate) async fn run_workflow_phase_with_command(
    context: &CommandExecutionContext<'_>,
    runtime_config: &orchestrator_core::AgentRuntimeConfig,
    command: &orchestrator_core::PhaseCommandDefinition,
) -> Result<CommandExecutionResult> {
    if !is_program_allowlisted(&command.program, &runtime_config.tools_allowlist) {
        return Err(anyhow!(
            "phase '{}' command '{}' is not in tools_allowlist",
            context.phase_id,
            command.program
        ));
    }

    let template_vars = build_command_template_vars(context);
    let args = command
        .args
        .iter()
        .map(|arg| orchestrator_config::expand_variables(arg, &template_vars))
        .collect::<Vec<_>>();
    let env = command
        .env
        .iter()
        .map(|(key, value)| {
            (
                key.clone(),
                orchestrator_config::expand_variables(value, &template_vars),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let cwd = resolve_command_cwd(context, command, &template_vars)?;
    let started = std::time::Instant::now();

    let mut process = TokioCommand::new(&command.program);
    process
        .args(&args)
        .current_dir(&cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    for (key, value) in &env {
        process.env(key, value);
    }

    let mut child = process.spawn()?;
    let stdout_reader = child
        .stdout
        .take()
        .ok_or_else(|| anyhow!("failed to capture stdout for command phase"))?;
    let stderr_reader = child
        .stderr
        .take()
        .ok_or_else(|| anyhow!("failed to capture stderr for command phase"))?;
    let stream_level = std::env::var("AO_STREAM_PHASE_OUTPUT").unwrap_or_default();
    let stream_normal = matches!(stream_level.as_str(), "1" | "normal");
    let stream_verbose = stream_level == "verbose";
    let stream_to_stderr = stream_normal || stream_verbose;
    let use_colors = stream_to_stderr && {
        use std::io::IsTerminal;
        std::io::stderr().is_terminal()
    };
    let phase_id = context.phase_id.to_string();
    let phase_id2 = phase_id.clone();
    let stdout_task = tokio::spawn(capture_command_stream(
        stdout_reader,
        Box::leak(phase_id.into_boxed_str()),
        stream_to_stderr,
        stream_verbose,
        use_colors,
    ));
    let stderr_task = tokio::spawn(capture_command_stream(
        stderr_reader,
        Box::leak(phase_id2.into_boxed_str()),
        stream_to_stderr,
        stream_verbose,
        use_colors,
    ));

    let status = if let Some(timeout_secs) = command.timeout_secs {
        match timeout(Duration::from_secs(timeout_secs), child.wait()).await {
            Ok(status) => status?,
            Err(_) => {
                let _ = child.kill().await;
                let _ = stdout_task.await;
                let _ = stderr_task.await;
                return Err(anyhow!(
                    "phase '{}' command '{}' timed out after {} seconds",
                    context.phase_id,
                    command.program,
                    timeout_secs
                ));
            }
        }
    } else {
        child.wait().await?
    };

    let stdout_capture = stdout_task
        .await
        .map_err(|error| anyhow!("stdout capture task failed: {error}"))??;
    let stderr_capture = stderr_task
        .await
        .map_err(|error| anyhow!("stderr capture task failed: {error}"))??;

    let exit_code = status.code().unwrap_or(-1);
    let stdout = stdout_capture.text;
    let stderr = stderr_capture.text;
    let duration_ms = started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
    let phase_decision = stdout_capture
        .phase_decision
        .or(stderr_capture.phase_decision)
        .or_else(|| parse_phase_decision_from_text(&stdout, context.phase_id))
        .or_else(|| parse_phase_decision_from_text(&stderr, context.phase_id));

    if !command.success_exit_codes.contains(&exit_code) {
        let mut failure_summary = format!(
            "Command `{}` exited with code {} (expected one of {:?}).",
            command.program, exit_code, command.success_exit_codes
        );
        if !stdout.trim().is_empty() {
            failure_summary.push_str("\n\nStdout:\n");
            failure_summary.push_str(stdout.trim());
        }
        if !stderr.trim().is_empty() {
            failure_summary.push_str("\n\nStderr:\n");
            failure_summary.push_str(stderr.trim());
        }
        return Ok(CommandExecutionResult {
            exit_code,
            program: command.program.clone(),
            args,
            stdout,
            stderr,
            cwd,
            duration_ms,
            parsed_payload: None,
            phase_decision,
            failure_summary: Some(failure_summary),
        });
    }

    let parsed_payload = if command.parse_json_output {
        let payload = parse_command_json_output(&stdout)?;
        validate_command_contract(
            &payload,
            command.expected_result_kind.as_deref(),
            command.expected_schema.as_ref(),
        )?;
        Some(payload)
    } else {
        None
    };

    Ok(CommandExecutionResult {
        exit_code,
        program: command.program.clone(),
        args,
        stdout,
        stderr,
        cwd,
        duration_ms,
        parsed_payload,
        phase_decision,
        failure_summary: None,
    })
}

fn parse_command_json_output(stdout: &str) -> Result<Value> {
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return Err(anyhow!("command output is empty; expected JSON payload"));
    }
    if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
        return Ok(value);
    }
    let payloads = crate::ipc::collect_json_payload_lines(stdout);
    payloads
        .last()
        .map(|(_, payload)| payload.clone())
        .ok_or_else(|| anyhow!("unable to parse JSON payload from command output"))
}

fn validate_command_contract(
    payload: &Value,
    expected_kind: Option<&str>,
    expected_schema: Option<&Value>,
) -> Result<()> {
    if let Some(kind) = expected_kind.map(str::trim).filter(|v| !v.is_empty()) {
        let payload_kind = payload
            .get("kind")
            .and_then(Value::as_str)
            .map(str::trim)
            .ok_or_else(|| anyhow!("payload is missing required field 'kind'"))?;
        if !payload_kind.eq_ignore_ascii_case(kind) {
            return Err(anyhow!(
                "payload kind mismatch: expected '{}', got '{}'",
                kind,
                payload_kind
            ));
        }
    }
    if let Some(schema) = expected_schema {
        crate::phase_executor::validate_basic_json_schema(payload, schema)?;
    }
    Ok(())
}
