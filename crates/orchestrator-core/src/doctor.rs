use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::daemon_config::daemon_project_config_path;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DoctorCheckStatus {
    Ok,
    Warn,
    Fail,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DoctorRemediation {
    pub id: String,
    pub available: bool,
    pub details: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DoctorCheck {
    pub id: String,
    pub status: DoctorCheckStatus,
    pub details: String,
    pub remediation: DoctorRemediation,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
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
        let project_root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        Self::run_for_project(&project_root)
    }

    pub fn run_for_project(project_root: &Path) -> Self {
        let mut checks = Vec::new();

        let cwd_ok = std::env::current_dir().is_ok();
        checks.push(build_check(
            "cwd_resolvable",
            if cwd_ok {
                DoctorCheckStatus::Ok
            } else {
                DoctorCheckStatus::Fail
            },
            if cwd_ok {
                "current working directory is available".to_string()
            } else {
                "failed to resolve current working directory".to_string()
            },
            "set_valid_working_directory",
            false,
            "set a valid working directory before running AO commands",
            None,
        ));

        let project_root_exists = project_root.exists();
        let project_root_is_dir = project_root.is_dir();
        checks.push(build_check(
            "project_root_exists",
            if project_root_is_dir {
                DoctorCheckStatus::Ok
            } else {
                DoctorCheckStatus::Fail
            },
            if project_root_is_dir {
                format!("project root exists at {}", project_root.display())
            } else if project_root_exists {
                format!(
                    "project root is not a directory at {}",
                    project_root.display()
                )
            } else {
                format!("project root does not exist at {}", project_root.display())
            },
            "provide_valid_project_root",
            false,
            "rerun with --project-root pointing to an existing directory",
            None,
        ));

        let ao_dir = project_root.join(".ao");
        let ao_dir_state = directory_state(&ao_dir);
        checks.push(build_check(
            "ao_directory_present",
            match ao_dir_state {
                DirectoryState::Directory => DoctorCheckStatus::Ok,
                DirectoryState::Missing => DoctorCheckStatus::Warn,
                DirectoryState::NotDirectory => DoctorCheckStatus::Fail,
            },
            match ao_dir_state {
                DirectoryState::Directory => {
                    format!("AO state directory exists at {}", ao_dir.display())
                }
                DirectoryState::Missing => {
                    format!("AO state directory missing at {}", ao_dir.display())
                }
                DirectoryState::NotDirectory => {
                    format!("AO state path is not a directory at {}", ao_dir.display())
                }
            },
            match ao_dir_state {
                DirectoryState::NotDirectory => "manual_ao_directory_repair",
                _ => "bootstrap_project_state",
            },
            !matches!(ao_dir_state, DirectoryState::NotDirectory),
            match ao_dir_state {
                DirectoryState::NotDirectory => {
                    "replace non-directory .ao path with an AO state directory"
                }
                _ => "create baseline AO state/config files",
            },
            match ao_dir_state {
                DirectoryState::NotDirectory => None,
                _ => Some("ao doctor --fix"),
            },
        ));

        let core_state_path = ao_dir.join("core-state.json");
        checks.push(build_ao_file_check(
            "core_state_present",
            &core_state_path,
            ao_dir_state,
        ));

        let config_path = ao_dir.join("config.json");
        checks.push(build_ao_file_check(
            "config_json_present",
            &config_path,
            ao_dir_state,
        ));

        let resume_config_path = ao_dir.join("resume-config.json");
        checks.push(build_ao_file_check(
            "resume_config_present",
            &resume_config_path,
            ao_dir_state,
        ));

        let daemon_config_path = daemon_project_config_path(project_root);
        let daemon_check = if ao_dir_state == DirectoryState::NotDirectory {
            build_check(
                "daemon_config_valid_json",
                DoctorCheckStatus::Fail,
                format!(
                    "AO state path is not a directory at {}; cannot read daemon config",
                    ao_dir.display()
                ),
                "manual_ao_directory_repair",
                false,
                "replace non-directory .ao path with an AO state directory",
                None,
            )
        } else if !daemon_config_path.exists() {
            build_check(
                "daemon_config_valid_json",
                DoctorCheckStatus::Warn,
                format!(
                    "daemon config not found at {}; defaults will be used",
                    daemon_config_path.display()
                ),
                "create_default_daemon_config",
                true,
                "create daemon config with default values",
                Some("ao doctor --fix"),
            )
        } else {
            match std::fs::read_to_string(&daemon_config_path) {
                Ok(content) => match serde_json::from_str::<serde_json::Value>(&content) {
                    Ok(value) if value.is_object() => build_check(
                        "daemon_config_valid_json",
                        DoctorCheckStatus::Ok,
                        format!("daemon config is valid at {}", daemon_config_path.display()),
                        "no_action",
                        false,
                        "no action required",
                        None,
                    ),
                    Ok(_) => build_check(
                        "daemon_config_valid_json",
                        DoctorCheckStatus::Fail,
                        format!(
                            "daemon config at {} must be a JSON object",
                            daemon_config_path.display()
                        ),
                        "manual_pm_config_repair",
                        false,
                        "repair malformed daemon config JSON",
                        None,
                    ),
                    Err(error) => build_check(
                        "daemon_config_valid_json",
                        DoctorCheckStatus::Fail,
                        format!(
                            "daemon config parse error at {}: {}",
                            daemon_config_path.display(),
                            error
                        ),
                        "manual_pm_config_repair",
                        false,
                        "repair malformed daemon config JSON",
                        None,
                    ),
                },
                Err(error) => build_check(
                    "daemon_config_valid_json",
                    DoctorCheckStatus::Fail,
                    format!(
                        "failed to read daemon config at {}: {}",
                        daemon_config_path.display(),
                        error
                    ),
                    "manual_pm_config_repair",
                    false,
                    "repair unreadable daemon config file permissions",
                    None,
                ),
            }
        };
        checks.push(daemon_check);

        let detected_clis = detect_llm_clis();
        checks.push(build_check(
            "llm_cli_availability",
            if detected_clis.is_empty() {
                DoctorCheckStatus::Warn
            } else {
                DoctorCheckStatus::Ok
            },
            if detected_clis.is_empty() {
                "no supported LLM CLI detected on PATH (checked codex, claude, gemini, opencode)"
                    .to_string()
            } else {
                format!("detected CLI tools: {}", detected_clis.join(", "))
            },
            "install_or_configure_llm_cli",
            false,
            "install and authenticate at least one supported LLM CLI",
            None,
        ));

        #[cfg(unix)]
        let runner_socket_path = protocol::Config::global_config_dir().join("agent-runner.sock");
        #[cfg(unix)]
        checks.push(build_check(
            "runner_socket_available",
            if runner_socket_path.exists() {
                DoctorCheckStatus::Ok
            } else {
                DoctorCheckStatus::Warn
            },
            if runner_socket_path.exists() {
                format!("runner socket detected at {}", runner_socket_path.display())
            } else {
                format!(
                    "runner socket not found at {}",
                    runner_socket_path.display()
                )
            },
            "start_runner",
            true,
            "start or connect to agent runner",
            Some("ao agent runner-status --start-runner"),
        ));

        #[cfg(not(unix))]
        checks.push(build_check(
            "runner_socket_available",
            DoctorCheckStatus::Warn,
            "runner socket check is only available on unix-like systems".to_string(),
            "start_runner",
            true,
            "start or connect to agent runner",
            Some("ao agent runner-status --start-runner"),
        ));

        let result = derive_result(&checks);
        Self { result, checks }
    }
}

fn build_check(
    id: &str,
    status: DoctorCheckStatus,
    details: String,
    remediation_id: &str,
    remediation_available: bool,
    remediation_details: &str,
    remediation_command: Option<&str>,
) -> DoctorCheck {
    DoctorCheck {
        id: id.to_string(),
        status,
        details,
        remediation: DoctorRemediation {
            id: remediation_id.to_string(),
            available: remediation_available,
            details: remediation_details.to_string(),
            command: remediation_command.map(str::to_string),
        },
    }
}

fn derive_result(checks: &[DoctorCheck]) -> DoctorCheckResult {
    if checks
        .iter()
        .any(|check| check.status == DoctorCheckStatus::Fail)
    {
        return DoctorCheckResult::Unhealthy;
    }
    if checks
        .iter()
        .any(|check| check.status == DoctorCheckStatus::Warn)
    {
        return DoctorCheckResult::Degraded;
    }
    DoctorCheckResult::Healthy
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DirectoryState {
    Missing,
    Directory,
    NotDirectory,
}

fn directory_state(path: &Path) -> DirectoryState {
    if !path.exists() {
        return DirectoryState::Missing;
    }
    if path.is_dir() {
        return DirectoryState::Directory;
    }
    DirectoryState::NotDirectory
}

fn build_ao_file_check(
    check_id: &str,
    expected_path: &Path,
    ao_dir_state: DirectoryState,
) -> DoctorCheck {
    if ao_dir_state == DirectoryState::NotDirectory {
        return build_check(
            check_id,
            DoctorCheckStatus::Fail,
            format!(
                "AO state path is not a directory at {}; expected {}",
                expected_path.parent().unwrap_or(expected_path).display(),
                expected_path.display()
            ),
            "manual_ao_directory_repair",
            false,
            "replace non-directory .ao path with an AO state directory",
            None,
        );
    }

    build_check(
        check_id,
        if expected_path.exists() {
            DoctorCheckStatus::Ok
        } else {
            DoctorCheckStatus::Warn
        },
        format!("expected {}", expected_path.display()),
        "bootstrap_project_state",
        true,
        "create baseline AO state/config files",
        Some("ao doctor --fix"),
    )
}

fn detect_llm_clis() -> Vec<String> {
    ["codex", "claude", "gemini", "opencode"]
        .iter()
        .copied()
        .filter(|binary| binary_in_path(binary))
        .map(str::to_string)
        .collect()
}

fn binary_in_path(binary: &str) -> bool {
    let Some(path_var) = std::env::var_os("PATH") else {
        return false;
    };

    #[cfg(windows)]
    let ext_candidates: Vec<String> = {
        let pathext = std::env::var("PATHEXT").unwrap_or_else(|_| ".EXE;.CMD;.BAT;.COM".into());
        pathext
            .split(';')
            .filter(|value| !value.trim().is_empty())
            .map(|value| value.trim().to_ascii_lowercase())
            .collect()
    };

    std::env::split_paths(&path_var).any(|dir| {
        #[cfg(windows)]
        {
            let direct = dir.join(binary);
            if path_is_executable(&direct) {
                return true;
            }
            let lower_binary = binary.to_ascii_lowercase();
            for ext in &ext_candidates {
                if lower_binary.ends_with(ext) {
                    continue;
                }
                let candidate = dir.join(format!("{binary}{ext}"));
                if path_is_executable(&candidate) {
                    return true;
                }
            }
            false
        }
        #[cfg(not(windows))]
        {
            path_is_executable(&dir.join(binary))
        }
    })
}

#[cfg(unix)]
fn path_is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;

    std::fs::metadata(path)
        .map(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn path_is_executable(path: &Path) -> bool {
    path.is_file()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn run_for_project_reports_warns_for_missing_bootstrap_files() {
        let temp = tempfile::tempdir().expect("tempdir should be created");
        let report = DoctorReport::run_for_project(temp.path());

        assert!(report
            .checks
            .iter()
            .any(|check| check.id == "ao_directory_present"
                && check.status == DoctorCheckStatus::Warn));
        assert!(report.checks.iter().any(
            |check| check.id == "core_state_present" && check.status == DoctorCheckStatus::Warn
        ));
    }

    #[test]
    fn run_for_project_marks_invalid_daemon_config_as_fail() {
        let temp = tempfile::tempdir().expect("tempdir should be created");
        let ao_dir = temp.path().join(".ao");
        std::fs::create_dir_all(&ao_dir).expect("ao dir should be created");
        std::fs::write(ao_dir.join("pm-config.json"), "{not-json").expect("file should be written");

        let report = DoctorReport::run_for_project(temp.path());
        let daemon_check = report
            .checks
            .iter()
            .find(|check| check.id == "daemon_config_valid_json")
            .expect("daemon config check should exist");
        assert_eq!(daemon_check.status, DoctorCheckStatus::Fail);
        assert_eq!(report.result, DoctorCheckResult::Unhealthy);
    }

    #[test]
    fn run_for_project_marks_project_root_file_as_fail() {
        let temp = tempfile::tempdir().expect("tempdir should be created");
        let root_file = temp.path().join("project-root-file");
        std::fs::write(&root_file, "not a directory").expect("root file should be written");

        let report = DoctorReport::run_for_project(&root_file);
        let project_root_check = report
            .checks
            .iter()
            .find(|check| check.id == "project_root_exists")
            .expect("project root check should exist");
        assert_eq!(project_root_check.status, DoctorCheckStatus::Fail);
        assert!(
            project_root_check.details.contains("not a directory"),
            "project root details should mention invalid directory type"
        );
    }

    #[test]
    fn run_for_project_marks_non_directory_ao_path_as_fail() {
        let temp = tempfile::tempdir().expect("tempdir should be created");
        let ao_path = temp.path().join(".ao");
        std::fs::write(&ao_path, "not a directory").expect("ao file should be written");

        let report = DoctorReport::run_for_project(temp.path());

        for id in [
            "ao_directory_present",
            "core_state_present",
            "config_json_present",
            "resume_config_present",
            "daemon_config_valid_json",
        ] {
            let check = report
                .checks
                .iter()
                .find(|check| check.id == id)
                .expect("check should exist");
            assert_eq!(check.status, DoctorCheckStatus::Fail, "{id} should fail");
            assert_eq!(check.remediation.id, "manual_ao_directory_repair");
            assert!(!check.remediation.available);
        }
        assert_eq!(report.result, DoctorCheckResult::Unhealthy);
    }

    #[test]
    fn run_for_project_marks_expected_core_files_as_ok_when_present() {
        let temp = tempfile::tempdir().expect("tempdir should be created");
        let ao_dir = temp.path().join(".ao");
        std::fs::create_dir_all(&ao_dir).expect("ao dir should be created");
        std::fs::write(ao_dir.join("core-state.json"), "{}").expect("core state should be written");
        std::fs::write(ao_dir.join("config.json"), "{}").expect("config should be written");
        std::fs::write(ao_dir.join("resume-config.json"), "{}")
            .expect("resume config should be written");
        std::fs::write(ao_dir.join("pm-config.json"), "{}")
            .expect("daemon config should be written");

        let report = DoctorReport::run_for_project(temp.path());
        for id in [
            "ao_directory_present",
            "core_state_present",
            "config_json_present",
            "resume_config_present",
            "daemon_config_valid_json",
        ] {
            let check = report
                .checks
                .iter()
                .find(|check| check.id == id)
                .expect("check should exist");
            assert_eq!(
                check.status,
                DoctorCheckStatus::Ok,
                "check `{id}` should be ok"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn path_is_executable_requires_execute_permission() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().expect("tempdir should be created");
        let file_path = temp.path().join("candidate");
        fs::write(&file_path, "echo test").expect("file should be created");

        let mut perms = fs::metadata(&file_path)
            .expect("file metadata should be read")
            .permissions();
        perms.set_mode(0o644);
        fs::set_permissions(&file_path, perms).expect("permissions should be updated");
        assert!(
            !path_is_executable(&file_path),
            "file without execute bit should not be executable"
        );

        let mut perms = fs::metadata(&file_path)
            .expect("file metadata should be read")
            .permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&file_path, perms).expect("permissions should be updated");
        assert!(
            path_is_executable(&file_path),
            "file with execute bit should be executable"
        );
    }
}
