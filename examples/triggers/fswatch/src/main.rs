//! `animus-trigger-fswatch` — reference filesystem-watch trigger backend.
//!
//! Reads a watched glob list out of the `trigger/watch` `config` block,
//! attaches a `notify` recommended watcher to each glob's parent directory,
//! debounces bursty modify events, and emits one [`TriggerEvent`] per
//! changed file. Demonstrates the end-to-end shape of a custom
//! `trigger_backend` plugin against the v0.5.5 protocol.
//!
//! The daemon's `TriggerSupervisor` forwards every enabled `type: plugin`
//! entry from workflow YAML as `config.triggers`. fswatch reads its
//! `trigger_id` out of each entry's `config` and starts a watcher per
//! entry. Shape on the wire:
//!
//! ```json
//! {
//!   "triggers": [
//!     {
//!       "id": "fswatch-default",
//!       "workflow_ref": "review-source-change",
//!       "config": {
//!         "trigger_id": "fswatch-default",
//!         "globs": ["src/**/*.rs", "docs/**/*.md"],
//!         "debounce_ms": 250
//!       }
//!     }
//!   ]
//! }
//! ```
//!
//! Backwards compatibility: when only a single trigger's `config` block
//! is passed (no enclosing `triggers` array), fswatch treats it as the
//! sole entry.
//!
//! Emitted [`TriggerEvent`] shape:
//!
//! ```json
//! {
//!   "event_id": "fswatch:src/lib.rs:1717003812000",
//!   "trigger_id": "fswatch-default",
//!   "action_hint": "run_workflow",
//!   "payload": {
//!     "path": "src/lib.rs",
//!     "kind": "modified",
//!     "occurred_at": "2026-05-30T18:30:12Z"
//!   }
//! }
//! ```
//!
//! Health: the watcher returns `Healthy` while the supervisor thread is
//! alive, `Unhealthy` if it crashed or never started.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use animus_plugin_protocol::{
    HealthCheckResult, HealthStatus, TriggerAckParams, TriggerActionHint, TriggerEvent,
    TriggerWatchParams, PLUGIN_KIND_TRIGGER_BACKEND, TRIGGER_METHOD_ACK, TRIGGER_METHOD_EVENT,
    TRIGGER_METHOD_WATCH,
};
use animus_plugin_runtime::{CancellationToken, Notifier, Plugin};
use anyhow::Result;
use chrono::Utc;
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use serde::Deserialize;
use serde_json::json;
use tokio::sync::{mpsc, Mutex};

const PLUGIN_NAME: &str = "animus-trigger-fswatch";
const PLUGIN_VERSION: &str = env!("CARGO_PKG_VERSION");
const PLUGIN_DESCRIPTION: &str = "Reference filesystem-watch trigger backend";
const DEFAULT_DEBOUNCE: Duration = Duration::from_millis(250);

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct FswatchConfig {
    trigger_id: Option<String>,
    globs: Vec<String>,
    debounce_ms: Option<u64>,
}

#[derive(Default)]
struct FswatchState {
    delivered: HashSet<String>,
    last_error: Option<String>,
    watching: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();

    let state: Arc<Mutex<FswatchState>> = Arc::new(Mutex::new(FswatchState::default()));

    let watch_state = state.clone();
    let ack_state = state.clone();
    let health_state = state.clone();

    Plugin::new(PLUGIN_NAME, PLUGIN_VERSION, PLUGIN_KIND_TRIGGER_BACKEND)
        .description(PLUGIN_DESCRIPTION)
        .methods([
            TRIGGER_METHOD_WATCH.to_string(),
            TRIGGER_METHOD_ACK.to_string(),
            "health/check".to_string(),
        ])
        .register_method::<TriggerWatchParams, serde_json::Value, _, _>(
            TRIGGER_METHOD_WATCH,
            move |params, ctx| {
                let state = watch_state.clone();
                async move {
                    ctx.keep_cancellation();
                    let cfgs = parse_config(&params).map_err(|err| {
                        animus_plugin_protocol::RpcError {
                            code: animus_plugin_protocol::error_codes::INVALID_PARAMS,
                            message: format!("fswatch trigger/watch config invalid: {err}"),
                            data: None,
                        }
                    })?;
                    let watch_count = cfgs.len();
                    for cfg in cfgs {
                        spawn_fswatch_loop(
                            state.clone(),
                            ctx.notifier.clone(),
                            ctx.cancellation.clone(),
                            cfg,
                        )
                        .await;
                    }
                    Ok(json!({"watching": true, "watcher_count": watch_count}))
                }
            },
        )
        .register_notification(TRIGGER_METHOD_ACK, move |params, _notifier| {
            let state = ack_state.clone();
            async move {
                if let Ok(parsed) = serde_json::from_value::<TriggerAckParams>(params) {
                    let mut guard = state.lock().await;
                    guard.delivered.remove(&parsed.event_id);
                }
            }
        })
        .register_method::<serde_json::Value, HealthCheckResult, _, _>(
            "health/check",
            move |_, _| {
                let state = health_state.clone();
                async move {
                    let guard = state.lock().await;
                    let status = if guard.last_error.is_some() {
                        HealthStatus::Unhealthy
                    } else if guard.watching {
                        HealthStatus::Healthy
                    } else {
                        HealthStatus::Degraded
                    };
                    Ok(HealthCheckResult {
                        status,
                        uptime_ms: None,
                        memory_usage_bytes: None,
                        last_error: guard.last_error.clone(),
                    })
                }
            },
        )
        .run()
        .await
}

fn parse_config(params: &TriggerWatchParams) -> Result<Vec<FswatchConfig>> {
    let Some(raw) = params.config.as_ref() else {
        return Ok(Vec::new());
    };
    if let Some(entries) = raw.get("triggers").and_then(|v| v.as_array()) {
        let mut out = Vec::new();
        for entry in entries {
            let cfg = entry.get("config").cloned().unwrap_or(serde_json::Value::Null);
            let parsed: FswatchConfig = serde_json::from_value(cfg).unwrap_or_default();
            if parsed.globs.is_empty() {
                continue;
            }
            let id_from_entry = entry.get("id").and_then(|v| v.as_str()).map(ToOwned::to_owned);
            out.push(FswatchConfig {
                trigger_id: parsed.trigger_id.or(id_from_entry),
                globs: parsed.globs,
                debounce_ms: parsed.debounce_ms,
            });
        }
        return Ok(out);
    }
    let parsed: FswatchConfig = serde_json::from_value(raw.clone()).unwrap_or_default();
    if parsed.globs.is_empty() {
        Ok(Vec::new())
    } else {
        Ok(vec![parsed])
    }
}

async fn spawn_fswatch_loop(
    state: Arc<Mutex<FswatchState>>,
    notifier: Notifier,
    cancellation: CancellationToken,
    cfg: FswatchConfig,
) {
    if cfg.globs.is_empty() {
        let mut guard = state.lock().await;
        guard.last_error = Some("config.globs must not be empty".to_string());
        guard.watching = false;
        return;
    }
    {
        let mut guard = state.lock().await;
        guard.watching = true;
        guard.last_error = None;
    }
    let trigger_id = cfg.trigger_id.clone();
    let debounce = cfg
        .debounce_ms
        .map(Duration::from_millis)
        .unwrap_or(DEFAULT_DEBOUNCE);
    let globs = cfg.globs.clone();

    tokio::spawn(async move {
        if let Err(err) = run_fswatch(
            state.clone(),
            notifier,
            cancellation,
            trigger_id,
            globs,
            debounce,
        )
        .await
        {
            let mut guard = state.lock().await;
            guard.watching = false;
            guard.last_error = Some(err.to_string());
            tracing::error!(error = %err, "fswatch loop exited with error");
        }
    });
}

async fn run_fswatch(
    state: Arc<Mutex<FswatchState>>,
    notifier: Notifier,
    cancellation: CancellationToken,
    trigger_id: Option<String>,
    globs: Vec<String>,
    debounce: Duration,
) -> Result<()> {
    let project_root = std::env::current_dir()?;
    let (raw_tx, mut raw_rx) = mpsc::unbounded_channel::<PathBuf>();

    let raw_tx_clone = raw_tx.clone();
    let mut watcher = RecommendedWatcher::new(
        move |res: notify::Result<Event>| {
            if let Ok(event) = res {
                if !matches!(
                    event.kind,
                    EventKind::Modify(_) | EventKind::Create(_) | EventKind::Remove(_)
                ) {
                    return;
                }
                for path in event.paths {
                    let _ = raw_tx_clone.send(path);
                }
            }
        },
        notify::Config::default(),
    )?;

    let watched_roots = resolve_watch_roots(&globs)?;
    for root in &watched_roots {
        watcher.watch(root, RecursiveMode::Recursive)?;
        tracing::info!(path = %root.display(), "fswatch attached watcher");
    }

    let compiled: Vec<glob::Pattern> = globs
        .iter()
        .map(|g| glob::Pattern::new(g))
        .collect::<std::result::Result<_, _>>()?;

    let mut pending: HashMap<PathBuf, std::time::Instant> = HashMap::new();
    let mut tick = tokio::time::interval(debounce.max(Duration::from_millis(50)));

    loop {
        tokio::select! {
            _ = cancellation.cancelled() => {
                tracing::info!("fswatch loop cancelled");
                break Ok(());
            }
            Some(path) = raw_rx.recv() => {
                let relative = path.strip_prefix(&project_root).map(Path::to_path_buf).unwrap_or(path);
                if path_matches_any(&relative, &compiled) {
                    pending.insert(relative, std::time::Instant::now());
                }
            }
            _ = tick.tick() => {
                let now = std::time::Instant::now();
                let ready: Vec<PathBuf> = pending
                    .iter()
                    .filter(|(_, seen)| now.duration_since(**seen) >= debounce)
                    .map(|(path, _)| path.clone())
                    .collect();
                for path in ready {
                    pending.remove(&path);
                    let event_id = format!(
                        "fswatch:{}:{}",
                        path.display(),
                        Utc::now().timestamp_millis()
                    );
                    let event = TriggerEvent {
                        event_id: event_id.clone(),
                        trigger_id: trigger_id.clone(),
                        subject_id: None,
                        subject_kind: None,
                        action_hint: Some(TriggerActionHint::RunWorkflow),
                        payload: json!({
                            "path": path.to_string_lossy(),
                            "kind": "modified",
                            "occurred_at": Utc::now().to_rfc3339(),
                        }),
                    };
                    {
                        let mut guard = state.lock().await;
                        guard.delivered.insert(event_id);
                    }
                    notifier.notify_typed(TRIGGER_METHOD_EVENT, &event).await;
                }
            }
        }
    }
}

fn path_matches_any(path: &Path, patterns: &[glob::Pattern]) -> bool {
    let str_path = path.to_string_lossy();
    patterns.iter().any(|p| p.matches(&str_path))
}

fn resolve_watch_roots(globs: &[String]) -> Result<Vec<PathBuf>> {
    let mut roots: HashSet<PathBuf> = HashSet::new();
    for glob_pattern in globs {
        let root = glob_root(glob_pattern);
        let absolute = if root.is_absolute() {
            root
        } else {
            std::env::current_dir()?.join(root)
        };
        if absolute.exists() {
            roots.insert(absolute);
        } else if let Some(parent) = absolute.parent() {
            if parent.exists() {
                roots.insert(parent.to_path_buf());
            }
        }
    }
    if roots.is_empty() {
        roots.insert(std::env::current_dir()?);
    }
    Ok(roots.into_iter().collect())
}

fn glob_root(pattern: &str) -> PathBuf {
    let mut root = PathBuf::new();
    for component in pattern.split('/') {
        if component.contains('*') || component.contains('?') || component.contains('[') {
            break;
        }
        root.push(component);
    }
    if root.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        root
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_root_extracts_prefix() {
        assert_eq!(glob_root("src/**/*.rs"), PathBuf::from("src"));
        assert_eq!(glob_root("a/b/c.txt"), PathBuf::from("a/b/c.txt"));
        assert_eq!(glob_root("*.md"), PathBuf::from("."));
    }

    #[test]
    fn path_matches_any_handles_globs() {
        let patterns = vec![
            glob::Pattern::new("src/**/*.rs").unwrap(),
            glob::Pattern::new("docs/*.md").unwrap(),
        ];
        assert!(path_matches_any(Path::new("src/lib.rs"), &patterns));
        assert!(path_matches_any(Path::new("src/sub/mod.rs"), &patterns));
        assert!(path_matches_any(Path::new("docs/intro.md"), &patterns));
        assert!(!path_matches_any(Path::new("README.md"), &patterns));
    }

    #[test]
    fn parse_config_accepts_full_block() {
        let params = TriggerWatchParams {
            cursor: None,
            config: Some(json!({
                "trigger_id": "fswatch-default",
                "globs": ["src/**/*.rs"],
                "debounce_ms": 500
            })),
        };
        let cfgs = parse_config(&params).unwrap();
        assert_eq!(cfgs.len(), 1);
        assert_eq!(cfgs[0].trigger_id.as_deref(), Some("fswatch-default"));
        assert_eq!(cfgs[0].globs, vec!["src/**/*.rs".to_string()]);
        assert_eq!(cfgs[0].debounce_ms, Some(500));
    }

    #[test]
    fn parse_config_reads_triggers_array_from_supervisor() {
        let params = TriggerWatchParams {
            cursor: None,
            config: Some(json!({
                "triggers": [
                    {
                        "id": "fswatch-default",
                        "workflow_ref": "review",
                        "config": {
                            "globs": ["src/**/*.rs"],
                            "debounce_ms": 100
                        }
                    }
                ]
            })),
        };
        let cfgs = parse_config(&params).unwrap();
        assert_eq!(cfgs.len(), 1);
        assert_eq!(cfgs[0].trigger_id.as_deref(), Some("fswatch-default"), "should fall back to entry id");
        assert_eq!(cfgs[0].globs, vec!["src/**/*.rs".to_string()]);
        assert_eq!(cfgs[0].debounce_ms, Some(100));
    }

    #[test]
    fn parse_config_returns_empty_when_no_entries_have_globs() {
        let params = TriggerWatchParams {
            cursor: None,
            config: Some(json!({
                "triggers": [
                    {"id": "other-plugin", "config": {"different_key": "x"}}
                ]
            })),
        };
        let cfgs = parse_config(&params).unwrap();
        assert!(cfgs.is_empty(), "no matching entry should yield empty config list");
    }

    #[test]
    fn parse_config_keeps_every_matching_entry() {
        let params = TriggerWatchParams {
            cursor: None,
            config: Some(json!({
                "triggers": [
                    {"id": "fswatch-rs", "config": {"globs": ["src/**/*.rs"]}},
                    {"id": "other-plugin", "config": {"slack_channel": "C123"}},
                    {"id": "fswatch-md", "config": {"globs": ["docs/**/*.md"], "debounce_ms": 1000}}
                ]
            })),
        };
        let cfgs = parse_config(&params).unwrap();
        assert_eq!(cfgs.len(), 2, "should keep both matching fswatch entries");
        assert_eq!(cfgs[0].trigger_id.as_deref(), Some("fswatch-rs"));
        assert_eq!(cfgs[1].trigger_id.as_deref(), Some("fswatch-md"));
        assert_eq!(cfgs[1].debounce_ms, Some(1000));
    }
}
