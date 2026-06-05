//! Opt-in anonymous usage metrics.
//!
//! Privacy invariants (enforced at the type system level — see
//! `events.rs`):
//!
//! * No file paths in any event.
//! * No repo names, branch names, git URLs.
//! * No prompt content, model responses, generated text.
//! * No environment variable contents.
//! * No credentials, API keys, tokens.
//! * No subject IDs (task IDs, requirement IDs).
//!
//! Every event name and tag value is a compile-time enum. Disabling is
//! always possible via `metrics.enabled = false` in the global
//! `config.json` or `ANIMUS_METRICS_DISABLE=1`.

pub mod events;
pub mod store;
pub mod transport;

use std::io::{self, IsTerminal, Write};

use anyhow::{Context, Result};
use chrono::Utc;
use protocol::{AutoUpdateConfig, Config, MetricsConfig};
use uuid::Uuid;

pub use events::{
    CommandGroup, ErrorClass, Event, EventName, PluginKind, WorkflowKind, WorkflowOutcome,
};
pub use store::{LastSendStatus, PendingEvent};
pub use transport::{build_payload, send_with_retry, HttpTransport, MetricsTransport, Payload};

const ENV_DISABLE: &str = "ANIMUS_METRICS_DISABLE";

pub fn env_disabled() -> bool {
    protocol::parse_env_bool(ENV_DISABLE)
}

fn load_config() -> Option<Config> {
    Config::load_global().ok()
}

fn save_config(config: &Config) -> Result<()> {
    let config_dir = Config::global_config_dir();
    std::fs::create_dir_all(&config_dir)
        .with_context(|| format!("Failed to create config dir {}", config_dir.display()))?;
    let path = config_dir.join("config.json");
    let tmp = path.with_extension("json.tmp");
    let json = serde_json::to_string_pretty(config)?;
    std::fs::write(&tmp, json)?;
    std::fs::rename(&tmp, &path).with_context(|| format!("Failed to write {}", path.display()))?;
    Ok(())
}

pub fn is_enabled() -> bool {
    if env_disabled() {
        return false;
    }
    load_config().map(|c| c.metrics.enabled == Some(true)).unwrap_or(false)
}

pub fn record(event: &Event) {
    if !is_enabled() {
        return;
    }
    let record = PendingEvent::from_event(event);
    let _ = store::append(&record);
}

pub fn record_cli_invoked(group: CommandGroup) {
    record(&Event::CliInvoked { group });
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FirstRunOutcome {
    AlreadySet,
    OptedIn,
    OptedOut,
    NonInteractiveDefault,
    Skipped,
}

pub fn prompt_if_first_run(stream: &mut dyn Write) -> FirstRunOutcome {
    let Some(mut config) = load_config() else {
        return FirstRunOutcome::Skipped;
    };
    if config.metrics.enabled.is_some() {
        return FirstRunOutcome::AlreadySet;
    }

    let interactive = io::stdin().is_terminal() && io::stdout().is_terminal();
    if !interactive {
        config.metrics.enabled = Some(false);
        let _ = save_config(&config);
        return FirstRunOutcome::NonInteractiveDefault;
    }

    let prompt = "Help improve Animus with anonymous usage data?\n\
                  \n\
                  Sends event counters only (workflows started, plugins installed, errors hit).\n\
                  No code, no file paths, no repo names, no prompts, no credentials.\n\
                  Aggregate counts only, batched daily.\n\
                  \n\
                  Opt in? [Y/n] ";
    let _ = stream.write_all(prompt.as_bytes());
    let _ = stream.flush();

    let mut answer = String::new();
    if io::stdin().read_line(&mut answer).is_err() {
        config.metrics.enabled = Some(false);
        let _ = save_config(&config);
        return FirstRunOutcome::NonInteractiveDefault;
    }
    let trimmed = answer.trim().to_ascii_lowercase();
    let opted_in = matches!(trimmed.as_str(), "" | "y" | "yes");

    config.metrics.enabled = Some(opted_in);
    if opted_in && config.metrics.install_id.is_none() {
        config.metrics.install_id = Some(Uuid::new_v4().to_string());
    }
    let _ = save_config(&config);

    if opted_in {
        FirstRunOutcome::OptedIn
    } else {
        FirstRunOutcome::OptedOut
    }
}

fn ensure_install_id(config: &mut Config) -> bool {
    if config.metrics.enabled == Some(true) && config.metrics.install_id.is_none() {
        config.metrics.install_id = Some(Uuid::new_v4().to_string());
        return true;
    }
    false
}

#[derive(Debug, Clone)]
pub struct FlushReport {
    pub skipped: bool,
    pub sent_events: usize,
    pub attempted_count: usize,
    pub error: Option<String>,
}

pub async fn flush<T: MetricsTransport + ?Sized>(transport: &T) -> Result<FlushReport> {
    if env_disabled() {
        return Ok(FlushReport { skipped: true, sent_events: 0, attempted_count: 0, error: None });
    }
    let Some(mut config) = load_config() else {
        return Ok(FlushReport { skipped: true, sent_events: 0, attempted_count: 0, error: None });
    };
    if config.metrics.enabled != Some(true) {
        return Ok(FlushReport { skipped: true, sent_events: 0, attempted_count: 0, error: None });
    }
    if ensure_install_id(&mut config) {
        save_config(&config)?;
    }
    let pending = store::read_all().unwrap_or_default();
    let attempted_count = pending.len();
    if pending.is_empty() {
        return Ok(FlushReport { skipped: false, sent_events: 0, attempted_count: 0, error: None });
    }
    let install_id = config.metrics.install_id.clone().unwrap_or_default();
    let payload = build_payload(&install_id, pending);
    let send_result = send_with_retry(transport, &config.metrics.endpoint, &payload).await;

    let mut last_status = store::read_last_send();
    last_status.last_attempt_at = Some(Utc::now());
    last_status.last_event_count = Some(attempted_count);
    match send_result {
        Ok(()) => {
            last_status.last_success_at = Some(Utc::now());
            last_status.last_error = None;
            let _ = store::write_last_send(&last_status);
            store::clear()?;
            Ok(FlushReport { skipped: false, sent_events: attempted_count, attempted_count, error: None })
        }
        Err(err) => {
            let msg = format!("{err:#}");
            last_status.last_error = Some(msg.clone());
            let _ = store::write_last_send(&last_status);
            store::clear()?;
            Ok(FlushReport { skipped: false, sent_events: 0, attempted_count, error: Some(msg) })
        }
    }
}

#[derive(Debug, Clone)]
pub struct MetricsStatus {
    pub enabled: Option<bool>,
    pub env_disabled: bool,
    pub install_id: Option<String>,
    pub endpoint: String,
    pub batch_interval: String,
    pub pending_count: usize,
    pub last_send: LastSendStatus,
}

pub fn status_snapshot() -> MetricsStatus {
    let config = load_config().unwrap_or_else(|| Config {
        agent_runner_token: None,
        mcp_servers: std::collections::BTreeMap::new(),
        claude_profiles: std::collections::BTreeMap::new(),
        default_subject_kind: None,
        metrics: MetricsConfig::default(),
        auto_update: AutoUpdateConfig::default(),
    });
    MetricsStatus {
        enabled: config.metrics.enabled,
        env_disabled: env_disabled(),
        install_id: config.metrics.install_id.clone(),
        endpoint: config.metrics.endpoint.clone(),
        batch_interval: config.metrics.batch_interval.clone(),
        pending_count: store::read_all().map(|events| events.len()).unwrap_or(0),
        last_send: store::read_last_send(),
    }
}

pub fn set_enabled(enabled: bool) -> Result<()> {
    let Some(mut config) = load_config() else {
        anyhow::bail!("failed to load global config to update metrics opt-in");
    };
    config.metrics.enabled = Some(enabled);
    if enabled && config.metrics.install_id.is_none() {
        config.metrics.install_id = Some(Uuid::new_v4().to_string());
    }
    save_config(&config)?;
    if !enabled {
        let _ = store::clear();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use tempfile::TempDir;

    struct ConfigDirGuard {
        previous: Option<std::ffi::OsString>,
    }

    impl ConfigDirGuard {
        fn install(dir: &std::path::Path) -> Self {
            let previous = std::env::var_os("ANIMUS_CONFIG_DIR");
            std::env::set_var("ANIMUS_CONFIG_DIR", dir);
            Self { previous }
        }
    }

    impl Drop for ConfigDirGuard {
        fn drop(&mut self) {
            match self.previous.take() {
                Some(value) => std::env::set_var("ANIMUS_CONFIG_DIR", value),
                None => std::env::remove_var("ANIMUS_CONFIG_DIR"),
            }
        }
    }

    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        use std::sync::{Mutex, OnceLock};
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(())).lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    #[test]
    fn env_override_short_circuits_emission() {
        let _guard = env_lock();
        let temp = TempDir::new().unwrap();
        let _dir_guard = ConfigDirGuard::install(temp.path());

        let mut config = Config::load_global().unwrap();
        config.metrics.enabled = Some(true);
        config.metrics.install_id = Some("test-install".to_string());
        save_config(&config).unwrap();

        std::env::set_var(ENV_DISABLE, "1");
        assert!(!is_enabled(), "env override must defeat opted-in config");
        record(&Event::DaemonStarted);
        assert!(store::read_all().unwrap().is_empty(), "no events should be recorded while env override is set");
        std::env::remove_var(ENV_DISABLE);
    }

    #[test]
    fn non_interactive_first_run_defaults_to_opt_out() {
        let _guard = env_lock();
        let temp = TempDir::new().unwrap();
        let _dir_guard = ConfigDirGuard::install(temp.path());
        let config = Config::load_global().unwrap();
        assert!(config.metrics.enabled.is_none());

        let mut out = Cursor::new(Vec::new());
        let outcome = prompt_if_first_run(&mut out);
        assert_eq!(outcome, FirstRunOutcome::NonInteractiveDefault);
        let after = Config::load_global().unwrap();
        assert_eq!(after.metrics.enabled, Some(false));
    }

    #[test]
    fn set_enabled_assigns_install_id_on_opt_in() {
        let _guard = env_lock();
        let temp = TempDir::new().unwrap();
        let _dir_guard = ConfigDirGuard::install(temp.path());
        set_enabled(true).unwrap();
        let config = Config::load_global().unwrap();
        assert_eq!(config.metrics.enabled, Some(true));
        assert!(config.metrics.install_id.is_some(), "opting in must generate an install id");
    }

    #[test]
    fn set_enabled_false_drops_pending_events() {
        let _guard = env_lock();
        let temp = TempDir::new().unwrap();
        let _dir_guard = ConfigDirGuard::install(temp.path());
        set_enabled(true).unwrap();
        record(&Event::DaemonStarted);
        assert_eq!(store::read_all().unwrap().len(), 1);
        set_enabled(false).unwrap();
        assert_eq!(store::read_all().unwrap().len(), 0);
    }

    #[test]
    fn privacy_event_constructors_only_use_closed_enums() {
        let events: Vec<Event> = vec![
            Event::WorkflowStarted { kind: WorkflowKind::Task },
            Event::WorkflowStarted { kind: WorkflowKind::Requirement },
            Event::WorkflowStarted { kind: WorkflowKind::Custom },
            Event::WorkflowCompleted { outcome: WorkflowOutcome::Success },
            Event::WorkflowCompleted { outcome: WorkflowOutcome::Failure },
            Event::WorkflowCompleted { outcome: WorkflowOutcome::Cancelled },
            Event::PluginInstalled { kind: PluginKind::SubjectBackend },
            Event::PluginInstalled { kind: PluginKind::Provider },
            Event::PluginInstalled { kind: PluginKind::Transport },
            Event::PluginInstalled { kind: PluginKind::WebUi },
            Event::PluginInstalled { kind: PluginKind::Trigger },
            Event::PluginInstalled { kind: PluginKind::LogStorage },
            Event::PluginInstalled { kind: PluginKind::Queue },
            Event::PluginInstalled { kind: PluginKind::Notifier },
            Event::PluginInstalled { kind: PluginKind::WorkflowRunner },
            Event::PluginInstalled { kind: PluginKind::AgentRunner },
            Event::DaemonStarted,
            Event::ErrorHit { class: ErrorClass::ParseError },
            Event::ErrorHit { class: ErrorClass::PreflightFailed },
            Event::ErrorHit { class: ErrorClass::PluginCrash },
            Event::ErrorHit { class: ErrorClass::NetworkError },
            Event::ErrorHit { class: ErrorClass::Other },
            Event::CliInvoked { group: CommandGroup::Workflow },
            Event::UpdateApplied,
        ];
        for event in events {
            if let Some((key, value)) = event.tag() {
                assert!(!key.is_empty());
                assert!(!value.is_empty());
            }
        }
    }

    #[tokio::test]
    async fn flush_no_op_when_disabled() {
        let _guard = env_lock();
        let temp = TempDir::new().unwrap();
        let _dir_guard = ConfigDirGuard::install(temp.path());
        let transport = HttpTransport::new();
        let report = flush(&transport).await.unwrap();
        assert!(report.skipped);
        assert_eq!(report.attempted_count, 0);
    }

    #[tokio::test]
    async fn flush_clears_pending_after_failed_attempts() {
        struct AlwaysFail;
        #[async_trait::async_trait]
        impl MetricsTransport for AlwaysFail {
            async fn send(&self, _endpoint: &str, _payload: &Payload) -> anyhow::Result<()> {
                anyhow::bail!("network down");
            }
        }
        let _guard = env_lock();
        let temp = TempDir::new().unwrap();
        let _dir_guard = ConfigDirGuard::install(temp.path());
        set_enabled(true).unwrap();
        record(&Event::DaemonStarted);
        record(&Event::CliInvoked { group: CommandGroup::Status });
        assert_eq!(store::read_all().unwrap().len(), 2);
        let report = flush(&AlwaysFail).await.unwrap();
        assert!(!report.skipped);
        assert_eq!(report.attempted_count, 2);
        assert_eq!(report.sent_events, 0);
        assert!(report.error.is_some());
        assert_eq!(store::read_all().unwrap().len(), 0, "pending events must be dropped after final attempt");
    }
}
