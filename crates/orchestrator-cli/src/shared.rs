mod cli_error;
mod output;
mod parsing;
mod runner;
mod task_generation;

pub(crate) use cli_error::*;
pub(crate) use output::*;
pub(crate) use parsing::*;
pub(crate) use runner::*;
pub(crate) use task_generation::*;

#[cfg(test)]
pub(crate) fn test_env_lock() -> &'static std::sync::Mutex<()> {
    static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| std::sync::Mutex::new(()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AgentRunArgs;
    use anyhow::anyhow;
    use orchestrator_core::{DependencyType, TaskStatus};
    use protocol::RunId;
    use sha2::{Digest, Sha256};
    use std::path::Path;

    fn make_agent_run_args() -> AgentRunArgs {
        AgentRunArgs {
            run_id: None,
            tool: "codex".to_string(),
            model: "gpt-5.3-codex".to_string(),
            prompt: Some("test".to_string()),
            cwd: None,
            timeout_secs: None,
            context_json: None,
            runtime_contract_json: None,
            detach: false,
            stream: true,
            save_jsonl: false,
            jsonl_dir: None,
            start_runner: false,
            runner_scope: None,
        }
    }

    struct EnvVarGuard {
        key: &'static str,
        previous: Option<String>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: Option<&str>) -> Self {
            let previous = std::env::var(key).ok();
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
            Self { key, previous }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            if let Some(previous) = &self.previous {
                std::env::set_var(self.key, previous);
            } else {
                std::env::remove_var(self.key);
            }
        }
    }

    #[test]
    fn parse_task_status_supports_aliases() {
        assert_eq!(parse_task_status("todo").unwrap(), TaskStatus::Backlog);
        assert_eq!(
            parse_task_status("in-progress").unwrap(),
            TaskStatus::InProgress
        );
        assert_eq!(parse_task_status("on_hold").unwrap(), TaskStatus::OnHold);
        assert!(parse_task_status("nonsense").is_err());
    }

    #[test]
    fn parse_dependency_type_supports_aliases() {
        assert_eq!(
            parse_dependency_type("blocked_by").unwrap(),
            DependencyType::BlockedBy
        );
        assert_eq!(
            parse_dependency_type("related-to").unwrap(),
            DependencyType::RelatedTo
        );
        assert!(parse_dependency_type("invalid").is_err());
    }

    #[test]
    fn runner_config_dir_prefers_explicit_override() {
        let _lock = test_env_lock()
            .lock()
            .expect("env lock should be available");
        let override_dir = tempfile::tempdir().expect("tempdir should be created");
        let override_dir_value = override_dir.path().to_string_lossy().to_string();
        let _ao_config = EnvVarGuard::set("AO_CONFIG_DIR", Some(&override_dir_value));
        let _legacy_config = EnvVarGuard::set("AGENT_ORCHESTRATOR_CONFIG_DIR", Some("ignored"));
        let _scope = EnvVarGuard::set("AO_RUNNER_SCOPE", Some("global"));

        let resolved = runner_config_dir(Path::new("/tmp/project-root"));
        assert_eq!(resolved, std::path::PathBuf::from(override_dir_value));
    }

    #[test]
    fn runner_config_dir_defaults_to_project_scope() {
        let _lock = test_env_lock()
            .lock()
            .expect("env lock should be available");
        let _ao_config = EnvVarGuard::set("AO_CONFIG_DIR", None);
        let _legacy_config = EnvVarGuard::set("AGENT_ORCHESTRATOR_CONFIG_DIR", None);
        let _scope = EnvVarGuard::set("AO_RUNNER_SCOPE", None);
        let project_root = Path::new("project-root");

        let resolved = runner_config_dir(project_root);
        assert!(resolved.ends_with("runner"));
        assert!(
            resolved
                .components()
                .any(|component| component.as_os_str() == ".ao"),
            "project scoped runner dir should be under ~/.ao/<repo-scope>/runner"
        );
        assert_ne!(resolved, project_root.join(".ao").join("runner"));
    }

    #[cfg(unix)]
    #[test]
    fn runner_config_dir_shortens_long_unix_socket_paths() {
        let _lock = test_env_lock()
            .lock()
            .expect("env lock should be available");
        let _ao_config = EnvVarGuard::set("AO_CONFIG_DIR", None);
        let _legacy_config = EnvVarGuard::set("AGENT_ORCHESTRATOR_CONFIG_DIR", None);
        let _scope = EnvVarGuard::set("AO_RUNNER_SCOPE", None);

        let long_root = std::path::PathBuf::from("/tmp").join("x".repeat(220));
        let default_dir = long_root.join(".ao").join("runner");
        let resolved = runner_config_dir(&long_root);

        assert_ne!(resolved, default_dir);
        assert!(
            resolved.join("agent-runner.sock").to_string_lossy().len() <= 100,
            "runner socket path should be shortened for unix sockets"
        );
    }

    #[test]
    fn run_dir_defaults_to_scoped_runtime_runs_root() {
        let temp = tempfile::tempdir().expect("tempdir should be created");
        let project_root = temp.path().join("project-root");
        std::fs::create_dir_all(&project_root).expect("project dir should be created");
        let run_id = RunId("trace-run-010".to_string());

        let resolved = run_dir(project_root.to_string_lossy().as_ref(), &run_id, None);
        let canonical = project_root
            .canonicalize()
            .expect("project path should canonicalize");
        let mut hasher = Sha256::new();
        hasher.update(canonical.to_string_lossy().as_bytes());
        let digest = hasher.finalize();
        let scope = format!(
            "project-root-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
            digest[0], digest[1], digest[2], digest[3], digest[4], digest[5]
        );
        let expected = dirs::home_dir()
            .expect("home directory should resolve")
            .join(".ao")
            .join(scope)
            .join("runs")
            .join(&run_id.0);

        assert_eq!(resolved, expected);
        assert_ne!(
            resolved,
            project_root.join(".ao").join("runs").join(&run_id.0)
        );
    }

    #[test]
    fn run_dir_stays_repo_scoped_when_runner_scope_is_global() {
        let _lock = test_env_lock()
            .lock()
            .expect("env lock should be available");
        let temp = tempfile::tempdir().expect("tempdir should be created");
        let _home = EnvVarGuard::set("HOME", Some(temp.path().to_string_lossy().as_ref()));
        let override_dir = temp.path().join("override-config");
        let _scope = EnvVarGuard::set("AO_RUNNER_SCOPE", Some("global"));
        let _config_override = EnvVarGuard::set(
            "AO_CONFIG_DIR",
            Some(override_dir.to_string_lossy().as_ref()),
        );

        let project_root = temp.path().join("project-root");
        std::fs::create_dir_all(&project_root).expect("project dir should be created");
        let run_id = RunId("trace-run-global-scope".to_string());

        let resolved = run_dir(project_root.to_string_lossy().as_ref(), &run_id, None);
        assert!(
            resolved.starts_with(temp.path().join(".ao")),
            "run directory should stay under ~/.ao repo-scoped runtime root"
        );
        assert!(
            !resolved.starts_with(&override_dir),
            "run directory should not use AO_CONFIG_DIR overrides"
        );
    }

    #[test]
    fn run_dir_uses_base_override_when_provided() {
        let project_root = tempfile::tempdir().expect("tempdir should be created");
        let override_root = tempfile::tempdir().expect("tempdir should be created");
        let run_id = RunId("trace-run-override".to_string());

        let resolved = run_dir(
            project_root.path().to_string_lossy().as_ref(),
            &run_id,
            Some(override_root.path().to_string_lossy().as_ref()),
        );
        assert_eq!(resolved, override_root.path().join(&run_id.0));
    }

    #[test]
    fn classify_error_maps_expected_exit_codes() {
        let invalid = invalid_input_error("invalid status");
        let confirmation =
            invalid_input_error("CONFIRMATION_REQUIRED: rerun command with --confirm TASK-1");
        let unavailable = unavailable_error("failed to connect to runner");
        let not_found = not_found_error("task not found");
        let conflict = conflict_error("architecture entity already exists");
        let internal = anyhow!("runner returned status payload while waiting for control response");

        assert_eq!(classify_exit_code(&invalid), 2);
        assert_eq!(classify_exit_code(&confirmation), 2);
        assert_eq!(classify_exit_code(&not_found), 3);
        assert_eq!(classify_exit_code(&conflict), 4);
        assert_eq!(classify_exit_code(&unavailable), 5);
        assert_eq!(classify_exit_code(&internal), 1);
    }

    #[test]
    fn collect_json_payload_lines_keeps_json_objects_and_arrays_only() {
        let input = "\n{\"kind\":\"event\"}\nnot-json\n[1,2,3]\n123\n";
        let rows = collect_json_payload_lines(input);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].0, "{\"kind\":\"event\"}");
        assert!(rows[0].1.is_object());
        assert!(rows[1].1.is_array());
    }

    #[test]
    fn build_runtime_contract_includes_rich_shape() {
        let contract = build_runtime_contract("codex", "gpt-5.3-codex", "hello world")
            .expect("codex runtime contract should be generated");

        assert_eq!(
            contract
                .pointer("/cli/name")
                .and_then(serde_json::Value::as_str),
            Some("codex")
        );
        assert_eq!(
            contract
                .pointer("/cli/capabilities/supports_tool_use")
                .and_then(serde_json::Value::as_bool),
            Some(true)
        );
        assert!(contract.get("mcp").is_some());
    }

    #[test]
    fn build_runtime_contract_honors_codex_reasoning_override_env() {
        let _lock = test_env_lock()
            .lock()
            .expect("env lock should be available");
        let _override = EnvVarGuard::set("AO_CODEX_REASONING_EFFORT", Some("high"));
        let contract = build_runtime_contract("codex", "gpt-5", "hello world")
            .expect("codex runtime contract should be generated");

        let args = contract
            .pointer("/cli/launch/args")
            .and_then(serde_json::Value::as_array)
            .expect("launch args should exist")
            .iter()
            .filter_map(serde_json::Value::as_str)
            .collect::<Vec<_>>();
        assert!(args.contains(&"-c"));
        assert!(args.contains(&"model_reasoning_effort=high"));
        assert_eq!(args.last().copied(), Some("hello world"));
    }

    #[test]
    fn build_agent_context_rejects_cwd_outside_project() {
        let temp = tempfile::tempdir().expect("tempdir should be created");
        let project = temp.path().join("project");
        let outside = temp.path().join("outside");
        std::fs::create_dir_all(&project).expect("project dir should be created");
        std::fs::create_dir_all(&outside).expect("outside dir should be created");

        let mut args = make_agent_run_args();
        args.cwd = Some(outside.to_string_lossy().to_string());

        let error = build_agent_context(&args, project.to_string_lossy().as_ref())
            .expect_err("cwd outside project must be rejected");
        assert!(error.to_string().contains("Security violation"));
    }

    #[test]
    fn build_agent_context_accepts_relative_cwd_inside_project() {
        let temp = tempfile::tempdir().expect("tempdir should be created");
        let project = temp.path().join("project");
        let nested = project.join("src");
        std::fs::create_dir_all(&nested).expect("nested dir should be created");

        let mut args = make_agent_run_args();
        args.cwd = Some("src".to_string());

        let context = build_agent_context(&args, project.to_string_lossy().as_ref())
            .expect("relative cwd inside project should be accepted");
        let expected = nested
            .canonicalize()
            .expect("nested path should canonicalize")
            .to_string_lossy()
            .to_string();
        assert_eq!(
            context.get("cwd").and_then(serde_json::Value::as_str),
            Some(expected.as_str())
        );
    }

    #[test]
    fn build_agent_context_accepts_managed_worktree_cwd() {
        let _lock = test_env_lock()
            .lock()
            .expect("env lock should be available");
        let temp = tempfile::tempdir().expect("tempdir should be created");
        let _home = EnvVarGuard::set("HOME", Some(temp.path().to_string_lossy().as_ref()));

        let project = temp.path().join("project");
        std::fs::create_dir_all(&project).expect("project dir should be created");
        let project_canonical = project
            .canonicalize()
            .expect("project path should canonicalize");

        let repo_scope = {
            use sha2::Digest;
            let canonical_display = project_canonical.to_string_lossy();
            let repo_name = project_canonical
                .file_name()
                .and_then(|value| value.to_str())
                .map(|value| value.to_ascii_lowercase())
                .unwrap_or_else(|| "repo".to_string());
            let mut hasher = sha2::Sha256::new();
            hasher.update(canonical_display.as_bytes());
            let digest = hasher.finalize();
            let suffix = format!(
                "{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
                digest[0], digest[1], digest[2], digest[3], digest[4], digest[5]
            );
            format!("{repo_name}-{suffix}")
        };

        let repo_ao_root = temp.path().join(".ao").join(repo_scope);
        let worktree = repo_ao_root.join("worktrees").join("task-task-011");
        std::fs::create_dir_all(&worktree).expect("managed worktree should be created");
        std::fs::write(
            repo_ao_root.join(".project-root"),
            format!("{}\n", project_canonical.to_string_lossy()),
        )
        .expect("project marker should be written");

        let mut args = make_agent_run_args();
        args.cwd = Some(worktree.to_string_lossy().to_string());

        let context = build_agent_context(&args, project.to_string_lossy().as_ref())
            .expect("managed worktree cwd should be accepted");
        let expected = worktree
            .canonicalize()
            .expect("worktree path should canonicalize")
            .to_string_lossy()
            .to_string();
        assert_eq!(
            context.get("cwd").and_then(serde_json::Value::as_str),
            Some(expected.as_str())
        );
    }
}
