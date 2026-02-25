use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DoctorCheck {
    pub name: String,
    pub ok: bool,
    pub details: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DoctorCheckResult {
    Healthy,
    Degraded,
    Unhealthy,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DoctorReport {
    pub result: DoctorCheckResult,
    pub checks: Vec<DoctorCheck>,
}

impl DoctorReport {
    pub fn run() -> Self {
        let current_dir = std::env::current_dir().ok();
        let env_project_root = std::env::var("PROJECT_ROOT").ok().map(PathBuf::from);
        let project_root = current_dir.as_deref().or_else(|| env_project_root.as_deref());

        let mut checks = vec![
            DoctorCheck {
                name: "cwd_resolvable".to_string(),
                ok: current_dir.is_some(),
                details: "current working directory available".to_string(),
            },
            DoctorCheck {
                name: "project_root_env".to_string(),
                ok: std::env::var("PROJECT_ROOT").is_ok(),
                details: "PROJECT_ROOT is set (optional)".to_string(),
            },
        ];
        checks.extend(policy_checks(project_root));

        let hard_fail = checks.iter().any(|check| {
            !check.ok
                && matches!(
                    check.name.as_str(),
                    "policy_schema_valid"
                        | "policy_phase_bindings_valid"
                        | "policy_elevation_store_writable"
                )
        });
        let soft_fail = checks.iter().any(|check| !check.ok);
        let result = if hard_fail {
            DoctorCheckResult::Unhealthy
        } else if soft_fail {
            DoctorCheckResult::Degraded
        } else {
            DoctorCheckResult::Healthy
        };

        Self { result, checks }
    }
}

fn policy_checks(project_root: Option<&Path>) -> Vec<DoctorCheck> {
    let Some(project_root) = project_root else {
        return vec![
            DoctorCheck {
                name: "policy_config_loadable".to_string(),
                ok: false,
                details: "project root unavailable for policy checks".to_string(),
            },
            DoctorCheck {
                name: "policy_schema_valid".to_string(),
                ok: false,
                details: "project root unavailable for policy checks".to_string(),
            },
            DoctorCheck {
                name: "policy_phase_bindings_valid".to_string(),
                ok: false,
                details: "project root unavailable for policy checks".to_string(),
            },
            DoctorCheck {
                name: "policy_elevation_store_writable".to_string(),
                ok: false,
                details: "project root unavailable for policy checks".to_string(),
            },
            DoctorCheck {
                name: "policy_runtime_defaults_resolvable".to_string(),
                ok: true,
                details: "global runtime defaults resolve without project overrides".to_string(),
            },
        ];
    };

    let loaded_runtime = crate::agent_runtime_config::load_agent_runtime_config_with_metadata(project_root);
    let (policy_config_loadable, config_details) = match &loaded_runtime {
        Ok(loaded) => (
            true,
            format!(
                "loaded {} from {}",
                loaded.metadata.schema,
                loaded.path.display()
            ),
        ),
        Err(error) => (false, error.to_string()),
    };

    let policy_schema_valid = loaded_runtime
        .as_ref()
        .map(|loaded| {
            loaded.metadata.schema == crate::agent_runtime_config::AGENT_RUNTIME_CONFIG_SCHEMA_ID
                && loaded.metadata.version
                    == crate::agent_runtime_config::AGENT_RUNTIME_CONFIG_VERSION
        })
        .unwrap_or(false);

    let bindings = match (
        crate::load_workflow_config_with_metadata(project_root),
        loaded_runtime.as_ref(),
    ) {
        (Ok(workflow), Ok(runtime)) => {
            crate::validate_workflow_and_runtime_configs(&workflow.config, &runtime.config)
                .map(|_| "workflow/runtime phase bindings validated".to_string())
                .map_err(|error| error.to_string())
        }
        (Err(error), _) => Err(error.to_string()),
        (_, Err(error)) => Err(error.to_string()),
    };

    let elevation_store_path = project_root
        .join(".ao")
        .join("state")
        .join("elevation-audit.v1.json");
    let elevation_store_writable = is_store_writable(&elevation_store_path)
        .map(|_| "elevation audit store writable".to_string())
        .map_err(|error| error.to_string());

    let defaults = match loaded_runtime.as_ref() {
        Ok(loaded) => {
            let resolved = crate::resolve_execution_policy(
                None,
                loaded.config.phase_execution_policy_override("default"),
                loaded.config.phase_agent_execution_policy_override("default"),
            );
            if resolved.policy.sandbox_mode == crate::SandboxMode::WorkspaceWrite {
                Ok("resolved default sandbox_mode=workspace_write".to_string())
            } else {
                Err(format!(
                    "unexpected default sandbox_mode={}",
                    resolved.policy.sandbox_mode.as_str()
                ))
            }
        }
        Err(_) => {
            let resolved = crate::resolve_execution_policy(None, None, None);
            if resolved.policy.sandbox_mode == crate::SandboxMode::WorkspaceWrite {
                Ok("resolved global fallback sandbox_mode=workspace_write".to_string())
            } else {
                Err("global fallback defaults unresolved".to_string())
            }
        }
    };

    vec![
        DoctorCheck {
            name: "policy_config_loadable".to_string(),
            ok: policy_config_loadable,
            details: config_details,
        },
        DoctorCheck {
            name: "policy_schema_valid".to_string(),
            ok: policy_schema_valid,
            details: if policy_schema_valid {
                "policy schema/version validated".to_string()
            } else {
                "policy schema/version invalid or config unavailable".to_string()
            },
        },
        DoctorCheck {
            name: "policy_phase_bindings_valid".to_string(),
            ok: bindings.is_ok(),
            details: bindings
                .unwrap_or_else(|error| format!("phase bindings validation failed: {error}")),
        },
        DoctorCheck {
            name: "policy_elevation_store_writable".to_string(),
            ok: elevation_store_writable.is_ok(),
            details: elevation_store_writable
                .unwrap_or_else(|error| format!("elevation store not writable: {error}")),
        },
        DoctorCheck {
            name: "policy_runtime_defaults_resolvable".to_string(),
            ok: defaults.is_ok(),
            details: defaults
                .unwrap_or_else(|error| format!("runtime defaults unresolved: {error}")),
        },
    ]
}

fn is_store_writable(path: &Path) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let probe = path.with_file_name(format!(
        "{}.doctor-write-probe.{}",
        path.file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("elevation-audit"),
        std::process::id()
    ));
    std::fs::write(&probe, b"{}")?;
    std::fs::remove_file(probe)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, OnceLock};

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    struct CwdGuard {
        previous: PathBuf,
    }

    impl CwdGuard {
        fn enter(path: &Path) -> Self {
            let previous = std::env::current_dir().expect("current dir should resolve");
            std::env::set_current_dir(path).expect("set current dir should work");
            Self { previous }
        }
    }

    impl Drop for CwdGuard {
        fn drop(&mut self) {
            let _ = std::env::set_current_dir(&self.previous);
        }
    }

    struct EnvGuard {
        key: &'static str,
        previous: Option<String>,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: Option<&str>) -> Self {
            let previous = std::env::var(key).ok();
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
            Self { key, previous }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            if let Some(previous) = &self.previous {
                std::env::set_var(self.key, previous);
            } else {
                std::env::remove_var(self.key);
            }
        }
    }

    #[test]
    fn doctor_report_includes_policy_checks() {
        let _lock = env_lock().lock().expect("env lock");
        let temp = tempfile::tempdir().expect("tempdir");
        let _cwd = CwdGuard::enter(temp.path());
        let _project_root = EnvGuard::set("PROJECT_ROOT", temp.path().to_str());
        crate::ensure_workflow_config_file(temp.path()).expect("workflow config");
        crate::ensure_agent_runtime_config_file(temp.path()).expect("runtime config");

        let report = DoctorReport::run();
        let names = report
            .checks
            .iter()
            .map(|check| check.name.as_str())
            .collect::<Vec<_>>();
        assert!(names.contains(&"policy_config_loadable"));
        assert!(names.contains(&"policy_schema_valid"));
        assert!(names.contains(&"policy_phase_bindings_valid"));
        assert!(names.contains(&"policy_elevation_store_writable"));
        assert!(names.contains(&"policy_runtime_defaults_resolvable"));
        assert!(!matches!(report.result, DoctorCheckResult::Unhealthy));
    }

    #[test]
    fn doctor_marks_invalid_policy_schema_unhealthy() {
        let _lock = env_lock().lock().expect("env lock");
        let temp = tempfile::tempdir().expect("tempdir");
        let _cwd = CwdGuard::enter(temp.path());
        let _project_root = EnvGuard::set("PROJECT_ROOT", temp.path().to_str());
        crate::ensure_workflow_config_file(temp.path()).expect("workflow config");
        let state_dir = temp.path().join(".ao").join("state");
        std::fs::create_dir_all(&state_dir).expect("state dir");
        std::fs::write(
            state_dir.join("agent-runtime-config.v2.json"),
            serde_json::json!({
                "schema": "ao.agent-runtime-config.invalid",
                "version": 2,
                "tools_allowlist": ["cargo"],
                "agents": {
                    "default": {
                        "description": "default",
                        "system_prompt": "default prompt",
                        "tool": null,
                        "model": null,
                        "fallback_models": [],
                        "reasoning_effort": null,
                        "web_search": null,
                        "timeout_secs": null,
                        "max_attempts": null,
                        "execution_policy": null
                    }
                },
                "phases": {
                    "default": {
                        "mode": "agent",
                        "agent_id": "default",
                        "directive": "default",
                        "runtime": null,
                        "output_contract": null,
                        "output_json_schema": null,
                        "command": null,
                        "manual": null
                    }
                }
            })
            .to_string(),
        )
        .expect("runtime config should be written");

        let report = DoctorReport::run();
        assert!(matches!(report.result, DoctorCheckResult::Unhealthy));
        let schema_check = report
            .checks
            .iter()
            .find(|check| check.name == "policy_schema_valid")
            .expect("schema check exists");
        assert!(!schema_check.ok);
    }
}
