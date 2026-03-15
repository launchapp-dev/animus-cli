use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};

mod engine;
pub mod schema;
mod validator;

pub use engine::{
    compile_state_machines_document, evaluate_guard, CompiledRequirementLifecycleMachine, CompiledStateMachines,
    CompiledWorkflowMachine, GuardContext, MachineMetadata, MachineSource, RequirementTransitionOutcome,
    TransitionError, WorkflowTransitionOutcome,
};
pub use schema::{
    builtin_state_machines_document, RequirementLifecycleEvent, RequirementLifecyclePolicy, StateMachinesDocument,
};
pub use validator::validate_state_machines_document;

pub const STATE_MACHINES_FILE_NAME: &str = "state-machines.v1.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StateMachineMode {
    Builtin,
    Json,
    JsonStrict,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateMachineLoadWarning {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone)]
pub struct LoadedStateMachines {
    pub compiled: CompiledStateMachines,
    pub warnings: Vec<StateMachineLoadWarning>,
    pub path: PathBuf,
}

pub fn state_machines_path(project_root: &Path) -> PathBuf {
    let base = protocol::scoped_state_root(project_root).unwrap_or_else(|| project_root.join(".ao"));
    base.join("state").join(STATE_MACHINES_FILE_NAME)
}

pub fn ensure_state_machines_file(project_root: &Path) -> Result<()> {
    let path = state_machines_path(project_root);
    if path.exists() {
        return Ok(());
    }

    let document = builtin_state_machines_document();
    write_document_at_path(&path, &document)
}

pub fn builtin_compiled_state_machines() -> CompiledStateMachines {
    compile_state_machines_document(builtin_state_machines_document(), MachineSource::Builtin)
        .expect("builtin state-machine config must always compile")
}

pub fn write_state_machines_document(
    project_root: &Path,
    document: &StateMachinesDocument,
) -> Result<CompiledStateMachines> {
    let compiled = compile_state_machines_document(document.clone(), MachineSource::Json)?;
    let path = state_machines_path(project_root);
    write_document_at_path(&path, document)?;
    Ok(compiled)
}

pub fn load_state_machines_for_project(project_root: &Path) -> Result<LoadedStateMachines> {
    load_state_machines_for_project_with_mode(project_root, StateMachineMode::Json)
}

pub fn load_state_machines_for_project_with_mode(
    project_root: &Path,
    mode: StateMachineMode,
) -> Result<LoadedStateMachines> {
    let path = state_machines_path(project_root);

    match mode {
        StateMachineMode::Builtin => Ok(LoadedStateMachines {
            compiled: compile_state_machines_document(builtin_state_machines_document(), MachineSource::Builtin)?,
            warnings: Vec::new(),
            path,
        }),
        StateMachineMode::Json | StateMachineMode::JsonStrict => load_json_mode_state_machines(project_root, mode),
    }
}

fn load_json_mode_state_machines(project_root: &Path, mode: StateMachineMode) -> Result<LoadedStateMachines> {
    let path = state_machines_path(project_root);

    if !path.exists() {
        return match mode {
            StateMachineMode::JsonStrict => Err(anyhow!("state machine metadata file not found: {}", path.display())),
            _ => Ok(fallback_loaded(
                path,
                "state_machine_file_missing",
                "state machine metadata file is missing; using builtin fallback",
            )?),
        };
    }

    let content = fs::read_to_string(&path)
        .with_context(|| format!("failed to read state machine metadata at {}", path.display()));

    let content = match content {
        Ok(content) => content,
        Err(error) => {
            return match mode {
                StateMachineMode::JsonStrict => Err(error),
                _ => Ok(fallback_loaded(
                    path,
                    "state_machine_file_unreadable",
                    &format!("failed to read state machine metadata; using builtin fallback: {error}"),
                )?),
            }
        }
    };

    let document = serde_json::from_str::<StateMachinesDocument>(&content);
    let document = match document {
        Ok(document) => document,
        Err(error) => {
            return match mode {
                StateMachineMode::JsonStrict => {
                    Err(anyhow!("invalid state machine metadata JSON at {}: {error}", path.display()))
                }
                _ => Ok(fallback_loaded(
                    path,
                    "state_machine_json_invalid",
                    &format!("invalid state machine metadata JSON; using builtin fallback: {error}"),
                )?),
            }
        }
    };

    let compiled = compile_state_machines_document(document, MachineSource::Json);
    match compiled {
        Ok(compiled) => Ok(LoadedStateMachines { compiled, warnings: Vec::new(), path }),
        Err(error) => match mode {
            StateMachineMode::JsonStrict => Err(error),
            _ => Ok(fallback_loaded(
                path,
                "state_machine_validation_failed",
                &format!("state machine validation failed; using builtin fallback: {error}"),
            )?),
        },
    }
}

fn fallback_loaded(path: PathBuf, code: &str, message: &str) -> Result<LoadedStateMachines> {
    Ok(LoadedStateMachines {
        compiled: compile_state_machines_document(builtin_state_machines_document(), MachineSource::BuiltinFallback)?,
        warnings: vec![StateMachineLoadWarning { code: code.to_string(), message: message.to_string() }],
        path,
    })
}

fn write_document_at_path(path: &Path, document: &StateMachinesDocument) -> Result<()> {
    crate::domain_state::write_json_pretty(path, document)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_falls_back_in_json_mode() {
        let temp = tempfile::tempdir().expect("tempdir");
        let loaded = load_state_machines_for_project_with_mode(temp.path(), StateMachineMode::Json)
            .expect("load should succeed with fallback");

        assert_eq!(loaded.compiled.metadata.source, MachineSource::BuiltinFallback);
        assert_eq!(loaded.warnings.len(), 1);
    }

    #[test]
    fn invalid_file_falls_back_in_json_mode() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = state_machines_path(temp.path());
        fs::create_dir_all(path.parent().expect("parent")).expect("create dir");
        fs::write(&path, "{ invalid json").expect("write invalid");

        let loaded = load_state_machines_for_project_with_mode(temp.path(), StateMachineMode::Json)
            .expect("load should succeed with fallback");

        assert_eq!(loaded.compiled.metadata.source, MachineSource::BuiltinFallback);
        assert_eq!(loaded.warnings.len(), 1);
    }

    #[test]
    fn strict_mode_errors_when_file_is_invalid() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = state_machines_path(temp.path());
        fs::create_dir_all(path.parent().expect("parent")).expect("create dir");
        fs::write(&path, "{ invalid json").expect("write invalid");

        let error = load_state_machines_for_project_with_mode(temp.path(), StateMachineMode::JsonStrict)
            .expect_err("strict mode should fail");

        assert!(error.to_string().contains("invalid state machine metadata JSON"));
    }

    #[test]
    fn write_state_machines_document_is_atomic_and_readable() {
        let temp = tempfile::tempdir().expect("tempdir");
        write_state_machines_document(temp.path(), &builtin_state_machines_document()).expect("write should succeed");

        let loaded = load_state_machines_for_project_with_mode(temp.path(), StateMachineMode::Json)
            .expect("load should succeed");
        assert_eq!(loaded.compiled.metadata.source, MachineSource::Json);
    }
}
