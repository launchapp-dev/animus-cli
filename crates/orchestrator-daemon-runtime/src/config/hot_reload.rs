//! Hot-reload for `.animus/workflows.yaml` and `.animus/workflows/*.yaml`.
//!
//! ## Design
//!
//! - A filesystem watcher (the `notify` crate, default recommended backend
//!   on each platform) observes `.animus/workflows.yaml` and the
//!   `.animus/workflows/` directory. Create / modify / remove events
//!   trigger a debounced reload of the workflow YAML pipeline.
//! - The most-recently successfully compiled [`WorkflowConfig`] is held in
//!   an [`arc_swap::ArcSwap`] so callers that read through the snapshot
//!   see a stable [`Arc`] for the duration of their borrow, even across a
//!   config swap. Today the daemon's workflow paths still load directly
//!   from disk (`load_workflow_config_with_metadata`); the snapshot is the
//!   long-term migration target and the source of truth for the
//!   `config_reloaded` broadcast hash.
//! - A malformed YAML edit logs a rustc-style diagnostic and keeps the
//!   previous snapshot active; the daemon never crashes due to an edit.
//! - On success, a `config_reloaded` event is broadcast on
//!   `workflow/events` with a synthetic workflow_id of `"<daemon>"` so
//!   existing subscribers fan it out without needing a new subscription
//!   shape.
//!
//! ## macOS FSEvents coarseness
//!
//! `notify`'s recommended backend on macOS is FSEvents, which delivers
//! file-system-level events that are already coarsened by the kernel. A
//! single editor write often becomes a single batched event regardless of
//! the userland write pattern. The [`WATCHER_DEBOUNCE_DEFAULT_MS`] window
//! is still applied uniformly so behaviour is consistent across Linux's
//! finer-grained inotify and Windows' ReadDirectoryChangesW.

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use arc_swap::ArcSwap;
use chrono::Utc;
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use orchestrator_core::{workflow_config_hash, WorkflowConfig};
use serde_json::json;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::control::WorkflowEventBroadcaster;

/// Default debounce window. Most editors (vim, vscode, emacs) write the
/// target file twice in rapid succession (atomic rename pattern); 500ms
/// coalesces those into a single reload while still feeling immediate to
/// a human author.
pub const WATCHER_DEBOUNCE_DEFAULT_MS: u64 = 500;

/// Wire-kind for the broadcast event emitted after a successful reload.
pub const RELOAD_EVENT_KIND: &str = "config_reloaded";

/// Wire-kind for the broadcast event emitted when a reload attempt fails
/// to compile or validate the YAML overlay.
pub const RELOAD_FAILED_EVENT_KIND: &str = "config_reload_failed";

/// Synthetic workflow id used on broadcast events that originate from the
/// daemon rather than a real workflow run.
const DAEMON_PSEUDO_WORKFLOW_ID: &str = "<daemon>";

/// Process-global slot for the daemon's most-recent successfully compiled
/// workflow config. Inner `Option` is `None` until the first compile lands.
static WORKFLOW_CONFIG_SNAPSHOT: OnceLock<WorkflowConfigSnapshot> = OnceLock::new();

/// Atomic snapshot handle for the workflow config. Cloned readers see a
/// stable `Arc<WorkflowConfig>` at the moment they `load()` and that arc
/// stays valid for the lifetime of the clone, even if a newer config has
/// since been swapped in.
#[derive(Clone, Default)]
pub struct WorkflowConfigSnapshot {
    inner: Arc<ArcSwap<Option<Arc<WorkflowConfig>>>>,
}

impl WorkflowConfigSnapshot {
    pub fn new() -> Self {
        Self { inner: Arc::new(ArcSwap::from_pointee(None)) }
    }

    pub fn current(&self) -> Option<Arc<WorkflowConfig>> {
        self.inner.load().as_ref().clone()
    }

    pub fn store(&self, config: Arc<WorkflowConfig>) {
        self.inner.store(Arc::new(Some(config)));
    }

    pub fn clear(&self) {
        self.inner.store(Arc::new(None));
    }
}

/// Return the process-global snapshot, creating it on first call.
pub fn workflow_config_snapshot() -> WorkflowConfigSnapshot {
    WORKFLOW_CONFIG_SNAPSHOT.get_or_init(WorkflowConfigSnapshot::new).clone()
}

/// Outcome of a single reload attempt. Returned from
/// [`reload_workflow_config_once`] and used as the body of the
/// CLI-facing `animus workflow config reload --json` envelope.
#[derive(Debug, Clone)]
pub struct WorkflowConfigReloadOutcome {
    pub reloaded: bool,
    pub phase_definitions: usize,
    pub workflows: usize,
    pub agent_profiles: usize,
    pub source_files: Vec<PathBuf>,
    pub config_hash: Option<String>,
    pub errors: Vec<String>,
}

impl WorkflowConfigReloadOutcome {
    pub fn to_json(&self) -> serde_json::Value {
        if self.reloaded {
            json!({
                "reloaded": true,
                "phase_definitions": self.phase_definitions,
                "workflows": self.workflows,
                "agent_profiles": self.agent_profiles,
                "source_files": self.source_files.iter().map(|p| p.display().to_string()).collect::<Vec<_>>(),
                "config_hash": self.config_hash,
            })
        } else {
            json!({
                "reloaded": false,
                "errors": self.errors.iter().map(|e| json!({"message": e})).collect::<Vec<_>>(),
                "source_files": self.source_files.iter().map(|p| p.display().to_string()).collect::<Vec<_>>(),
            })
        }
    }
}

/// Run the workflow YAML compile pipeline once.
///
/// On success: stores the freshly compiled config in `snapshot` and (when
/// `broadcaster` is `Some`) emits a `config_reloaded` event. On failure:
/// the prior snapshot is preserved untouched and the diagnostic is
/// returned in `errors` plus emitted as a `config_reload_failed` event
/// (when a broadcaster is wired).
pub fn reload_workflow_config_once(
    project_root: &Path,
    snapshot: &WorkflowConfigSnapshot,
    broadcaster: Option<&WorkflowEventBroadcaster>,
) -> WorkflowConfigReloadOutcome {
    match orchestrator_core::validate_and_compile_yaml_workflows(project_root) {
        Ok(Some(result)) => {
            let config_hash = workflow_config_hash(&result.config);
            let phase_definitions = result.config.phase_definitions.len();
            let agent_profiles = result.config.agent_profiles.len();
            let workflows = result.config.workflows.len();
            let source_files = result.source_files.clone();
            let config_arc = Arc::new(result.config);
            snapshot.store(config_arc);

            if let Some(bus) = broadcaster {
                let payload = json!({
                    "phase_definitions": phase_definitions,
                    "workflows": workflows,
                    "agent_profiles": agent_profiles,
                    "source_files": source_files.iter().map(|p| p.display().to_string()).collect::<Vec<_>>(),
                    "config_hash": config_hash,
                });
                bus.emit(animus_control_protocol::types::WorkflowEvent {
                    workflow_id: DAEMON_PSEUDO_WORKFLOW_ID.to_string(),
                    kind: RELOAD_EVENT_KIND.to_string(),
                    payload,
                    occurred_at: Utc::now(),
                });
            }

            tracing::info!(
                target: "animus.config.hot_reload",
                phase_definitions,
                workflows,
                agent_profiles,
                source_file_count = source_files.len(),
                config_hash = %config_hash,
                "workflow config reloaded; daemon transport settings unchanged"
            );

            WorkflowConfigReloadOutcome {
                reloaded: true,
                phase_definitions,
                workflows,
                agent_profiles,
                source_files,
                config_hash: Some(config_hash),
                errors: Vec::new(),
            }
        }
        Ok(None) => {
            // No YAML overlays present at all. If we previously held a
            // snapshot, the user has removed every overlay file — clear
            // the snapshot so callers don't keep seeing definitions for
            // workflows the operator has explicitly retired. Distinct
            // from the malformed-YAML case below, which preserves the
            // prior snapshot because the operator's intent there is "fix
            // the syntax", not "delete the config".
            snapshot.clear();
            WorkflowConfigReloadOutcome {
                reloaded: false,
                phase_definitions: 0,
                workflows: 0,
                agent_profiles: 0,
                source_files: Vec::new(),
                config_hash: None,
                errors: vec!["no YAML workflow files found in .animus/workflows/ or .animus/workflows.yaml".to_string()],
            }
        }
        Err(error) => {
            let diagnostic = format!("{error:#}");
            tracing::warn!(
                target: "animus.config.hot_reload",
                error = %diagnostic,
                "workflow YAML reload failed; previous config remains active"
            );
            if let Some(bus) = broadcaster {
                bus.emit(animus_control_protocol::types::WorkflowEvent {
                    workflow_id: DAEMON_PSEUDO_WORKFLOW_ID.to_string(),
                    kind: RELOAD_FAILED_EVENT_KIND.to_string(),
                    payload: json!({"error": diagnostic.clone()}),
                    occurred_at: Utc::now(),
                });
            }
            WorkflowConfigReloadOutcome {
                reloaded: false,
                phase_definitions: 0,
                workflows: 0,
                agent_profiles: 0,
                source_files: Vec::new(),
                config_hash: None,
                errors: vec![diagnostic],
            }
        }
    }
}

/// Handle for the spawned watcher task. Dropping the handle does NOT stop
/// the watcher; call [`WorkflowConfigWatcherHandle::shutdown`] from the
/// daemon teardown path to terminate it cleanly.
pub struct WorkflowConfigWatcherHandle {
    shutdown_tx: mpsc::Sender<()>,
    join: JoinHandle<()>,
    // Keep the `notify` watcher alive for the lifetime of the handle —
    // dropping it stops file-system event delivery, so we own it here.
    _watcher: RecommendedWatcher,
}

impl WorkflowConfigWatcherHandle {
    pub async fn shutdown(self) {
        let _ = self.shutdown_tx.send(()).await;
        let _ = self.join.await;
    }
}

/// Spawn the workflow YAML watcher. `debounce` is the coalescing window
/// applied to bursts of filesystem events. The returned handle keeps the
/// underlying `notify` watcher alive; the spawned task exits when the
/// shutdown channel fires.
pub fn spawn_workflow_config_watcher(
    project_root: PathBuf,
    snapshot: WorkflowConfigSnapshot,
    broadcaster: Arc<WorkflowEventBroadcaster>,
    debounce: Duration,
) -> notify::Result<WorkflowConfigWatcherHandle> {
    let animus_dir = project_root.join(".animus");
    // Ensure the .animus directory exists so the watcher can register
    // against it even when the project is fresh.
    if !animus_dir.exists() {
        if let Err(err) = std::fs::create_dir_all(&animus_dir) {
            tracing::warn!(
                target: "animus.config.hot_reload",
                path = %animus_dir.display(),
                error = %err,
                "failed to create .animus dir for hot-reload watcher; watcher may miss events until the dir appears"
            );
        }
    }

    let (raw_tx, mut raw_rx) = mpsc::unbounded_channel::<Event>();
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<Event>| {
        if let Ok(event) = res {
            let _ = raw_tx.send(event);
        }
    })?;

    // Watch the `.animus` directory recursively so we pick up both
    // `.animus/workflows.yaml` and any file under `.animus/workflows/`.
    // Editors using atomic rename land on the parent directory, so a
    // directory watch is the most reliable choice across backends.
    watcher.watch(&animus_dir, RecursiveMode::Recursive)?;

    let (shutdown_tx, mut shutdown_rx) = mpsc::channel::<()>(1);

    let join = tokio::spawn(async move {
        let mut pending_reload = false;
        loop {
            tokio::select! {
                biased;
                _ = shutdown_rx.recv() => {
                    break;
                }
                maybe_event = raw_rx.recv() => {
                    match maybe_event {
                        Some(event) => {
                            if event_affects_workflow_yaml(&event) {
                                pending_reload = true;
                            }
                        }
                        None => {
                            // Sender dropped — watcher gone, nothing to do.
                            break;
                        }
                    }
                }
            }

            if !pending_reload {
                continue;
            }

            // Debounce: keep draining events for `debounce` ms; any further
            // matching events extend the window only implicitly by being
            // consumed inside the select! below. After the window, run a
            // single reload for all coalesced edits.
            let mut deadline = tokio::time::Instant::now() + debounce;
            loop {
                tokio::select! {
                    biased;
                    _ = shutdown_rx.recv() => {
                        return;
                    }
                    _ = tokio::time::sleep_until(deadline) => {
                        break;
                    }
                    maybe_event = raw_rx.recv() => {
                        match maybe_event {
                            Some(event) => {
                                if event_affects_workflow_yaml(&event) {
                                    // Reset debounce window so rapid bursts
                                    // (vim's two-write pattern) coalesce
                                    // into a single reload.
                                    deadline = tokio::time::Instant::now() + debounce;
                                }
                            }
                            None => return,
                        }
                    }
                }
            }

            pending_reload = false;
            let outcome = {
                let project_root = project_root.clone();
                let snapshot = snapshot.clone();
                let broadcaster = broadcaster.clone();
                tokio::task::spawn_blocking(move || {
                    reload_workflow_config_once(&project_root, &snapshot, Some(&broadcaster))
                })
                .await
            };
            match outcome {
                Ok(out) if out.reloaded => {
                    tracing::debug!(
                        target: "animus.config.hot_reload",
                        phase_definitions = out.phase_definitions,
                        workflows = out.workflows,
                        "hot-reload succeeded"
                    );
                    // Wake the daemon scheduler loop so it recomputes the
                    // next cron deadline against the reloaded schedules
                    // immediately instead of after the in-flight sleep.
                    crate::daemon::nudge_scheduler_local();
                }
                Ok(out) => {
                    tracing::debug!(
                        target: "animus.config.hot_reload",
                        errors = ?out.errors,
                        "hot-reload skipped or failed"
                    );
                }
                Err(join_err) => {
                    tracing::warn!(
                        target: "animus.config.hot_reload",
                        error = %join_err,
                        "hot-reload task panicked or was cancelled"
                    );
                }
            }
        }
    });

    Ok(WorkflowConfigWatcherHandle { shutdown_tx, join, _watcher: watcher })
}

/// Predicate: does this filesystem event touch a workflow YAML file?
///
/// We accept Create / Modify / Remove. Vim writes a `.swp` + atomic rename
/// onto the real path; vscode writes a `*.tmp` then renames; both surface
/// here as Create / Modify on the destination. Filtering on the path
/// suffix screens out sibling files in `.animus/` (state JSON, pm-config)
/// so an unrelated daemon write doesn't trigger a reload storm.
fn event_affects_workflow_yaml(event: &Event) -> bool {
    if !matches!(event.kind, EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_) | EventKind::Any) {
        return false;
    }
    event.paths.iter().any(|p| path_is_workflow_yaml(p))
}

fn path_is_workflow_yaml(path: &Path) -> bool {
    // Also accept the well-known sentinel path the CLI drops to nudge a
    // daemon that missed earlier remove events (see
    // `touch_workflow_yaml_for_watcher` in orchestrator-cli). The sentinel
    // itself is a non-YAML temp file; treating it as a reload trigger is
    // safe because the next compile will just observe whatever overlays
    // are or aren't present.
    if path.file_name() == Some(std::ffi::OsStr::new(".reload-nudge")) {
        let parent = path.parent().and_then(|p| p.file_name());
        if parent == Some(std::ffi::OsStr::new(".animus")) {
            return true;
        }
    }

    let ext_ok = path
        .extension()
        .and_then(|os| os.to_str())
        .map(|ext| ext.eq_ignore_ascii_case("yaml") || ext.eq_ignore_ascii_case("yml"))
        .unwrap_or(false);
    if !ext_ok {
        return false;
    }
    let mut components = path.components().rev();
    let _file = match components.next() {
        Some(c) => c,
        None => return false,
    };
    let parent = match components.next() {
        Some(c) => c.as_os_str(),
        None => return false,
    };
    let grandparent = components.next().map(|c| c.as_os_str());

    // Match `.animus/workflows.yaml` or `.animus/workflows/<anything>.yaml`.
    let is_top_workflows_yaml =
        parent == std::ffi::OsStr::new(".animus") && path.file_name() == Some(std::ffi::OsStr::new("workflows.yaml"));
    let is_overlay_dir = parent == std::ffi::OsStr::new("workflows")
        && grandparent.map(|gp| gp == std::ffi::OsStr::new(".animus")).unwrap_or(false);
    is_top_workflows_yaml || is_overlay_dir
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn write_minimal_overlay(dir: &Path, phase_id: &str) {
        let animus = dir.join(".animus");
        fs::create_dir_all(animus.join("workflows")).unwrap();
        let yaml = format!(
            "phases:\n  {phase_id}:\n    mode: agent\n    agent_id: hot-reload-agent\nagents:\n  hot-reload-agent:\n    description: hot-reload fixture\n    system_prompt: hot-reload prompt\n    skills: []\nworkflows:\n  - id: hot-reload-workflow\n    name: Hot Reload\n    phases:\n      - {phase_id}\n"
        );
        fs::write(animus.join("workflows.yaml"), yaml).unwrap();
    }

    #[test]
    fn snapshot_starts_empty_and_round_trips_store_clear() {
        let snap = WorkflowConfigSnapshot::new();
        assert!(snap.current().is_none());
        let cfg = Arc::new(WorkflowConfig::default());
        snap.store(cfg.clone());
        let loaded = snap.current().expect("snapshot stores the config arc");
        assert!(Arc::ptr_eq(&loaded, &cfg), "snapshot must return the same Arc that was stored");
        snap.clear();
        assert!(snap.current().is_none());
    }

    #[test]
    fn reload_with_valid_overlay_populates_snapshot() {
        let dir = tempdir().unwrap();
        write_minimal_overlay(dir.path(), "alpha");
        let snap = WorkflowConfigSnapshot::new();
        let out = reload_workflow_config_once(dir.path(), &snap, None);
        assert!(out.reloaded, "reload must succeed: {:?}", out.errors);
        assert!(out.phase_definitions >= 1);
        let loaded = snap.current().expect("snapshot populated after successful reload");
        assert!(loaded.phase_definitions.contains_key("alpha"), "expected phase 'alpha' in compiled config");
    }

    #[test]
    fn reload_with_malformed_yaml_keeps_prior_snapshot() {
        let dir = tempdir().unwrap();
        write_minimal_overlay(dir.path(), "alpha");
        let snap = WorkflowConfigSnapshot::new();
        let first = reload_workflow_config_once(dir.path(), &snap, None);
        assert!(first.reloaded);
        let prior = snap.current().expect("first reload populated snapshot");

        // Overwrite with malformed content.
        fs::write(dir.path().join(".animus").join("workflows.yaml"), ": not valid\n[]\n").unwrap();
        let second = reload_workflow_config_once(dir.path(), &snap, None);
        assert!(!second.reloaded, "malformed YAML must NOT report reloaded=true");
        assert!(!second.errors.is_empty(), "diagnostic must be surfaced");

        let after = snap.current().expect("snapshot must persist prior config on failure");
        assert!(Arc::ptr_eq(&after, &prior), "snapshot must point at the prior config Arc unchanged");
    }

    #[test]
    fn reload_with_no_overlay_reports_not_reloaded() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join(".animus")).unwrap();
        let snap = WorkflowConfigSnapshot::new();
        let out = reload_workflow_config_once(dir.path(), &snap, None);
        assert!(!out.reloaded);
        assert!(snap.current().is_none(), "snapshot stays empty when there's nothing to compile");
    }

    #[test]
    fn path_filter_matches_top_level_and_overlay_files() {
        let root = PathBuf::from("/tmp/proj");
        assert!(path_is_workflow_yaml(&root.join(".animus/workflows.yaml")));
        assert!(path_is_workflow_yaml(&root.join(".animus/workflows/extra.yaml")));
        assert!(path_is_workflow_yaml(&root.join(".animus/workflows/extra.yml")));
        // Sibling files in .animus must NOT trigger the reload.
        assert!(!path_is_workflow_yaml(&root.join(".animus/config.json")));
        assert!(!path_is_workflow_yaml(&root.join(".animus/state.json")));
        // A `workflows.yaml` outside the .animus dir must NOT match.
        assert!(!path_is_workflow_yaml(&root.join("workflows.yaml")));
    }

    #[test]
    fn outcome_json_envelope_success() {
        let out = WorkflowConfigReloadOutcome {
            reloaded: true,
            phase_definitions: 3,
            workflows: 2,
            agent_profiles: 1,
            source_files: vec![PathBuf::from("/tmp/.animus/workflows.yaml")],
            config_hash: Some("abc".to_string()),
            errors: Vec::new(),
        };
        let value = out.to_json();
        assert_eq!(value["reloaded"], json!(true));
        assert_eq!(value["phase_definitions"], json!(3));
        assert_eq!(value["workflows"], json!(2));
        assert_eq!(value["config_hash"], json!("abc"));
    }

    #[test]
    fn reload_with_removed_overlay_clears_prior_snapshot() {
        let dir = tempdir().unwrap();
        write_minimal_overlay(dir.path(), "alpha");
        let snap = WorkflowConfigSnapshot::new();
        let first = reload_workflow_config_once(dir.path(), &snap, None);
        assert!(first.reloaded);
        assert!(snap.current().is_some());

        // Remove all overlays. A reload should clear the snapshot rather
        // than keep stale workflow definitions exposed.
        fs::remove_file(dir.path().join(".animus").join("workflows.yaml")).unwrap();
        let second = reload_workflow_config_once(dir.path(), &snap, None);
        assert!(!second.reloaded, "no overlay must not report reloaded=true");
        assert!(snap.current().is_none(), "snapshot must be cleared when the operator removes every overlay");
    }

    #[test]
    fn outcome_json_envelope_failure() {
        let out = WorkflowConfigReloadOutcome {
            reloaded: false,
            phase_definitions: 0,
            workflows: 0,
            agent_profiles: 0,
            source_files: Vec::new(),
            config_hash: None,
            errors: vec!["expected `:`, found `[`".to_string()],
        };
        let value = out.to_json();
        assert_eq!(value["reloaded"], json!(false));
        let errors = value["errors"].as_array().expect("errors array");
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0]["message"], json!("expected `:`, found `[`"));
    }

    #[tokio::test]
    async fn watcher_triggers_reload_on_yaml_write() {
        let dir = tempdir().unwrap();
        write_minimal_overlay(dir.path(), "alpha");
        let snap = WorkflowConfigSnapshot::new();
        // Prime the snapshot so we can assert the change-detection path.
        let initial = reload_workflow_config_once(dir.path(), &snap, None);
        assert!(initial.reloaded);
        let initial_arc = snap.current().expect("initial snapshot present");

        let bus = WorkflowEventBroadcaster::new();
        let (_sub_id, mut rx) = bus.subscribe(crate::control::WorkflowEventFilter {
            workflow_id: None,
            kinds: Some(vec![RELOAD_EVENT_KIND.to_string()]),
        });

        let handle = spawn_workflow_config_watcher(
            dir.path().to_path_buf(),
            snap.clone(),
            bus.clone(),
            Duration::from_millis(150),
        )
        .expect("watcher spawn must succeed");

        // Brief settle so the watcher registers before we write.
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Modify the overlay — append a second phase so the compiled
        // config changes shape.
        let yaml = "phases:\n  alpha:\n    mode: agent\n    agent_id: hot-reload-agent\n  beta:\n    mode: agent\n    agent_id: hot-reload-agent\nagents:\n  hot-reload-agent:\n    description: hot-reload fixture\n    system_prompt: hot-reload prompt\n    skills: []\nworkflows:\n  - id: hot-reload-workflow\n    name: Hot Reload\n    phases:\n      - alpha\n      - beta\n";
        std::fs::write(dir.path().join(".animus").join("workflows.yaml"), yaml).unwrap();

        // Wait up to 4 seconds for the broadcast event.
        let event = tokio::time::timeout(Duration::from_secs(4), rx.recv()).await;
        let received = event.expect("watcher must emit a reload event within timeout").expect("event present");
        match received {
            crate::control::SubscriberItem::Event(e) => {
                assert_eq!(e.kind, RELOAD_EVENT_KIND);
                assert_eq!(e.workflow_id, DAEMON_PSEUDO_WORKFLOW_ID);
                let phase_definitions = e.payload.get("phase_definitions").and_then(|v| v.as_u64()).unwrap_or(0);
                assert!(phase_definitions >= 2, "expected the new phase to be reflected in the broadcast payload");
            }
            crate::control::SubscriberItem::Closed { reason } => panic!("expected Event, got Closed({reason})"),
        }

        let after = snap.current().expect("snapshot present after reload");
        assert!(!Arc::ptr_eq(&after, &initial_arc), "snapshot must have been swapped to a new Arc");

        handle.shutdown().await;
    }

    #[tokio::test]
    async fn watcher_debounces_rapid_writes_into_one_reload() {
        let dir = tempdir().unwrap();
        write_minimal_overlay(dir.path(), "alpha");
        let snap = WorkflowConfigSnapshot::new();

        let bus = WorkflowEventBroadcaster::new();
        let (_sub_id, mut rx) = bus.subscribe(crate::control::WorkflowEventFilter {
            workflow_id: None,
            kinds: Some(vec![RELOAD_EVENT_KIND.to_string()]),
        });

        let handle = spawn_workflow_config_watcher(
            dir.path().to_path_buf(),
            snap.clone(),
            bus.clone(),
            Duration::from_millis(400),
        )
        .expect("watcher spawn must succeed");

        tokio::time::sleep(Duration::from_millis(100)).await;

        // Three rapid writes within the debounce window.
        for n in 0..3u32 {
            let yaml = format!(
                "phases:\n  alpha:\n    mode: agent\n    agent_id: hot-reload-agent\nagents:\n  hot-reload-agent:\n    description: rapid-{n}\n    system_prompt: rapid prompt\n    skills: []\nworkflows:\n  - id: hot-reload-workflow\n    name: Hot Reload\n    phases:\n      - alpha\n"
            );
            std::fs::write(dir.path().join(".animus").join("workflows.yaml"), yaml).unwrap();
            tokio::time::sleep(Duration::from_millis(50)).await;
        }

        // First reload event should arrive after the debounce window settles.
        let first = tokio::time::timeout(Duration::from_secs(4), rx.recv()).await;
        assert!(first.is_ok(), "at least one reload event must arrive");

        // Now drain for a short window to confirm we got at most one reload
        // event for the burst (FSEvents may emit a second batched event,
        // but never three).
        let mut additional = 0usize;
        let drain_deadline = tokio::time::Instant::now() + Duration::from_millis(800);
        while tokio::time::Instant::now() < drain_deadline {
            match tokio::time::timeout(Duration::from_millis(200), rx.recv()).await {
                Ok(Some(crate::control::SubscriberItem::Event(_))) => additional += 1,
                Ok(_) | Err(_) => break,
            }
        }
        assert!(
            additional <= 1,
            "debounce should coalesce three rapid writes into at most two total events (saw {} additional)",
            additional
        );

        handle.shutdown().await;
    }
}
