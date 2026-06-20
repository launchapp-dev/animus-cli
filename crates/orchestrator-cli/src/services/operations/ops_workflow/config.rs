use std::path::Path;

use anyhow::{Context, Result};
use serde_json::Value;
use std::path::PathBuf;

use super::project_state_dir;

pub(crate) fn workflow_config_path(project_root: &str) -> PathBuf {
    let single_file = Path::new(project_root).join(".animus").join("workflows.yaml");
    if single_file.exists() {
        single_file
    } else {
        orchestrator_core::yaml_workflows_dir(Path::new(project_root))
    }
}

pub(crate) fn agent_runtime_path(project_root: &str) -> PathBuf {
    let single_file = Path::new(project_root).join(".animus").join("workflows.yaml");
    if single_file.exists() {
        single_file
    } else {
        orchestrator_core::yaml_workflows_dir(Path::new(project_root))
    }
}

pub(super) fn manual_approvals_path(project_root: &str) -> PathBuf {
    project_state_dir(project_root).join("manual-phase-approvals.v1.json")
}

pub(crate) fn get_state_machine_payload(project_root: &str) -> Result<Value> {
    let loaded = orchestrator_core::load_state_machines_for_project(Path::new(project_root))?;
    Ok(serde_json::json!({
        "path": loaded.path.display().to_string(),
        "schema": loaded.compiled.metadata.schema,
        "version": loaded.compiled.metadata.version,
        "hash": loaded.compiled.metadata.hash,
        "source": loaded.compiled.metadata.source,
        "warnings": loaded.warnings,
        "state_machines": loaded.compiled.document,
    }))
}

pub(crate) fn validate_state_machine_payload(project_root: &str) -> Value {
    let path = orchestrator_core::state_machines_path(Path::new(project_root));
    if !path.exists() {
        return serde_json::json!({
            "path": path.display().to_string(),
            "valid": false,
            "errors": ["state machine metadata file is missing"],
            "warnings": [],
        });
    }

    let content = match std::fs::read_to_string(&path) {
        Ok(content) => content,
        Err(error) => {
            return serde_json::json!({
                "path": path.display().to_string(),
                "valid": false,
                "errors": [format!("failed to read metadata file: {error}")],
                "warnings": [],
            })
        }
    };

    let document = match serde_json::from_str::<orchestrator_core::StateMachinesDocument>(&content) {
        Ok(document) => document,
        Err(error) => {
            return serde_json::json!({
                "path": path.display().to_string(),
                "valid": false,
                "errors": [format!("invalid JSON: {error}")],
                "warnings": [],
            })
        }
    };

    match orchestrator_core::state_machines::compile_state_machines_document(
        document,
        orchestrator_core::MachineSource::Json,
    ) {
        Ok(compiled) => serde_json::json!({
            "path": path.display().to_string(),
            "valid": true,
            "errors": [],
            "warnings": [],
            "schema": compiled.metadata.schema,
            "version": compiled.metadata.version,
            "hash": compiled.metadata.hash,
            "source": compiled.metadata.source,
        }),
        Err(error) => serde_json::json!({
            "path": path.display().to_string(),
            "valid": false,
            "errors": [error.to_string()],
            "warnings": [],
        }),
    }
}

pub(crate) fn set_state_machine_payload(project_root: &str, input_json: &str) -> Result<Value> {
    let document: orchestrator_core::StateMachinesDocument =
        serde_json::from_str(input_json).with_context(|| {
            "invalid --input-json payload for workflow state-machine set; run 'animus workflow state-machine set --help' for schema"
        })?;
    let compiled = orchestrator_core::write_state_machines_document(Path::new(project_root), &document)?;
    let path = orchestrator_core::state_machines_path(Path::new(project_root));

    Ok(serde_json::json!({
        "path": path.display().to_string(),
        "schema": compiled.metadata.schema,
        "version": compiled.metadata.version,
        "hash": compiled.metadata.hash,
        "source": compiled.metadata.source,
        "state_machines": compiled.document,
    }))
}

pub(crate) fn get_agent_runtime_payload(project_root: &str) -> Value {
    let path = agent_runtime_path(project_root);
    match orchestrator_core::agent_runtime_config::load_agent_runtime_config_with_metadata(Path::new(project_root)) {
        Ok(loaded) => serde_json::json!({
            "path": path.display().to_string(),
            "source": loaded.metadata.source,
            "schema": loaded.metadata.schema,
            "version": loaded.metadata.version,
            "hash": loaded.metadata.hash,
            "warnings": [],
            "agent_runtime": loaded.config,
        }),
        Err(error) => serde_json::json!({
            "path": path.display().to_string(),
            "source": "error",
            "schema": orchestrator_core::agent_runtime_config::AGENT_RUNTIME_CONFIG_SCHEMA_ID,
            "version": orchestrator_core::agent_runtime_config::AGENT_RUNTIME_CONFIG_VERSION,
            "warnings": [error.to_string()],
            "agent_runtime": orchestrator_core::builtin_agent_runtime_config(),
        }),
    }
}

pub(crate) fn validate_agent_runtime_payload(project_root: &str) -> Value {
    let path = agent_runtime_path(project_root);
    match orchestrator_core::agent_runtime_config::load_agent_runtime_config_with_metadata(Path::new(project_root)) {
        Ok(loaded) => serde_json::json!({
            "path": path.display().to_string(),
            "valid": true,
            "errors": [],
            "warnings": [],
            "schema": loaded.metadata.schema,
            "version": loaded.metadata.version,
            "hash": loaded.metadata.hash,
            "source": loaded.metadata.source,
        }),
        Err(error) => serde_json::json!({
            "path": path.display().to_string(),
            "valid": false,
            "errors": [error.to_string()],
            "warnings": [],
        }),
    }
}

pub(crate) fn set_agent_runtime_payload(project_root: &str, input_json: &str) -> Result<Value> {
    let config: orchestrator_core::AgentRuntimeConfig =
        serde_json::from_str(input_json).with_context(|| {
            "invalid --input-json payload for workflow agent-runtime set; run 'animus workflow agent-runtime set --help' for schema"
        })?;
    orchestrator_core::write_agent_runtime_config(Path::new(project_root), &config)?;
    let path = agent_runtime_path(project_root);

    Ok(serde_json::json!({
        "path": path.display().to_string(),
        "schema": config.schema,
        "version": config.version,
        "hash": orchestrator_core::agent_runtime_config::agent_runtime_config_hash(&config),
        "agent_runtime": config,
    }))
}

pub(crate) fn get_workflow_config_payload(project_root: &str) -> Value {
    let path = workflow_config_path(project_root);
    match orchestrator_core::load_workflow_config_with_metadata(Path::new(project_root)) {
        Ok(loaded) => serde_json::json!({
            "path": path.display().to_string(),
            "source": loaded.metadata.source,
            "schema": loaded.metadata.schema,
            "version": loaded.metadata.version,
            "hash": loaded.metadata.hash,
            "workflow_config": loaded.config,
        }),
        Err(error) => serde_json::json!({
            "path": path.display().to_string(),
            "source": "error",
            "schema": orchestrator_core::WORKFLOW_CONFIG_SCHEMA_ID,
            "version": orchestrator_core::WORKFLOW_CONFIG_VERSION,
            "errors": [error.to_string()],
            "workflow_config": serde_json::Value::Null,
        }),
    }
}

/// Build a structured error object from an `anyhow::Error`, routing through
/// the same `YamlDiagnostic` formatter `compile` surfaces. When a
/// `YamlDiagnostic` is present anywhere in the error chain, its message,
/// file/line/col, stable code, and full rustc-style caret rendering are
/// carried through so `validate` errors[] are as rich as `compile`'s output.
/// Otherwise the full `{:#}` context chain is flattened into `message`.
fn config_error_to_value(error: &anyhow::Error) -> Value {
    for source in error.chain() {
        if let Some(diag) = source.downcast_ref::<orchestrator_core::YamlDiagnostic>() {
            let mut obj = serde_json::json!({
                "message": diag.message.clone(),
                "code": diag.code.clone(),
                "rendered": format!("{diag}"),
            });
            let map = obj.as_object_mut().expect("json object");
            if let Some(file) = &diag.file {
                map.insert("file".into(), Value::String(file.display().to_string()));
            }
            if let Some(line) = diag.line {
                map.insert("line".into(), Value::from(line));
            }
            if let Some(col) = diag.col {
                map.insert("col".into(), Value::from(col));
            }
            return obj;
        }
    }
    serde_json::json!({ "message": format!("{error:#}") })
}

pub(crate) fn validate_workflow_config_payload(project_root: &str) -> Value {
    let workflow_loaded = orchestrator_core::load_workflow_config_with_metadata(Path::new(project_root));
    let runtime_loaded =
        orchestrator_core::agent_runtime_config::load_agent_runtime_config_with_metadata(Path::new(project_root));
    let warnings = unenforced_warnings_payload(project_root);

    match (workflow_loaded, runtime_loaded) {
        (Ok(workflow), Ok(runtime)) => {
            match orchestrator_core::validate_workflow_and_runtime_configs_with_project_root(
                &workflow.config,
                &runtime.config,
                Some(Path::new(project_root)),
            ) {
                Ok(()) => serde_json::json!({
                    "valid": true,
                    "errors": [],
                    "warnings": warnings,
                    "summary": {
                        "workflows": workflow.config.workflows.len(),
                        "phases": workflow.config.phase_definitions.len(),
                        "agents": workflow.config.agent_profiles.len(),
                        "errors": 0,
                        "warnings": warnings.len(),
                    },
                    "workflow_config_path": workflow.path.display().to_string(),
                    "agent_runtime_path": runtime.path.display().to_string(),
                    "workflow_config_hash": workflow.metadata.hash,
                    "agent_runtime_hash": runtime.metadata.hash,
                }),
                Err(error) => serde_json::json!({
                    "valid": false,
                    "errors": [config_error_to_value(&error)],
                    "warnings": warnings,
                    "workflow_config_path": workflow.path.display().to_string(),
                    "agent_runtime_path": runtime.path.display().to_string(),
                }),
            }
        }
        (Err(workflow_error), Err(runtime_error)) => serde_json::json!({
            "valid": false,
            "errors": [config_error_to_value(&workflow_error), config_error_to_value(&runtime_error)],
            "warnings": warnings,
        }),
        (Err(workflow_error), _) => serde_json::json!({
            "valid": false,
            "errors": [config_error_to_value(&workflow_error)],
            "warnings": warnings,
        }),
        (_, Err(runtime_error)) => serde_json::json!({
            "valid": false,
            "errors": [config_error_to_value(&runtime_error)],
            "warnings": warnings,
        }),
    }
}

/// Render the `validate` payload for human (non-`--json`) consumers. On
/// success prints a one-line summary; on failure prints each structured
/// error using the same rustc-style caret rendering `compile` shows, then
/// any warnings. Mirrors `compile`'s human ergonomics so the two verbs no
/// longer diverge.
pub(crate) fn render_validate_human(payload: &Value) -> String {
    let mut out = String::new();
    let warnings = payload.get("warnings").and_then(Value::as_array);
    let warning_count = warnings.map(Vec::len).unwrap_or(0);

    if payload.get("valid").and_then(Value::as_bool).unwrap_or(false) {
        let summary = payload.get("summary");
        let workflows = summary.and_then(|s| s.get("workflows")).and_then(Value::as_u64).unwrap_or(0);
        let phases = summary.and_then(|s| s.get("phases")).and_then(Value::as_u64).unwrap_or(0);
        let agents = summary.and_then(|s| s.get("agents")).and_then(Value::as_u64).unwrap_or(0);
        out.push_str(&format!(
            "valid: {} workflow{}, {} phase{}, {} agent{} — 0 errors, {} warning{}\n",
            workflows,
            plural(workflows),
            phases,
            plural(phases),
            agents,
            plural(agents),
            warning_count,
            plural(warning_count as u64),
        ));
    } else {
        if let Some(errors) = payload.get("errors").and_then(Value::as_array) {
            for err in errors {
                if let Some(rendered) = err.get("rendered").and_then(Value::as_str) {
                    out.push_str(rendered);
                    if !rendered.ends_with('\n') {
                        out.push('\n');
                    }
                } else if let Some(message) = err.get("message").and_then(Value::as_str) {
                    out.push_str(&format!("error: {message}\n"));
                }
            }
        }
        let error_count = payload.get("errors").and_then(Value::as_array).map(Vec::len).unwrap_or(0);
        out.push_str(&format!(
            "invalid: {} error{}, {} warning{}\n",
            error_count,
            plural(error_count as u64),
            warning_count,
            plural(warning_count as u64),
        ));
    }

    for warning in warnings.into_iter().flatten() {
        if let Some(message) = warning.get("message").and_then(Value::as_str) {
            out.push_str(&format!("warning: {message}\n"));
        }
    }

    out
}

fn plural(n: u64) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}

/// Structured `warnings` array for validate/compile payloads: one entry per
/// declared-but-unenforced YAML field, with the source file, dotted field
/// path, and a one-line explanation of where the real knob lives. Warnings
/// never affect `valid` / `compiled` — existing configs keep compiling.
fn unenforced_warnings_payload(project_root: &str) -> Vec<Value> {
    let mut warnings: Vec<Value> = orchestrator_core::unenforced_project_yaml_warnings(Path::new(project_root))
        .into_iter()
        .map(|warning| {
            serde_json::json!({
                "field": warning.field,
                "source": warning.source,
                "message": warning.to_string(),
            })
        })
        .collect();
    // Explicit `skills:` declarations that do not resolve against the
    // project's skill sources (typo'd or not-yet-installed names). Same
    // never-fails-validation posture as the unenforced-field warnings.
    warnings.extend(
        orchestrator_core::missing_project_skill_reference_warnings(Path::new(project_root)).into_iter().map(
            |warning| {
                serde_json::json!({
                    "field": warning.field,
                    "source": warning.source,
                    "skill": warning.skill,
                    "message": warning.to_string(),
                })
            },
        ),
    );
    warnings
}

/// CLI surface for `animus workflow config reload`. Runs the same
/// compile pipeline the daemon's hot-reload watcher uses and prints the
/// diagnostic envelope.
///
/// When a daemon is detected in a separate process (via the project's
/// `daemon.pid` file), each candidate YAML source file's mtime is also
/// touched so the daemon's own filesystem watcher observes a Modify event
/// and reruns its in-process reload. The CLI compile is still authoritative
/// for the diagnostic envelope the user sees; the touch is fire-and-forget.
pub(crate) fn reload_workflow_config_payload(project_root: &str) -> Value {
    let snapshot = orchestrator_daemon_runtime::config::workflow_config_snapshot();
    let outcome =
        orchestrator_daemon_runtime::config::reload_workflow_config_once(Path::new(project_root), &snapshot, None);

    // Best-effort nudge a separate-process daemon to re-pick-up the same
    // edit via its own watcher. We avoid building a dedicated control-RPC
    // verb so the path stays a single chokepoint (the daemon's existing
    // watcher) and so a CLI invocation without a running daemon is still a
    // no-op-safe operation.
    if orchestrator_daemon_runtime::DaemonRuntimeState::read_daemon_pid_file(project_root).is_some() {
        touch_workflow_yaml_for_watcher(Path::new(project_root));
    }

    outcome.to_json()
}

/// Best-effort touch of every workflow YAML source file under `.animus/`
/// so the daemon's filesystem watcher observes a Modify event. Uses
/// `std::fs::File::set_modified` (stable since Rust 1.75) so no extra
/// crate is required. Failures are swallowed — the CLI compile pipeline
/// is the authoritative diagnostic, and the daemon's own watcher recovers
/// on the next real edit if this nudge is missed.
fn touch_workflow_yaml_for_watcher(project_root: &Path) {
    let now = std::time::SystemTime::now();
    let mut touched_any_file = false;

    let single = project_root.join(".animus").join("workflows.yaml");
    if single.exists() {
        if let Ok(file) = std::fs::OpenOptions::new().write(true).open(&single) {
            if file.set_modified(now).is_ok() {
                touched_any_file = true;
            }
        }
    }
    let workflows_dir = project_root.join(".animus").join("workflows");
    if workflows_dir.is_dir() {
        if let Ok(entries) = std::fs::read_dir(&workflows_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                let is_yaml = path
                    .extension()
                    .and_then(|ext| ext.to_str())
                    .map(|ext| ext.eq_ignore_ascii_case("yaml") || ext.eq_ignore_ascii_case("yml"))
                    .unwrap_or(false);
                if is_yaml {
                    if let Ok(file) = std::fs::OpenOptions::new().write(true).open(&path) {
                        if file.set_modified(now).is_ok() {
                            touched_any_file = true;
                        }
                    }
                }
            }
        }
    }

    // If every YAML overlay was removed before the manual reload ran, there
    // is nothing to set_modified on — but the daemon's watcher still needs
    // a kick to notice the removed-final-file state and clear its snapshot.
    // Drop a sentinel marker file (which itself looks like a non-YAML write
    // inside the watched .animus dir and therefore does not retrigger the
    // reload path) immediately followed by removal. The Create / Remove
    // events alone are enough to wake the watcher's select loop; the path
    // filter then sees `.animus` is now empty of YAML and clears.
    if !touched_any_file {
        let animus_dir = project_root.join(".animus");
        if animus_dir.is_dir() {
            let sentinel = animus_dir.join(".reload-nudge");
            if std::fs::write(&sentinel, b"").is_ok() {
                let _ = std::fs::remove_file(&sentinel);
            }
        }
    }
}

pub(crate) fn compile_yaml_workflows_payload(project_root: &str) -> Result<Value> {
    match orchestrator_core::validate_and_compile_yaml_workflows(Path::new(project_root))? {
        Some(result) => {
            let source_files: Vec<String> = result.source_files.iter().map(|p| p.display().to_string()).collect();
            Ok(serde_json::json!({
                "compiled": true,
                "source_files": source_files,
                "output_path": result.output_path.display().to_string(),
                "workflows": result.config.workflows.iter().map(|p| &p.id).collect::<Vec<_>>(),
                "phase_definitions": result.config.phase_definitions.len(),
                "agent_profiles": result.config.agent_profiles.len(),
                "hash": orchestrator_core::workflow_config_hash(&result.config),
                "warnings": unenforced_warnings_payload(project_root),
            }))
        }
        None => Ok(serde_json::json!({
            "compiled": false,
            "message": "no YAML workflow files found in .animus/workflows/ or .animus/workflows.yaml",
        })),
    }
}

pub(super) fn title_case_phase_id(phase_id: &str) -> String {
    phase_id
        .split(['-', '_'])
        .filter(|part| !part.trim().is_empty())
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => {
                    let mut label = first.to_ascii_uppercase().to_string();
                    label.push_str(chars.as_str());
                    label
                }
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn write_minimal_overlay(dir: &Path) {
        let animus = dir.join(".animus");
        fs::create_dir_all(animus.join("workflows")).unwrap();
        let yaml = "phases:\n  alpha:\n    mode: agent\n    agent_id: hot-reload-agent\nagents:\n  hot-reload-agent:\n    description: hot-reload fixture\n    system_prompt: hot-reload prompt\n    skills: []\nworkflows:\n  - id: hot-reload-workflow\n    name: Hot Reload\n    phases:\n      - alpha\n";
        fs::write(animus.join("workflows.yaml"), yaml).unwrap();
    }

    #[test]
    fn reload_payload_reports_reloaded_true_for_valid_overlay() {
        let dir = tempdir().unwrap();
        write_minimal_overlay(dir.path());
        let _config_source_seam =
            orchestrator_config::workflow_config::config_source_client::install_yaml_config_source_base(dir.path());
        let project_root = dir.path().to_string_lossy().to_string();
        let value = reload_workflow_config_payload(&project_root);
        assert_eq!(value["reloaded"], serde_json::json!(true), "valid overlay must reload");
        let phases = value["phase_definitions"].as_u64().expect("phase_definitions present");
        assert!(phases >= 1, "expected at least one phase");
    }

    #[test]
    fn validate_payload_surfaces_unenforced_field_warnings() {
        let dir = tempdir().unwrap();
        write_minimal_overlay(dir.path());
        let yaml_path = dir.path().join(".animus").join("workflows.yaml");
        let mut yaml = fs::read_to_string(&yaml_path).unwrap();
        yaml.push_str("daemon:\n  pool_size: 4\n");
        fs::write(&yaml_path, yaml).unwrap();

        let _config_source_seam =
            orchestrator_config::workflow_config::config_source_client::install_yaml_config_source_base(dir.path());
        let project_root = dir.path().to_string_lossy().to_string();
        let value = validate_workflow_config_payload(&project_root);
        assert_eq!(value["valid"], serde_json::json!(true), "warnings must never fail validation: {value}");
        let warnings = value["warnings"].as_array().expect("warnings array");
        assert!(
            warnings.iter().any(|w| w["field"] == "daemon.pool_size"),
            "expected daemon.pool_size warning, got {warnings:?}"
        );
        let compile = compile_yaml_workflows_payload(&project_root).expect("compile payload");
        assert_eq!(compile["compiled"], serde_json::json!(true));
        assert!(
            compile["warnings"].as_array().is_some_and(|w| !w.is_empty()),
            "compile payload must carry warnings: {compile}"
        );
    }

    #[test]
    fn reload_payload_reports_reloaded_false_for_malformed_overlay() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join(".animus")).unwrap();
        fs::write(dir.path().join(".animus").join("workflows.yaml"), ": not valid\n[]\n").unwrap();
        let project_root = dir.path().to_string_lossy().to_string();
        let value = reload_workflow_config_payload(&project_root);
        assert_eq!(value["reloaded"], serde_json::json!(false), "malformed overlay must NOT reload");
        let errors = value["errors"].as_array().expect("errors array");
        assert!(!errors.is_empty(), "diagnostic must be surfaced");
    }

    #[test]
    fn validate_payload_carries_summary_on_success() {
        let dir = tempdir().unwrap();
        write_minimal_overlay(dir.path());
        let _config_source_seam =
            orchestrator_config::workflow_config::config_source_client::install_yaml_config_source_base(dir.path());
        let project_root = dir.path().to_string_lossy().to_string();
        let value = validate_workflow_config_payload(&project_root);
        assert_eq!(value["valid"], serde_json::json!(true), "minimal overlay must validate: {value}");
        let summary = value.get("summary").expect("summary present on success");
        assert!(summary["workflows"].as_u64().is_some(), "summary must count workflows: {summary}");
        assert!(summary["phases"].as_u64().is_some(), "summary must count phases: {summary}");
        assert!(summary["agents"].as_u64().is_some(), "summary must count agents: {summary}");

        let human = render_validate_human(&value);
        assert!(human.starts_with("valid: "), "human success line must start with 'valid: ': {human}");
        assert!(human.contains("0 errors"), "human success line must report 0 errors: {human}");
    }

    #[test]
    fn validate_payload_rejects_removed_post_success_merge() {
        let dir = tempdir().unwrap();
        let animus = dir.path().join(".animus");
        fs::create_dir_all(&animus).unwrap();
        let yaml = "phases:\n  alpha:\n    mode: agent\n    agent_id: a\nagents:\n  a:\n    description: d\n    system_prompt: p\n    skills: []\nworkflows:\n  - id: ship\n    name: Ship\n    phases:\n      - alpha\n    post_success:\n      merge:\n        strategy: squash\n        target_branch: main\n";
        fs::write(animus.join("workflows.yaml"), yaml).unwrap();
        let project_root = dir.path().to_string_lossy().to_string();

        // v0.6: the kernel sources its base config from the config_source
        // plugin, which compiles the project YAML. The `post_success.merge`
        // removal is therefore surfaced at compile time (the same boundary the
        // `animus-config-yaml` plugin hits) rather than from the kernel's
        // validate pass. Assert the rejection error carries the removal note.
        let error = match orchestrator_core::compile_yaml_workflow_files(Path::new(&project_root)) {
            Err(error) => error,
            Ok(_) => panic!("post_success.merge must be rejected at compile time"),
        };
        let message = format!("{error:#}");
        assert!(message.contains("`post_success.merge` was removed"), "error must mention the removal: {message}");
    }
}
