use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const DAEMON_PROJECT_CONFIG_FILE_NAME: &str = "pm-config.json";

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct DaemonProjectConfig {
    // Runtime-reconfigurable settings (persisted, hot-reloaded by daemon each tick)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pool_size: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interval_secs: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tasks_per_tick: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_run_ready: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stale_threshold_hours: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase_timeout_secs: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idle_timeout_secs: Option<u64>,
    /// Unknown keys round-trip untouched. This also keeps pm-config.json
    /// files written before the v0.5.x removal of the daemon git/merge
    /// policy fields (`auto_merge_enabled`, `auto_pr_enabled`,
    /// `auto_commit_before_merge`, `auto_merge_target_branch`,
    /// `auto_merge_no_ff`, `auto_push_remote`,
    /// `auto_cleanup_worktree_enabled`,
    /// `auto_prune_worktrees_after_merge`) loading without error.
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

pub fn daemon_project_config_path(project_root: &Path) -> PathBuf {
    protocol::scoped_state_root(project_root)
        .map(|root| root.join("daemon").join(DAEMON_PROJECT_CONFIG_FILE_NAME))
        .expect("scoped_state_root requires a home directory")
}

pub fn load_daemon_project_config(project_root: &Path) -> Result<DaemonProjectConfig> {
    let path = daemon_project_config_path(project_root);
    if !path.exists() {
        return Ok(DaemonProjectConfig::default());
    }

    let content =
        fs::read_to_string(&path).with_context(|| format!("failed to read daemon config at {}", path.display()))?;
    if content.trim().is_empty() {
        return Ok(DaemonProjectConfig::default());
    }

    serde_json::from_str(&content).with_context(|| format!("invalid daemon config JSON at {}", path.display()))
}

pub fn write_daemon_project_config(project_root: &Path, config: &DaemonProjectConfig) -> Result<()> {
    let path = daemon_project_config_path(project_root);
    crate::domain_state::write_json_pretty(&path, config)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_daemon_project_config_defaults_when_missing() {
        crate::test_env::stable_test_home();
        let temp = tempfile::tempdir().expect("tempdir should be created");
        let loaded = load_daemon_project_config(temp.path()).expect("missing daemon config should default");
        assert_eq!(loaded, DaemonProjectConfig::default());
    }

    #[test]
    fn daemon_project_config_preserves_unknown_fields() {
        crate::test_env::stable_test_home();
        let temp = tempfile::tempdir().expect("tempdir should be created");
        let config_path = daemon_project_config_path(temp.path());
        if let Some(parent) = config_path.parent() {
            std::fs::create_dir_all(parent).expect("config dir should be created");
        }
        std::fs::write(
            &config_path,
            serde_json::to_string_pretty(&serde_json::json!({
                "custom_key": "keep-me"
            }))
            .expect("json should serialize"),
        )
        .expect("seed config should be written");

        let loaded = load_daemon_project_config(temp.path()).expect("config should load");
        assert_eq!(loaded.extra.get("custom_key").and_then(Value::as_str), Some("keep-me"));

        write_daemon_project_config(temp.path(), &loaded).expect("config should write");
        let content = std::fs::read_to_string(config_path).expect("written config should be read");
        let parsed: Value = serde_json::from_str(&content).expect("written config should parse");
        assert_eq!(parsed.get("custom_key").and_then(Value::as_str), Some("keep-me"));
    }

    #[test]
    fn daemon_project_config_loads_legacy_auto_policy_fields() {
        // pm-config.json files written before the v0.5.x removal of the
        // daemon git/merge policy still carry the removed fields. They must
        // keep loading (the removed keys land in `extra` and round-trip).
        crate::test_env::stable_test_home();
        let temp = tempfile::tempdir().expect("tempdir should be created");
        let config_path = daemon_project_config_path(temp.path());
        if let Some(parent) = config_path.parent() {
            std::fs::create_dir_all(parent).expect("config dir should be created");
        }
        std::fs::write(
            &config_path,
            serde_json::to_string_pretty(&serde_json::json!({
                "auto_merge_enabled": true,
                "auto_pr_enabled": false,
                "auto_commit_before_merge": true,
                "auto_merge_target_branch": "main",
                "auto_merge_no_ff": true,
                "auto_push_remote": "origin",
                "auto_cleanup_worktree_enabled": true,
                "auto_prune_worktrees_after_merge": false,
                "pool_size": 4
            }))
            .expect("json should serialize"),
        )
        .expect("seed config should be written");

        let loaded = load_daemon_project_config(temp.path()).expect("legacy config should load");
        assert_eq!(loaded.pool_size, Some(4));
        assert_eq!(loaded.extra.get("auto_merge_enabled").and_then(Value::as_bool), Some(true));
        assert_eq!(loaded.extra.get("auto_push_remote").and_then(Value::as_str), Some("origin"));

        write_daemon_project_config(temp.path(), &loaded).expect("config should write");
        let reloaded = load_daemon_project_config(temp.path()).expect("config should reload");
        assert_eq!(reloaded, loaded);
    }

    #[test]
    fn daemon_project_config_round_trips_runtime_fields() {
        crate::test_env::stable_test_home();
        let temp = tempfile::tempdir().expect("tempdir");
        let config = DaemonProjectConfig {
            pool_size: Some(8),
            interval_secs: Some(30),
            max_tasks_per_tick: Some(5),
            auto_run_ready: Some(false),
            stale_threshold_hours: Some(48),
            phase_timeout_secs: Some(600),
            idle_timeout_secs: Some(1200),
            ..Default::default()
        };
        write_daemon_project_config(temp.path(), &config).expect("write should succeed");
        let loaded = load_daemon_project_config(temp.path()).expect("load should succeed");
        assert_eq!(loaded.pool_size, Some(8));
        assert_eq!(loaded.interval_secs, Some(30));
        assert_eq!(loaded.max_tasks_per_tick, Some(5));
        assert_eq!(loaded.auto_run_ready, Some(false));
        assert_eq!(loaded.stale_threshold_hours, Some(48));
        assert_eq!(loaded.phase_timeout_secs, Some(600));
        assert_eq!(loaded.idle_timeout_secs, Some(1200));
    }

    #[test]
    fn daemon_project_config_serializes_none_runtime_fields_omitted() {
        let config = DaemonProjectConfig {
            pool_size: Some(4),
            interval_secs: None,
            max_tasks_per_tick: None,
            auto_run_ready: None,
            stale_threshold_hours: None,
            phase_timeout_secs: None,
            idle_timeout_secs: None,
            ..Default::default()
        };
        let json = serde_json::to_string(&config).expect("serialize should succeed");
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("parse should succeed");
        assert_eq!(parsed.get("pool_size").and_then(serde_json::Value::as_u64), Some(4));
        // skip_serializing_if = "Option::is_none" means these should be absent
        assert!(!parsed.as_object().unwrap().contains_key("interval_secs"));
        assert!(!parsed.as_object().unwrap().contains_key("max_tasks_per_tick"));
        assert!(!parsed.as_object().unwrap().contains_key("auto_run_ready"));
    }

    #[test]
    fn daemon_project_config_deserializes_missing_runtime_fields_as_none() {
        crate::test_env::stable_test_home();
        let temp = tempfile::tempdir().expect("tempdir");
        let config_path = daemon_project_config_path(temp.path());
        if let Some(parent) = config_path.parent() {
            std::fs::create_dir_all(parent).expect("config dir should be created");
        }
        // Config with only legacy fields — runtime fields should deserialize as None
        std::fs::write(
            &config_path,
            serde_json::to_string_pretty(&serde_json::json!({
                "auto_merge_enabled": true,
                "auto_pr_enabled": false
            }))
            .expect("json should serialize"),
        )
        .expect("seed config should be written");

        let loaded = load_daemon_project_config(temp.path()).expect("load should succeed");
        assert_eq!(loaded.pool_size, None);
        assert_eq!(loaded.interval_secs, None);
        assert_eq!(loaded.max_tasks_per_tick, None);
        assert_eq!(loaded.auto_run_ready, None);
    }
}
