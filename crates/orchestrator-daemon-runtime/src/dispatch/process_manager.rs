use std::collections::HashSet;
#[cfg(unix)]
use std::path::Path;
#[cfg(unix)]
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use protocol::orchestrator::WorkflowStatus;
use protocol::SubjectDispatch;
use tokio::process::{Child, Command};
use tokio::task::JoinHandle;

use super::{build_runner_command_with_id, build_runner_command_with_resume};
#[cfg(unix)]
use crate::control::WorkflowEventBroadcaster;
use crate::dispatch::environment_broker::{
    is_local_environment, EnvironmentBroker, ANIMUS_ENVIRONMENT_BROKER_ENVIRONMENT_ID_ENV,
    ANIMUS_ENVIRONMENT_BROKER_RUN_ID_ENV, ANIMUS_ENVIRONMENT_BROKER_SOCKET_ENV, ANIMUS_ENVIRONMENT_BROKER_TOKEN_ENV,
};
#[cfg(unix)]
use crate::dispatch::event_pipe::SubprocessEventPipe;
use crate::{CompletedProcess, RunnerEvent};

#[cfg(unix)]
#[allow(unsafe_code)]
fn set_session_id_on_spawn(cmd: &mut Command) {
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

pub const ANIMUS_AGENT_RUN_ID_ENV: &str = "ANIMUS_AGENT_RUN_ID";

/// Env-var allowlist consulted by the workflow-runner subprocess for
/// keychain-backed secret injection. The runner is not a plugin and
/// therefore has no manifest; instead the daemon ships a fixed set of
/// well-known credential variables that historically belong on the
/// runner's environment. Operators may extend the list at runtime via
/// the comma-separated [`RUNNER_SECRET_ALLOWLIST_ENV`] override.
/// (codex round-5 P1.)
pub const RUNNER_SECRET_ALLOWLIST_DEFAULT: &[&str] = &[
    "ANTHROPIC_API_KEY",
    "OPENAI_API_KEY",
    "GEMINI_API_KEY",
    "GOOGLE_API_KEY",
    "GOOGLE_APPLICATION_CREDENTIALS",
    "LINEAR_API_TOKEN",
    "GITHUB_TOKEN",
    "GH_TOKEN",
    "SLACK_BOT_TOKEN",
    "SLACK_SIGNING_SECRET",
];

/// Comma-separated env var name; values listed here are appended to
/// [`RUNNER_SECRET_ALLOWLIST_DEFAULT`] for the workflow-runner spawn
/// path.
pub const RUNNER_SECRET_ALLOWLIST_ENV: &str = "ANIMUS_RUNNER_SECRET_ALLOWLIST";

/// Env keys the daemon manages itself on the workflow-runner spawn —
/// never replace these from the keychain even if a user stores a
/// matching key. (codex round-5 P2.)
const DAEMON_MANAGED_RUNNER_ENV_KEYS: &[&str] = &[
    ANIMUS_AGENT_RUN_ID_ENV,
    "ANIMUS_WORKFLOW_REATTACH_SOCKET",
    "ANIMUS_WORKFLOW_EVENT_PIPE",
    animus_runtime_shared::phase_skills::ANIMUS_PHASE_SKILLS_ENV,
    ANIMUS_ENVIRONMENT_BROKER_SOCKET_ENV,
    ANIMUS_ENVIRONMENT_BROKER_TOKEN_ENV,
    ANIMUS_ENVIRONMENT_BROKER_RUN_ID_ENV,
    ANIMUS_ENVIRONMENT_BROKER_ENVIRONMENT_ID_ENV,
];

/// Upper bound on the serialized phase-skills payload the daemon will put on
/// the runner spawn environment. Oversized payloads are dropped with a
/// warning; the runner then resolves skills locally (its no-payload
/// fallback), so nothing is lost beyond the daemon-side resolution log.
/// Windows caps the entire process environment block at ~32 KiB, so the cap
/// there must leave headroom for the inherited daemon environment; Unix
/// limits (ARG_MAX-style, typically >= 1 MiB shared) allow a roomier cap.
/// (codex P2.)
#[cfg(windows)]
const PHASE_SKILLS_ENV_MAX_BYTES: usize = 16 * 1024;
#[cfg(not(windows))]
const PHASE_SKILLS_ENV_MAX_BYTES: usize = 512 * 1024;

/// Resolve the project's phase/profile skill declarations (same scoped
/// sources and trust rules as the ad-hoc `--skill` path) and serialize the
/// result for the runner spawn env. Returns `None` when no phase declares
/// skills. Missing skill names have already been logged by the resolver;
/// they ride along in the payload's `missing` lists so the runner records
/// them on phase metadata.
fn workflow_skills_env_payload(project_root: &str) -> Option<String> {
    let payload = animus_runtime_shared::phase_skills::resolve_workflow_skills_payload(project_root);
    if payload.phases.is_empty() {
        return None;
    }
    match serde_json::to_string(&payload) {
        Ok(json) if json.len() > PHASE_SKILLS_ENV_MAX_BYTES => {
            tracing::warn!(
                bytes = json.len(),
                "phase-skills payload exceeds env size cap; runner falls back to local skill resolution"
            );
            None
        }
        Ok(json) => Some(json),
        Err(error) => {
            tracing::warn!(%error, "failed to serialize phase-skills payload; runner falls back to local skill resolution");
            None
        }
    }
}

fn runner_secret_allowlist() -> Vec<String> {
    let mut out: Vec<String> = RUNNER_SECRET_ALLOWLIST_DEFAULT.iter().map(|s| (*s).to_string()).collect();
    if let Ok(extra) = std::env::var(RUNNER_SECRET_ALLOWLIST_ENV) {
        for raw in extra.split(',') {
            let name = raw.trim();
            if !name.is_empty() && !out.iter().any(|n| n == name) {
                out.push(name.to_string());
            }
        }
    }
    out
}

/// Recoverable spawn rejection: the workflow concurrency cap is reached.
/// Callers must NOT treat the dispatch as poisoned — leave the entry queued
/// (or release it back to pending) so the next tick retries it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkflowConcurrencyCapReached {
    pub active: usize,
    pub cap: usize,
}

impl std::fmt::Display for WorkflowConcurrencyCapReached {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "workflow concurrency cap reached ({} active, max {}); leaving entry queued for next tick",
            self.active, self.cap
        )
    }
}

impl std::error::Error for WorkflowConcurrencyCapReached {}

struct WorkflowProcess {
    subject_key: String,
    subject_id: String,
    subject_kind: String,
    task_id: Option<String>,
    workflow_ref: String,
    schedule_id: Option<String>,
    started_at: std::time::Instant,
    child: Arc<Mutex<Child>>,
    stderr_lines: Arc<Mutex<Vec<String>>>,
    stderr_reader: Option<JoinHandle<()>>,
    /// Per-spawn workflow_events back-channel. Dropped after the
    /// subprocess is reaped so the socket file is cleaned up.
    #[cfg(unix)]
    #[allow(dead_code)]
    event_pipe: Option<SubprocessEventPipe>,
    agent_session_id: Option<String>,
    project_root: Option<std::path::PathBuf>,
    /// Kernel-selected workflow id for either idempotent fresh creation or
    /// journal resume. Used as the completion fallback when the runner exits
    /// before emitting an event, keeping queue and journal reconciliation on
    /// the same identity.
    target_workflow_id: Option<String>,
    /// REQ-048 cross-phase environment broker: the broker `run_id` this spawn
    /// was bound to (the workflow run id), set ONLY when the dispatch routes to
    /// a non-local environment and a broker is wired. The daemon tears the run's
    /// shared node down by this id once the workflow reaches a terminal state.
    environment_run_id: Option<String>,
}

pub struct ProcessManager {
    processes: Vec<WorkflowProcess>,
    process_timeout_secs: Option<u64>,
    pub phase_routing: Option<protocol::PhaseRoutingConfig>,
    pub mcp_config: Option<protocol::McpRuntimeConfig>,
    /// Broadcaster that subprocess back-channel readers forward into.
    /// `None` means subprocess workflow_events fan-out is disabled and the
    /// spawn path falls back to setting no env var (runner uses the noop
    /// emitter). Wired by the daemon at startup via
    /// [`Self::with_event_broadcaster`].
    #[cfg(unix)]
    event_broadcaster: Option<Arc<WorkflowEventBroadcaster>>,
    /// Root directory under which per-spawn event-pipe socket files live.
    #[cfg(unix)]
    pipe_root: Option<PathBuf>,
    /// Cap on the number of concurrently-running runner subprocesses. New
    /// spawn requests beyond this point are rejected; the dispatcher then
    /// leaves the entry in the ready queue for the next tick.
    workflow_concurrency_max: Option<usize>,
    /// REQ-048: config-level environment routing (kind/harness rules + default).
    /// Drives the daemon-side decision of whether a dispatch routes to a
    /// non-local environment (and thus through the [`EnvironmentBroker`]).
    pub environment_routing: Option<animus_config_protocol::workflow_types::EnvironmentRouting>,
    /// Per-workflow `environment:` overrides (workflow id -> environment plugin
    /// id, lowercased keys). The broker gate resolves the dispatch's workflow to
    /// its `environment:` and feeds it to `resolve_environment` as `workflow_env`
    /// — without this a workflow-level environment (the only environment config
    /// on many deployments) was invisible to the daemon, so the broker never
    /// engaged and each phase prepared its OWN node. See TASK-431 / REQ-051.
    pub workflow_environments: std::collections::HashMap<String, String>,
    /// REQ-048: the cross-phase ephemeral-environment broker. When wired AND the
    /// dispatch routes to a non-local environment, the daemon sets the four
    /// `ANIMUS_ENVIRONMENT_BROKER_*` env vars on the runner so it acquires the
    /// run's shared node from the daemon instead of preparing its own.
    environment_broker: Option<EnvironmentBroker>,
}

impl Default for ProcessManager {
    fn default() -> Self {
        Self::new()
    }
}

impl ProcessManager {
    pub fn new() -> Self {
        // The workflow concurrency cap is sourced from `RuntimeQuotas`
        // (which reads `ANIMUS_WORKFLOW_CONCURRENCY_MAX` once at install
        // time, with a documented default of 10). When the env var is
        // explicitly set, the quota struct still honors it — this keeps
        // the operator escape hatch working while ensuring the documented
        // default actually applies even when no env var is present.
        //
        // The subprocess workflow_events broadcaster is NOT looked up
        // here — it is picked up lazily on each spawn via
        // [`crate::daemon::current_workflow_event_broadcaster`] so that
        // a `ProcessManager` constructed before `run_daemon` installs
        // the broadcaster (the normal CLI startup sequence) still
        // attaches per-run pipes once the daemon is live.
        let workflow_concurrency_max = Some(crate::quotas::runtime_quotas().workflow_concurrency_max);
        Self {
            processes: Vec::new(),
            process_timeout_secs: None,
            phase_routing: None,
            mcp_config: None,
            #[cfg(unix)]
            event_broadcaster: None,
            #[cfg(unix)]
            pipe_root: None,
            workflow_concurrency_max,
            environment_routing: None,
            workflow_environments: std::collections::HashMap::new(),
            environment_broker: None,
        }
    }

    pub fn with_timeout(mut self, timeout_secs: Option<u64>) -> Self {
        self.process_timeout_secs = timeout_secs;
        self
    }

    /// Wire the subprocess workflow_events back-channel into this
    /// `ProcessManager`. Every spawn will allocate a per-run Unix
    /// domain socket under `pipe_root` (created if missing), advertise it
    /// to the runner via `ANIMUS_WORKFLOW_EVENT_PIPE`, and start a reader
    /// task that forwards events into `broadcaster`.
    #[cfg(unix)]
    pub fn with_event_broadcaster(mut self, broadcaster: Arc<WorkflowEventBroadcaster>, pipe_root: PathBuf) -> Self {
        self.event_broadcaster = Some(broadcaster);
        self.pipe_root = Some(pipe_root);
        self
    }

    /// Override the cap on the number of concurrently-running runner
    /// subprocesses. `Some(n)` pins the cap at `n`; `None` disables the
    /// cap entirely (unbounded — for tests / specialty deployments that
    /// rely on external scheduling). When the cap is reached,
    /// [`Self::spawn_workflow_runner`] returns a recoverable error and
    /// the caller leaves the entry in the dispatch queue for the next
    /// tick.
    ///
    /// Note: the default cap (from `ProcessManager::new()`) is already
    /// seeded from `RuntimeQuotas::workflow_concurrency_max`, so this
    /// setter is only needed when overriding for tests or specialty
    /// dispatchers.
    pub fn with_workflow_concurrency_max(mut self, max: Option<usize>) -> Self {
        self.workflow_concurrency_max = max;
        self
    }

    /// REQ-048: wire the cross-phase ephemeral-environment broker. Once set,
    /// [`Self::spawn_workflow_runner`] routes dispatches bound to a non-local
    /// environment through the broker (four `ANIMUS_ENVIRONMENT_BROKER_*` env
    /// vars on the runner), and [`Self::check_running`] tears the run's shared
    /// node down when the workflow reaches a terminal state.
    pub fn with_environment_broker(mut self, broker: EnvironmentBroker) -> Self {
        self.environment_broker = Some(broker);
        self
    }

    pub fn spawn_workflow_runner(&mut self, dispatch: &SubjectDispatch, project_root: &str) -> Result<()> {
        self.spawn_workflow_runner_inner(dispatch, project_root, None, None)
    }

    /// Start a fresh queue-backed workflow using the kernel-selected durable id.
    /// The runner creates the journal row idempotently with this exact value.
    pub fn spawn_workflow_runner_with_id(
        &mut self,
        dispatch: &SubjectDispatch,
        project_root: &str,
        workflow_id: &str,
    ) -> Result<()> {
        self.spawn_workflow_runner_inner(dispatch, project_root, Some(workflow_id), None)
    }

    /// BU-4 journal-resume re-dispatch: spawn a runner that CONTINUES the
    /// EXISTING persisted run `resume_workflow_id` from its `current_phase`
    /// (via `execute --workflow-id`), rather than starting a fresh workflow for
    /// the subject. Same spawn bookkeeping (concurrency cap, agent record,
    /// event pipe, reattach socket) as [`Self::spawn_workflow_runner`].
    pub fn spawn_workflow_runner_resume(
        &mut self,
        dispatch: &SubjectDispatch,
        project_root: &str,
        resume_workflow_id: &str,
    ) -> Result<()> {
        self.spawn_workflow_runner_inner(dispatch, project_root, None, Some(resume_workflow_id))
    }

    fn spawn_workflow_runner_inner(
        &mut self,
        dispatch: &SubjectDispatch,
        project_root: &str,
        new_workflow_id: Option<&str>,
        resume_workflow_id: Option<&str>,
    ) -> Result<()> {
        if let Some(cap) = self.workflow_concurrency_max {
            if self.processes.len() >= cap {
                return Err(anyhow::Error::new(WorkflowConcurrencyCapReached { active: self.processes.len(), cap }));
            }
        }

        debug_assert!(new_workflow_id.is_none() || resume_workflow_id.is_none());
        let target_workflow_id = new_workflow_id.or(resume_workflow_id);
        let std_cmd = match new_workflow_id {
            Some(workflow_id) => build_runner_command_with_id(
                dispatch,
                project_root,
                self.phase_routing.as_ref(),
                self.mcp_config.as_ref(),
                workflow_id,
            ),
            None => build_runner_command_with_resume(
                dispatch,
                project_root,
                self.phase_routing.as_ref(),
                self.mcp_config.as_ref(),
                resume_workflow_id,
            ),
        };
        let command_line: Vec<String> = std::iter::once(std_cmd.get_program().to_string_lossy().into_owned())
            .chain(std_cmd.get_args().map(|a| a.to_string_lossy().into_owned()))
            .collect();
        let mut command = Command::from(std_cmd);
        command.stdout(Stdio::null()).stderr(Stdio::piped());

        // v0.5.1 P2 #6.2: pre-allocate the agent session id BEFORE spawn so
        // we can wire the reattach-socket path the runner will bind into
        // the spawn env. Keep the id SHORT (`agent-<8-hex-uuid>`) so the
        // resulting socket path fits within SUN_LEN (~100 bytes on macOS,
        // ~108 on Linux) even when scoped state lives under a deep home
        // path. We carry the dispatch subject id in the spawn record for
        // human-readable correlation; the on-disk id stays compact.
        let project_root_path = std::path::Path::new(project_root).to_path_buf();
        let short_uuid = uuid::Uuid::new_v4().simple().to_string()[..8].to_string();
        let pending_session_id = format!("agent-{short_uuid}");
        // v0.5.1 fold-in (item 2): advertise the daemon-chosen run_id to the
        // runner so its `decisions.jsonl` lands under a path the daemon can
        // predict from the spawn record alone. The runner is expected to
        // honor `ANIMUS_AGENT_RUN_ID` (plugin-side change in
        // `animus-workflow-runner-default`); older runners that ignore it
        // still write under a self-chosen run_id, in which case
        // `replay_gap_from_spawn_record` falls back to scanning runs/ for
        // the most recent decisions.jsonl matching the spawn time.
        command.env(ANIMUS_AGENT_RUN_ID_ENV, &pending_session_id);
        // Bound the workflow-runner's tokio pool for the same reason we cap plugins
        // (orchestrator-plugin-host host.rs): a bare `#[tokio::main]` otherwise sizes
        // the worker pool to all CPU cores, compounding the PID/thread pressure. This
        // path inherits the daemon env, so only impose the default when unset.
        if std::env::var_os("TOKIO_WORKER_THREADS").is_none() {
            command.env("TOKIO_WORKER_THREADS", "2");
        }
        // Phase skills pass-down: resolve the union of phase-level `skills:`
        // and the executing agent profile's `skills:` daemon-side (scoped
        // sources + trust stripping, identical to the ad-hoc `--skill`
        // path) and ship the resolved definitions on the spawn env. Older
        // runners ignore the env var; newer runners apply activation gating
        // at phase execution where the selected tool/model is known, and
        // fall back to resolving locally when the var is absent.
        // Clear any inherited value first: `Command` inherits the daemon's
        // environment, so a stale parent-process payload must never reach
        // the runner when this dispatch produces none. (codex P2.)
        command.env_remove(animus_runtime_shared::phase_skills::ANIMUS_PHASE_SKILLS_ENV);
        if let Some(skills_json) = workflow_skills_env_payload(project_root) {
            command.env(animus_runtime_shared::phase_skills::ANIMUS_PHASE_SKILLS_ENV, skills_json);
        }
        // v0.5.8 secrets: inject keychain entries into the runner env so
        // workflow runs see the same secret values as
        // `PluginHost::spawn_with_options` would. The runner is a
        // subprocess that we do NOT have a manifest allowlist for, so
        // the set of keys we expose is gated by a fixed allowlist plus
        // the operator override `ANIMUS_RUNNER_SECRET_ALLOWLIST`. We
        // never expose the daemon-managed env keys (run id, reattach
        // socket, ...), and parent env / daemon-set values win on
        // collision so explicit overrides keep working.
        // (codex round-3 P1, round-4 P2, round-5 P1+P2.)
        if let Some(provider) = orchestrator_plugin_host::current_secret_snapshot_provider() {
            // Pre-filter the requested allowlist so the keychain isn't
            // touched for keys the parent env or daemon already provides;
            // matches the plugin-host precedence rules. (codex round-7 P2.)
            let requested: Vec<String> = runner_secret_allowlist()
                .into_iter()
                .filter(|name| !DAEMON_MANAGED_RUNNER_ENV_KEYS.contains(&name.as_str()))
                .filter(|name| std::env::var_os(name).is_none())
                .collect();
            let snapshot = if requested.is_empty() {
                std::collections::BTreeMap::new()
            } else {
                provider.snapshot_filtered(&requested)
            };
            let mut total: usize = 0;
            for (key, value) in snapshot {
                // Defensive: should be a no-op since we pre-filtered, but
                // covers the case where a provider returns extra keys.
                if DAEMON_MANAGED_RUNNER_ENV_KEYS.iter().any(|managed| **managed == key) {
                    continue;
                }
                if std::env::var_os(&key).is_some() {
                    continue;
                }
                let next = total.saturating_add(key.len()).saturating_add(value.len());
                if next > orchestrator_plugin_host::MAX_INJECTED_SECRET_BYTES {
                    tracing::warn!(
                        runner = %pending_session_id,
                        skipped_key = %key,
                        "workflow-runner secret entry skipped: would exceed cumulative cap"
                    );
                    continue;
                }
                command.env(&key, value);
                total = next;
            }
        }
        #[cfg(unix)]
        let reattach_socket_path = reattach_socket_path_for(&project_root_path, &pending_session_id);
        #[cfg(unix)]
        if let Some(path) = reattach_socket_path.as_ref() {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            command.env(animus_runtime_shared::reattach::ANIMUS_WORKFLOW_REATTACH_SOCKET_ENV, path.as_os_str());
        }

        // REQ-048 cross-phase environment broker: when this dispatch routes to a
        // non-local environment and a broker is wired, register the run and set
        // the four `ANIMUS_ENVIRONMENT_BROKER_*` env vars so the runner acquires
        // the run's SHARED node from the daemon instead of preparing its own.
        // Returns the broker run_id so the completion path can tear it down.
        let environment_run_id =
            self.configure_environment_broker(dispatch, project_root, target_workflow_id, &mut command);

        // Bind the subprocess workflow_events back-channel before fork so the
        // env var we set on the child points to a listener that's already
        // accepting. Best-effort: if bind fails (eg no Unix DS support in a
        // sandbox) we still spawn without the back-channel and the runner
        // falls back to its noop emitter.
        #[cfg(unix)]
        let event_pipe = self.bind_event_pipe_for(dispatch, &mut command);

        // Required for daemon-restart-survivable runner; #6.2 v0.5.1.
        #[cfg(unix)]
        set_session_id_on_spawn(&mut command);

        if let Ok(host_cli) = std::env::current_exe() {
            command.env("ANIMUS_HOST_CLI_PATH", host_cli);
        }

        let mut child = command.spawn().context("failed to spawn animus-workflow-runner")?;

        let stderr_lines: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let stderr_reader = if let Some(stderr) = child.stderr.take() {
            let lines = stderr_lines.clone();
            Some(tokio::spawn(async move {
                use tokio::io::{AsyncBufReadExt, BufReader};
                let reader = BufReader::new(stderr);
                let mut line_stream = reader.lines();
                while let Ok(Some(line)) = line_stream.next_line().await {
                    if let Ok(mut buf) = lines.lock() {
                        buf.push(line);
                    }
                }
            }))
        } else {
            None
        };

        let task_id = dispatch.task_id().map(String::from);
        let workflow_ref = dispatch.workflow_ref.clone();
        let schedule_id = dispatch.schedule_id().map(String::from);

        let pid = child.id();
        let agent_session_id = pid.map(|pid_value| {
            let id = pending_session_id.clone();
            #[cfg(unix)]
            let socket_for_record = reattach_socket_path.as_ref().map(|p| p.display().to_string());
            #[cfg(not(unix))]
            let socket_for_record: Option<String> = None;
            let decisions_for_record =
                animus_runtime_shared::recording::decision_log_path(project_root, &id).map(|p| p.display().to_string());
            let record = super::agent_record::build_record_with_decisions(
                id.clone(),
                pid_value,
                dispatch,
                command_line.clone(),
                socket_for_record,
                decisions_for_record,
            );
            if let Err(error) = super::agent_record::write_record(&project_root_path, &record) {
                tracing::warn!(
                    target: "animus.runtime.agent_record",
                    %error,
                    agent_session_id = %id,
                    "failed to write agent spawn record (best-effort; v0.5.1 reattach scaffolding)"
                );
            }
            id
        });

        self.processes.push(WorkflowProcess {
            subject_key: dispatch.subject_key().unwrap_or_default(),
            subject_id: dispatch.subject_id().unwrap_or_default().to_string(),
            subject_kind: dispatch.subject_kind().unwrap_or_default().to_string(),
            task_id,
            workflow_ref,
            schedule_id,
            started_at: std::time::Instant::now(),
            child: Arc::new(Mutex::new(child)),
            stderr_lines,
            stderr_reader,
            #[cfg(unix)]
            event_pipe,
            agent_session_id,
            project_root: Some(project_root_path),
            target_workflow_id: target_workflow_id.map(String::from),
            environment_run_id,
        });

        Ok(())
    }

    /// REQ-048: decide whether this dispatch routes to a non-local environment
    /// and, if so, register the run with the broker + set the four
    /// `ANIMUS_ENVIRONMENT_BROKER_*` env vars on the runner command. Returns the
    /// broker `run_id` (the workflow run id) when brokered, else `None` (the
    /// runner then uses its legacy owned-environment path).
    ///
    /// The run_id must be STABLE across every phase. Queue-backed dispatch now
    /// supplies the authoritative workflow id before spawn, so use it first;
    /// legacy ad-hoc callers fall back to the subject or a fresh local id.
    fn configure_environment_broker(
        &self,
        dispatch: &SubjectDispatch,
        project_root: &str,
        target_workflow_id: Option<&str>,
        command: &mut Command,
    ) -> Option<String> {
        let broker = self.environment_broker.as_ref()?;
        // The daemon dispatches per phase and cannot know a PHASE-level
        // `environment:` override here, but the node is per-RUN anyway, so a
        // single run-level environment is the correct granularity. Feed the
        // dispatch's WORKFLOW-level `environment:` as `workflow_env` (the runner
        // does the same when it resolves each phase) so a workflow-level
        // environment engages the broker even with no kind-level routing rule —
        // otherwise the broker never fired and every phase owned its own node.
        // See TASK-431 / REQ-051.
        let workflow_ref = dispatch.workflow_ref.trim();
        let workflow_env = if workflow_ref.is_empty() {
            None
        } else {
            self.workflow_environments.get(&workflow_ref.to_ascii_lowercase()).map(String::as_str)
        };
        let environment_id = orchestrator_config::workflow_config::resolve_environment(
            dispatch.subject_kind(),
            None,
            None,
            workflow_env,
            self.environment_routing.as_ref(),
        )?;
        if is_local_environment(&environment_id) {
            return None;
        }
        let run_id = target_workflow_id
            .map(str::to_string)
            .or_else(|| {
                dispatch.subject_id().filter(|subject| !subject.is_empty()).map(|subject| format!("run-{subject}"))
            })
            .unwrap_or_else(|| format!("wf-{}", uuid::Uuid::new_v4().simple()));
        broker.register_run(&run_id, project_root, &environment_id);
        command.env(ANIMUS_ENVIRONMENT_BROKER_SOCKET_ENV, broker.socket_path());
        command.env(ANIMUS_ENVIRONMENT_BROKER_TOKEN_ENV, broker.token());
        command.env(ANIMUS_ENVIRONMENT_BROKER_RUN_ID_ENV, &run_id);
        command.env(ANIMUS_ENVIRONMENT_BROKER_ENVIRONMENT_ID_ENV, &environment_id);
        Some(run_id)
    }

    /// Bind a fresh per-spawn event pipe and attach the
    /// `ANIMUS_WORKFLOW_EVENT_PIPE` env var to `command` so the runner can
    /// connect. Returns `None` when the back-channel isn't configured on
    /// this `ProcessManager` (eg tests, or daemons that opted out) or when
    /// `bind` fails on the host filesystem. Failure is best-effort: we
    /// proceed with the spawn so the workflow still runs; only the
    /// fan-out is silently disabled for that run.
    #[cfg(unix)]
    fn bind_event_pipe_for(&self, dispatch: &SubjectDispatch, command: &mut Command) -> Option<SubprocessEventPipe> {
        // Lazy lookup so the broadcaster is picked up even when the
        // `ProcessManager` was constructed BEFORE [`crate::run_daemon`]
        // installed it (the production daemon spawns the manager first
        // and then starts the daemon loop). Explicit per-instance wiring
        // via [`Self::with_event_broadcaster`] still wins so tests can
        // pin a specific broadcaster.
        let broadcaster = match self.event_broadcaster.as_ref() {
            Some(bus) => bus.clone(),
            None => crate::daemon::current_workflow_event_broadcaster()?,
        };
        let pipe_root = match self.pipe_root.as_ref() {
            Some(root) => root.clone(),
            None => default_event_pipe_root(),
        };
        let subject_label = dispatch.subject_id().unwrap_or_default().to_string();
        // Bind synchronously on the calling thread (just a couple of
        // syscalls) and let `SubprocessEventPipe::bind_sync` spawn the
        // reader task on the current Tokio runtime. This avoids the
        // previous pattern of spawning a bind helper task and blocking
        // on a channel for its result, which could deadlock on a
        // current-thread runtime and stall an executor worker on
        // multi-thread runtimes.
        //
        // Requires a current Tokio runtime (the reader task needs a home);
        // returning `None` when none is present preserves legacy
        // best-effort semantics — the workflow still spawns, only the
        // fan-out is dropped for that run.
        if tokio::runtime::Handle::try_current().is_err() {
            return None;
        }
        let pipe = match SubprocessEventPipe::bind_sync(&pipe_root, &subject_label, broadcaster) {
            Ok(pipe) => Some(pipe),
            Err(error) => {
                tracing::warn!(
                    target: "animus.runtime.event_pipe",
                    %error,
                    "failed to bind workflow event pipe; subprocess events will be dropped"
                );
                None
            }
        };
        if let Some(ref pipe) = pipe {
            command.env(SubprocessEventPipe::env_var(), pipe.socket_path());
        }
        pipe
    }

    pub async fn check_running(&mut self) -> Vec<CompletedProcess> {
        let timeout = self.process_timeout_secs;
        self.check_running_with_timeout(timeout).await
    }

    async fn check_running_with_timeout(&mut self, timeout_secs: Option<u64>) -> Vec<CompletedProcess> {
        let mut completed = Vec::new();
        let mut active = Vec::with_capacity(self.processes.len());
        // Cloned out of `self` up front so the terminal-teardown calls below do
        // not conflict with the `&mut self.processes` drain borrow.
        let environment_broker = self.environment_broker.clone();

        for mut process in self.processes.drain(..) {
            if let Some(timeout) = timeout_secs {
                if process.started_at.elapsed().as_secs() > timeout {
                    let pid = process.child.lock().ok().and_then(|c| c.id());
                    if let Some(pid) = pid {
                        // graceful_kill_process sleeps up to ~5.2s; run it on
                        // the blocking pool so it cannot pin an async worker.
                        let _ = tokio::task::spawn_blocking(move || protocol::graceful_kill_process(pid as i32)).await;
                    }
                    drain_stderr_reader(&mut process.stderr_reader).await;
                    // Drain the event pipe before the WorkflowProcess is
                    // dropped: otherwise Drop just aborts the reader and the
                    // last batch of `workflow_events` the subprocess
                    // emitted right before timeout-kill is lost.
                    #[cfg(unix)]
                    drain_event_pipe(&mut process.event_pipe).await;
                    cleanup_agent_record(&process);
                    // Timeout kill is a terminal Failed outcome: dispose the
                    // run's shared node (if brokered) before reaping the record.
                    teardown_environment_if_terminal(
                        &environment_broker,
                        process.environment_run_id.take(),
                        Some(WorkflowStatus::Failed),
                    )
                    .await;
                    completed.push(CompletedProcess {
                        subject_id: process.subject_key,
                        subject_kind: Some(process.subject_kind),
                        task_id: process.task_id,
                        workflow_id: process.target_workflow_id.take(),
                        workflow_ref: Some(process.workflow_ref),
                        workflow_status: Some(WorkflowStatus::Failed),
                        schedule_id: process.schedule_id,
                        exit_code: None,
                        success: false,
                        failure_reason: Some(format!("workflow runner exceeded timeout of {} seconds", timeout)),
                        events: parse_runner_events(&process.stderr_lines),
                    });
                    continue;
                }
            }
            let status = {
                let mut maybe_child = match process.child.lock() {
                    Ok(guard) => guard,
                    Err(error) => {
                        #[cfg(unix)]
                        drain_event_pipe(&mut process.event_pipe).await;
                        cleanup_agent_record(&process);
                        completed.push(CompletedProcess {
                            subject_id: process.subject_key,
                            subject_kind: Some(process.subject_kind),
                            task_id: process.task_id,
                            workflow_id: process.target_workflow_id.take(),
                            workflow_ref: Some(process.workflow_ref),
                            workflow_status: None,
                            schedule_id: process.schedule_id,
                            exit_code: None,
                            success: false,
                            failure_reason: Some(format!("failed to lock workflow process handle: {}", error)),
                            events: Vec::new(),
                        });
                        continue;
                    }
                };

                maybe_child.try_wait()
            };

            match status {
                Ok(Some(status)) => {
                    drain_stderr_reader(&mut process.stderr_reader).await;
                    // Normal-lifecycle drain: take + await
                    // `event_pipe.shutdown()` before the WorkflowProcess is
                    // dropped. Pre-fix the Drop path aborted the reader,
                    // which could discard the runner's final
                    // `workflow_events` batch sitting in the socket buffer.
                    #[cfg(unix)]
                    drain_event_pipe(&mut process.event_pipe).await;
                    cleanup_agent_record(&process);
                    let exit_code = status.code();
                    let events = parse_runner_events(&process.stderr_lines);
                    let workflow_target = process.target_workflow_id.take();
                    let runner_workflow_id = latest_runner_workflow_id(&events);
                    let (workflow_id, identity_mismatch) =
                        reconcile_workflow_identity(workflow_target.as_deref(), runner_workflow_id.as_deref());
                    let mut workflow_status = latest_runner_workflow_status(&events);
                    let (mut success, mut failure_reason) = if status.success() {
                        (true, None)
                    } else {
                        (false, Some(format!("workflow runner exited unsuccessfully with status {:?}", exit_code)))
                    };
                    if let Some(mismatch) = identity_mismatch {
                        tracing::error!(
                            target: "animus.runtime.workflow_identity",
                            kernel_workflow_id = workflow_target.as_deref().unwrap_or("-"),
                            runner_workflow_id = runner_workflow_id.as_deref().unwrap_or("-"),
                            "runner reported a workflow id that diverges from the authoritative dispatch id"
                        );
                        success = false;
                        workflow_status = Some(WorkflowStatus::Failed);
                        failure_reason = Some(match failure_reason {
                            Some(existing) => format!("{existing}; {mismatch}"),
                            None => mismatch,
                        });
                    }
                    // BU-4 (codex P2): a RESUME-spawned runner that exits
                    // non-zero WITHOUT reporting a workflow status (e.g. a bad
                    // `--workflow-id`, a plugin/startup failure, an arg parse
                    // error — all before any `workflow_events`) must terminalize
                    // the TARGETED run as Failed. Otherwise the projector only
                    // blocks the task, the workflow stays Running, and the
                    // journal-resume sweep re-dispatches it every tick
                    // (livelock). This is safe for both resume and preallocated
                    // fresh spawns because each has an authoritative journal id.
                    if workflow_status.is_none() && !status.success() && workflow_target.is_some() {
                        workflow_status = Some(WorkflowStatus::Failed);
                    }

                    // Record WHY the runner exited (code/signal/duration + stderr
                    // tail) so a mid-run death — e.g. the ~60s exec_session
                    // severance / SIGKILL / OOM that leaves a delegated run a
                    // "running" ghost — is diagnosable from the logs instead of
                    // silent (TASK-799).
                    emit_runner_exit_diagnostic(
                        workflow_id.as_deref(),
                        &status,
                        process.started_at,
                        &process.stderr_lines,
                    );

                    // Terminal workflow state => tear the run's shared node down
                    // (one node per run). Non-terminal (Running/Paused between
                    // phases) => KEEP the node for the next phase's runner.
                    teardown_environment_if_terminal(
                        &environment_broker,
                        process.environment_run_id.take(),
                        workflow_status,
                    )
                    .await;

                    completed.push(CompletedProcess {
                        subject_id: process.subject_key,
                        subject_kind: Some(process.subject_kind),
                        task_id: process.task_id,
                        workflow_id,
                        workflow_ref: Some(process.workflow_ref),
                        workflow_status,
                        schedule_id: process.schedule_id,
                        exit_code,
                        success,
                        failure_reason,
                        events,
                    });
                }
                Ok(None) => active.push(process),
                Err(error) => {
                    #[cfg(unix)]
                    drain_event_pipe(&mut process.event_pipe).await;
                    cleanup_agent_record(&process);
                    completed.push(CompletedProcess {
                        subject_id: process.subject_key,
                        subject_kind: Some(process.subject_kind),
                        task_id: process.task_id,
                        workflow_id: process.target_workflow_id.take(),
                        workflow_ref: Some(process.workflow_ref),
                        workflow_status: None,
                        schedule_id: process.schedule_id,
                        exit_code: None,
                        success: false,
                        failure_reason: Some(format!("failed to probe workflow process status: {}", error)),
                        events: Vec::new(),
                    });
                }
            }
        }

        self.processes = active;
        completed
    }

    pub fn active_count(&self) -> usize {
        self.processes.len()
    }

    pub fn active_subject_ids(&self) -> HashSet<String> {
        self.processes.iter().flat_map(|process| [process.subject_key.clone(), process.subject_id.clone()]).collect()
    }
}

/// REQ-048: tear the run's shared ephemeral node down IFF the completed phase
/// landed the workflow in a terminal state (Completed/Failed/Cancelled/
/// Escalated). A non-terminal exit (Running/Paused between phases) keeps the
/// node for the next phase's runner. A no-op when the dispatch was not brokered
/// (`run_id` is `None`) or no broker is wired. `teardown` is idempotent.
async fn teardown_environment_if_terminal(
    broker: &Option<EnvironmentBroker>,
    run_id: Option<String>,
    status: Option<WorkflowStatus>,
) {
    let (Some(broker), Some(run_id)) = (broker.as_ref(), run_id) else {
        return;
    };
    if is_terminal_workflow_status(status) {
        let _ = broker.teardown(&run_id).await;
    }
}

fn is_terminal_workflow_status(status: Option<WorkflowStatus>) -> bool {
    matches!(
        status,
        Some(
            WorkflowStatus::Completed | WorkflowStatus::Failed | WorkflowStatus::Cancelled | WorkflowStatus::Escalated
        )
    )
}

fn cleanup_agent_record(process: &WorkflowProcess) {
    if let (Some(project_root), Some(id)) = (process.project_root.as_ref(), process.agent_session_id.as_ref()) {
        super::agent_record::delete_record(project_root, id);
    }
}

/// Default per-process directory for per-run event-pipe socket files.
/// Picked under `$TMPDIR/animus-event-pipes/<pid>/` so the path stays well
/// under SUN_LEN on macOS / Linux even when the project root is deep, and
/// so concurrent daemons can't collide on file names.
#[cfg(unix)]
fn default_event_pipe_root() -> std::path::PathBuf {
    std::env::temp_dir().join("animus-event-pipes").join(std::process::id().to_string())
}

/// v0.5.1 P2 #6.2: pick a deterministic, daemon-restart-stable socket path
/// for the runner's reattach listener. Lives under the scoped state root
/// when one is available so the orphan scan can discover it by reading the
/// spawn record alone. Falls back to `$TMPDIR` when scoped state is missing
/// (tests with no git context); reattach across restarts is unreachable in
/// that fallback mode but local first-spawn streaming still works.
///
/// SUN_LEN (104 on macOS, 108 on Linux) caps the absolute socket path,
/// so we use a short suffix (`r.sock`) and prefer `$TMPDIR/animus-reattach`
/// when the canonical path would overflow the limit.
#[cfg(unix)]
fn reattach_socket_path_for(project_root: &Path, agent_session_id: &str) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    // macOS SUN_LEN is 104; subtract one NUL byte. Linux SUN_LEN is 108.
    // Pick the tighter (macOS) limit so paths that fit on macOS also work
    // on Linux. The kernel rejects bind() above this without a useful
    // error so we proactively switch to the fallback tmpdir-based path.
    const MAX_UNIX_SOCKET_PATH_BYTES: usize = 103;
    let scoped_path = protocol::scoped_state_root(project_root)
        .map(|root| root.join("runs").join("_pending").join("agents").join(format!("{agent_session_id}.r.sock")));
    if let Some(path) = scoped_path.as_ref() {
        if path.as_os_str().as_bytes().len() <= MAX_UNIX_SOCKET_PATH_BYTES {
            return Some(path.clone());
        }
    }
    let fallback = std::env::temp_dir()
        .join("animus-reattach")
        .join(std::process::id().to_string())
        .join(format!("{agent_session_id}.r.sock"));
    if fallback.as_os_str().as_bytes().len() <= MAX_UNIX_SOCKET_PATH_BYTES {
        Some(fallback)
    } else {
        // Path too long even for the fallback location; skip the reattach
        // socket entirely rather than handing the runner a path it cannot
        // bind. First-spawn streaming via the legacy event pipe still
        // works because its root selection already handles SUN_LEN.
        None
    }
}

async fn drain_stderr_reader(handle: &mut Option<JoinHandle<()>>) {
    if let Some(h) = handle.take() {
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), h).await;
    }
}

/// Take + await `SubprocessEventPipe::shutdown()` on a process's event pipe
/// before the surrounding `WorkflowProcess` is dropped. Without this the
/// Drop path aborts the reader task, which can discard the final batch of
/// `workflow_events` the subprocess emitted right before exit (the writer
/// flushed bytes into the socket buffer; the reader had not yet consumed
/// them when abort fired). `shutdown` performs a bounded-wait drain so a
/// misbehaving plugin still cannot stall daemon progress.
#[cfg(unix)]
async fn drain_event_pipe(pipe: &mut Option<SubprocessEventPipe>) {
    if let Some(pipe) = pipe.take() {
        pipe.shutdown().await;
    }
}

fn parse_runner_events(stderr_lines: &Arc<Mutex<Vec<String>>>) -> Vec<RunnerEvent> {
    let lines = match stderr_lines.lock() {
        Ok(guard) => guard.clone(),
        Err(_) => return Vec::new(),
    };
    lines.iter().filter_map(|line| serde_json::from_str::<RunnerEvent>(line).ok()).collect()
}

fn latest_runner_workflow_id(events: &[RunnerEvent]) -> Option<String> {
    events.iter().rev().find_map(|event| event.workflow_id.clone())
}

/// Select the identity used for completion reconciliation. A queue/kernel
/// target is authoritative: runner output may confirm it but can never replace
/// it. Divergence fails closed and is returned as a diagnostic containing both
/// values. The runner id remains authoritative only for legacy ad-hoc spawns.
fn reconcile_workflow_identity(
    workflow_target: Option<&str>,
    runner_workflow_id: Option<&str>,
) -> (Option<String>, Option<String>) {
    match (workflow_target, runner_workflow_id) {
        (Some(target), Some(reported)) if target != reported => (
            Some(target.to_string()),
            Some(format!("workflow identity mismatch: authoritative_id={target} runner_reported_id={reported}")),
        ),
        (Some(target), _) => (Some(target.to_string()), None),
        (None, Some(reported)) => (Some(reported.to_string()), None),
        (None, None) => (None, None),
    }
}

fn latest_runner_workflow_status(events: &[RunnerEvent]) -> Option<WorkflowStatus> {
    events.iter().rev().find_map(|event| event.workflow_status)
}

/// Max stderr lines included in the `runner-exit` diagnostic tail (a bounded
/// slice of the already-captured stderr — enough to name the exit reason).
const RUNNER_STDERR_TAIL_LINES: usize = 40;

/// Hard char budget for the diagnostic stderr tail — bounds the log line AND the
/// transient allocation while building it.
const RUNNER_STDERR_TAIL_MAX_CHARS: usize = 2000;

/// Build a single-line, newline-escaped, char-budgeted tail from the last few
/// stderr lines WITHOUT ever allocating the (potentially huge) full content: it
/// copies at most `max_chars` chars, so a pathological megabyte-long stderr line
/// can never OOM the daemon during reap (codex). Lines are joined with an escaped
/// `\n`; embedded newlines are escaped the same way.
fn capped_escaped_tail(lines: &[String], max_chars: usize) -> String {
    let mut out = String::new();
    let mut budget = max_chars;
    for (idx, line) in lines.iter().enumerate() {
        if budget == 0 {
            break;
        }
        if idx > 0 {
            if budget < 2 {
                break;
            }
            out.push_str("\\n");
            budget -= 2;
        }
        for ch in line.chars() {
            if ch == '\n' {
                if budget < 2 {
                    return out;
                }
                out.push_str("\\n");
                budget -= 2;
            } else {
                if budget == 0 {
                    return out;
                }
                out.push(ch);
                budget -= 1;
            }
        }
    }
    out
}

/// Emit one greppable `runner-exit` line to STDOUT (the daemon's log stream,
/// which reaches Railway — same channel as the reconcile-* lines) when a
/// dispatched workflow runner exits. Records exit code, signal (SIGKILL=OOM/
/// supervision), wall-clock duration, and the stderr tail — the definitive "why
/// did the delegating runner die" signal (TASK-799). Safe here (unlike the local
/// CLI spawn path): the daemon is a long-lived supervisor, not a `--json` command.
fn emit_runner_exit_diagnostic(
    workflow_id: Option<&str>,
    status: &std::process::ExitStatus,
    started_at: std::time::Instant,
    stderr_lines: &Arc<Mutex<Vec<String>>>,
) {
    #[cfg(unix)]
    let signal = {
        use std::os::unix::process::ExitStatusExt;
        status.signal()
    };
    #[cfg(not(unix))]
    let signal: Option<i32> = None;
    let tail_capped = stderr_lines
        .lock()
        .map(|buf| {
            let start = buf.len().saturating_sub(RUNNER_STDERR_TAIL_LINES);
            capped_escaped_tail(&buf[start..], RUNNER_STDERR_TAIL_MAX_CHARS)
        })
        .unwrap_or_default();
    println!(
        "{}",
        format_runner_exit_line(workflow_id, status.code(), signal, started_at.elapsed().as_secs(), &tail_capped)
    );
}

/// Pure formatter for the `runner-exit` line (kept separate so it is testable).
/// `tail_capped` is expected to be pre-capped + single-lined by
/// [`capped_escaped_tail`], so this only interpolates.
fn format_runner_exit_line(
    workflow_id: Option<&str>,
    code: Option<i32>,
    signal: Option<i32>,
    duration_secs: u64,
    tail_capped: &str,
) -> String {
    format!(
        "runner-exit workflow_id={} code={code:?} signal={signal:?} duration_secs={duration_secs} stderr_tail={tail_capped}",
        workflow_id.unwrap_or("-"),
    )
}

#[cfg(test)]
mod tests {
    #![allow(clippy::await_holding_lock)]

    use super::*;
    use protocol::SubjectDispatchExt;
    use std::fs;
    use std::sync::Mutex;
    use tempfile::TempDir;

    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn runner_exit_line_formats_signal_and_pre_capped_tail() {
        // A SIGKILL (OOM / external supervision) — the "why did the delegating
        // runner die" record for a mid-run death.
        let tail = capped_escaped_tail(&["boom".to_string(), "exec_session closed".to_string()], 2000);
        let line = format_runner_exit_line(Some("wf-abc"), None, Some(9), 61, &tail);
        assert!(line.starts_with("runner-exit workflow_id=wf-abc"));
        assert!(line.contains("code=None"));
        assert!(line.contains("signal=Some(9)"));
        assert!(line.contains("duration_secs=61"));
        // Lines joined + newlines escaped so the record stays one greppable line.
        assert!(line.contains("stderr_tail=boom\\nexec_session closed"));
        assert!(!line.contains('\n'), "the record must stay one greppable line");
        // A clean exit renders code/workflow_id defaults.
        let clean = format_runner_exit_line(None, Some(0), None, 5, "");
        assert!(clean.contains("workflow_id=-"));
        assert!(clean.contains("code=Some(0)"));
    }

    #[test]
    fn kernel_workflow_id_wins_and_divergence_fails_closed_with_both_ids() {
        let (workflow_id, mismatch) = reconcile_workflow_identity(Some("queue-id"), Some("runner-id"));
        assert_eq!(workflow_id.as_deref(), Some("queue-id"));
        let mismatch = mismatch.expect("divergence must fail closed");
        assert!(mismatch.contains("authoritative_id=queue-id"));
        assert!(mismatch.contains("runner_reported_id=runner-id"));

        let (legacy_id, mismatch) = reconcile_workflow_identity(None, Some("runner-only-id"));
        assert_eq!(legacy_id.as_deref(), Some("runner-only-id"));
        assert!(mismatch.is_none());
    }

    #[test]
    fn capped_escaped_tail_bounds_memory_and_escapes_newlines() {
        // A pathological megabyte-long single stderr line must NOT be copied
        // whole — the cap applies while building, so the result is <= max_chars.
        let huge = "x".repeat(1_000_000);
        let out = capped_escaped_tail(std::slice::from_ref(&huge), 2000);
        assert_eq!(out.chars().count(), 2000);
        // Embedded newlines and the inter-line join are both escaped to `\n`.
        let escaped = capped_escaped_tail(&["a\nb".to_string(), "c".to_string()], 2000);
        assert_eq!(escaped, "a\\nb\\nc");
        assert!(!escaped.contains('\n'));
    }

    fn test_env_lock() -> &'static Mutex<()> {
        // Use the dispatch-wide shared lock so we serialize with sibling
        // modules (build_runner_command_from_dispatch tests) that also
        // mutate process-wide env vars (`ANIMUS_WORKFLOW_RUNNER_BIN`,
        // `PATH`).
        crate::dispatch::test_env::lock()
    }

    use protocol::test_utils::EnvVarGuard;

    #[test]
    fn new_process_manager_starts_empty() {
        let manager = ProcessManager::new();
        assert_eq!(manager.active_count(), 0);
    }

    #[test]
    fn new_process_manager_seeds_concurrency_cap_from_runtime_quotas() {
        // `ProcessManager::new()` must always seed `workflow_concurrency_max`
        // from `RuntimeQuotas` — never leave it `None`. Previously the
        // field was `None` unless `ANIMUS_WORKFLOW_CONCURRENCY_MAX` was
        // explicitly set, leaving the spawn site unbounded for typical
        // operators and contradicting the documented "default 10" cap
        // in the v0.4.13 CHANGELOG.
        //
        // We don't mutate the env here (would race other tests sharing
        // the process); we only assert the wiring: whatever the
        // process-wide quota currently is, `ProcessManager::new()`
        // mirrors it as `Some(quota)`.
        let manager = ProcessManager::new();
        let cap = manager.workflow_concurrency_max.expect("default cap must be wired from RuntimeQuotas");
        let expected = crate::quotas::runtime_quotas().workflow_concurrency_max;
        assert_eq!(cap, expected, "ProcessManager cap must match the live RuntimeQuotas value");
        assert!(cap > 0, "default workflow concurrency must be > 0; got {cap}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn spawn_workflow_runner_persists_reattach_socket_path_in_record() {
        // v0.5.1 P2 #6.2 round-3: after spawn, the AgentSpawnRecord written
        // under `runs/_pending/agents/<id>.json` must carry a non-None
        // `stdio_socket_path` so the next daemon start's orphan-scan +
        // reattach pass can find the runner's reattach listener.
        let _lock = test_env_lock().lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        crate::test_env::stable_test_home();

        let temp_dir = TempDir::new().expect("temp directory");
        let runner_path = temp_dir.path().join("animus-workflow-runner");
        // Sleep long enough that the record is on disk before check_running drains it.
        let runner_payload = "#!/bin/sh\nsleep 3\nexit 0\n";
        fs::write(&runner_path, runner_payload).expect("write runner");
        let mut permissions = fs::metadata(&runner_path).expect("meta").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&runner_path, permissions).expect("perm");

        let runner_override = runner_path.to_string_lossy();
        let _runner_guard = EnvVarGuard::set("ANIMUS_WORKFLOW_RUNNER_BIN", Some(runner_override.as_ref()));

        // Use the temp dir as the project root so the spawn record lands
        // under a path we can inspect.
        let mut manager = ProcessManager::new();
        let dispatch = SubjectDispatch::for_task("TASK-REATTACH", "standard");
        manager
            .spawn_workflow_runner(&dispatch, temp_dir.path().to_string_lossy().as_ref())
            .expect("spawn must succeed");

        // Find the just-written record.
        let agents_dir = protocol::scoped_state_root(temp_dir.path())
            .map(|scope| scope.join("runs").join("_pending").join("agents"));
        let dir = agents_dir.expect("scoped state root must resolve under test home");
        // Records appear under either the scoped root or, in degraded test
        // homes, may not exist if pid was None. Either is acceptable, but
        // when the record is present `stdio_socket_path` must be Some.
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|s| s.to_str()) != Some("json") {
                    continue;
                }
                let raw = std::fs::read_to_string(&path).expect("read record");
                let record: crate::dispatch::agent_record::AgentSpawnRecord =
                    serde_json::from_str(&raw).expect("parse record");
                assert!(
                    record.stdio_socket_path.is_some(),
                    "spawn record must carry the reattach socket path so v0.5.1 reattach can find the runner (record: {raw})"
                );
                let socket = record.stdio_socket_path.unwrap();
                assert!(socket.ends_with(".r.sock"), "socket path must use the .r.sock suffix; got {socket}");
            }
        }

        let _ = manager.check_running().await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn spawn_workflow_runner_starts_in_new_session() {
        // v0.5.1 #8: the spawn path calls `setsid()` in `pre_exec`, so the
        // runner subprocess must end up in a different POSIX session from
        // the test parent. This survives terminal hangups (SIGHUP) in
        // addition to the SIGTERM-foreground-group propagation that the
        // weaker `process_group(0)` form covered.
        let _lock = test_env_lock().lock().unwrap_or_else(|poisoned| poisoned.into_inner());

        let temp_dir = TempDir::new().expect("temp dir");
        let runner_path = temp_dir.path().join("animus-workflow-runner");
        // Sleep long enough that the parent can `getsid(child_pid)` while
        // the runner is still alive. 2s is plenty for an in-process probe.
        let runner_payload = "#!/bin/sh\nsleep 2\nexit 0\n";
        fs::write(&runner_path, runner_payload).expect("write runner");
        let mut permissions = fs::metadata(&runner_path).expect("meta").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&runner_path, permissions).expect("perm");

        let runner_override = runner_path.to_string_lossy();
        let _runner_guard = EnvVarGuard::set("ANIMUS_WORKFLOW_RUNNER_BIN", Some(runner_override.as_ref()));

        let mut manager = ProcessManager::new();
        let dispatch = SubjectDispatch::for_task("TASK-SETSID", "standard");
        manager
            .spawn_workflow_runner(&dispatch, temp_dir.path().to_string_lossy().as_ref())
            .expect("spawn must succeed");

        let child_pid =
            manager.processes.first().expect("spawned process must be tracked").child.lock().unwrap().id().unwrap();

        // SAFETY: getsid(pid) is an infallible syscall with respect to
        // memory and aliasing; -1 + EPERM is the only error mode and we
        // assert on the value the kernel returns.
        #[allow(unsafe_code)]
        let child_sid = unsafe { libc::getsid(child_pid as i32) };
        #[allow(unsafe_code)]
        let parent_sid = unsafe { libc::getsid(0) };

        assert_ne!(child_sid, -1, "getsid(child_pid) failed: {}", std::io::Error::last_os_error());
        assert_ne!(
            child_sid, parent_sid,
            "runner subprocess must run in a new session (child sid={child_sid}, parent sid={parent_sid})"
        );

        let _ = manager.check_running().await;
    }

    #[tokio::test]
    async fn spawn_workflow_runner_tracks_active_processes() {
        let _lock = test_env_lock().lock().unwrap_or_else(|poisoned| poisoned.into_inner());

        let temp_dir = TempDir::new().expect("temp directory should be created");
        let runner_path = {
            #[cfg(unix)]
            let path = temp_dir.path().join("animus-workflow-runner");
            #[cfg(not(unix))]
            let path = temp_dir.path().join("animus-workflow-runner.exe");
            path
        };

        #[cfg(unix)]
        let runner_payload = "#!/bin/sh\nexit 0\n";
        #[cfg(not(unix))]
        let runner_payload = "@echo off\r\nexit /B 0\r\n";

        fs::write(&runner_path, runner_payload).expect("mock runner should be written");
        #[cfg(unix)]
        {
            let mut permissions =
                fs::metadata(&runner_path).expect("mock runner metadata should be available").permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(&runner_path, permissions).expect("mock runner should be executable");
        }

        let runner_override = runner_path.to_string_lossy();
        let _runner_guard = EnvVarGuard::set("ANIMUS_WORKFLOW_RUNNER_BIN", Some(runner_override.as_ref()));

        let mut manager = ProcessManager::new();
        let dispatch = SubjectDispatch::for_task("task-123", "standard");
        manager
            .spawn_workflow_runner(&dispatch, temp_dir.path().to_string_lossy().as_ref())
            .expect("mock runner should be spawned via explicit workflow runner override");
        assert_eq!(manager.active_count(), 1);
        let _ = manager.check_running().await;
    }

    #[tokio::test]
    async fn workflow_concurrency_queues_when_at_cap() {
        // v0.4.13: ProcessManager with `with_workflow_concurrency_max(2)`
        // accepts the first two spawns and refuses the third with a
        // recoverable error so the dispatcher leaves the third entry in
        // the ready queue for the next tick.
        let _lock = test_env_lock().lock().unwrap_or_else(|poisoned| poisoned.into_inner());

        let temp_dir = TempDir::new().expect("temp directory should be created");
        let runner_path = temp_dir.path().join("animus-workflow-runner");
        // A runner that sleeps long enough that the first two stay active
        // while we attempt the third spawn.
        #[cfg(unix)]
        let runner_payload = "#!/bin/sh\nsleep 5\nexit 0\n";
        #[cfg(not(unix))]
        let runner_payload = "@echo off\r\ntimeout 5\r\nexit /B 0\r\n";
        fs::write(&runner_path, runner_payload).expect("mock runner should be written");
        #[cfg(unix)]
        {
            let mut permissions =
                fs::metadata(&runner_path).expect("mock runner metadata should be available").permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(&runner_path, permissions).expect("mock runner should be executable");
        }
        let runner_override = runner_path.to_string_lossy();
        let _runner_guard = EnvVarGuard::set("ANIMUS_WORKFLOW_RUNNER_BIN", Some(runner_override.as_ref()));

        let mut manager = ProcessManager::new().with_workflow_concurrency_max(Some(2));
        let project_root = temp_dir.path().to_string_lossy().to_string();

        let d1 = SubjectDispatch::for_task("task-1", "standard");
        let d2 = SubjectDispatch::for_task("task-2", "standard");
        let d3 = SubjectDispatch::for_task("task-3", "standard");

        manager.spawn_workflow_runner(&d1, &project_root).expect("spawn 1 should succeed (under cap)");
        manager.spawn_workflow_runner(&d2, &project_root).expect("spawn 2 should succeed (at cap)");
        assert_eq!(manager.active_count(), 2);

        let third = manager.spawn_workflow_runner(&d3, &project_root);
        assert!(third.is_err(), "spawn 3 must be refused when at concurrency cap");
        let err = third.unwrap_err().to_string();
        assert!(err.contains("workflow concurrency cap reached"), "error must explain the cap; got: {err}");
        // The dispatcher's contract: refused entries stay in the queue.
        // We assert active count is still 2 (the third was not silently
        // accepted then dropped).
        assert_eq!(manager.active_count(), 2);

        // Drain so the test exits cleanly.
        for _ in 0..200 {
            if manager.check_running().await.len() == 2 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    }

    #[test]
    fn subject_id_returns_correct_value_for_each_variant() {
        let task = SubjectDispatch::for_task("TASK-1", "standard");
        assert_eq!(task.subject_id(), Some("TASK-1"));
        assert!(task.schedule_id().is_none());

        let requirement = SubjectDispatch::for_requirement("REQ-1", "standard", "manual");
        assert_eq!(requirement.subject_id(), Some("REQ-1"));
        assert!(requirement.schedule_id().is_none());

        let custom = SubjectDispatch::for_custom(
            "schedule:nightly",
            "nightly run",
            "standard",
            Some(serde_json::json!({"key":"value"})),
            "schedule",
        );
        assert_eq!(custom.subject_id(), Some("schedule:nightly"));
        assert_eq!(custom.schedule_id(), Some("nightly"));
    }

    #[tokio::test]
    async fn custom_subject_tracks_schedule_id_and_parses_events() {
        let _lock = test_env_lock().lock().unwrap_or_else(|poisoned| poisoned.into_inner());

        let temp_dir = TempDir::new().expect("temp directory should be created");
        let runner_path = temp_dir.path().join("animus-workflow-runner");
        let runner_payload = "#!/bin/sh\nprintf '%s\\n' '{\"event\":\"runner_start\",\"workflow_ref\":\"standard\"}' >&2\nprintf '%s\\n' '{\"event\":\"runner_complete\",\"workflow_ref\":\"standard\",\"exit_code\":0}' >&2\nexit 0\n";
        fs::write(&runner_path, runner_payload).expect("mock runner should be written");
        #[cfg(unix)]
        {
            let mut permissions =
                fs::metadata(&runner_path).expect("mock runner metadata should be available").permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(&runner_path, permissions).expect("mock runner should be executable");
        }

        let runner_override = runner_path.to_string_lossy();
        let _runner_guard = EnvVarGuard::set("ANIMUS_WORKFLOW_RUNNER_BIN", Some(runner_override.as_ref()));

        let mut manager = ProcessManager::new();
        let dispatch = SubjectDispatch::for_custom("schedule:nightly", "nightly run", "standard", None, "schedule");
        manager
            .spawn_workflow_runner(&dispatch, temp_dir.path().to_string_lossy().as_ref())
            .expect("mock runner should spawn");

        let mut completed = Vec::new();
        for _ in 0..100 {
            completed = manager.check_running().await;
            if !completed.is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }

        assert_eq!(completed.len(), 1);
        let completed = &completed[0];
        assert_eq!(completed.subject_id, "schedule:nightly");
        assert_eq!(completed.schedule_id.as_deref(), Some("nightly"));
        assert!(completed.success);
        assert_eq!(completed.events.len(), 2);
        assert!(completed.workflow_id.is_none());
        assert!(completed.workflow_status.is_none());
        assert_eq!(completed.events[0].workflow_ref.as_deref(), Some("standard"));
        assert_eq!(completed.events[1].workflow_ref.as_deref(), Some("standard"));
    }

    #[tokio::test]
    async fn generic_subjects_keep_kind_qualified_completion_identity() {
        let _lock = test_env_lock().lock().unwrap_or_else(|poisoned| poisoned.into_inner());

        let temp_dir = TempDir::new().expect("temp directory should be created");
        let runner_path = temp_dir.path().join("animus-workflow-runner");
        fs::write(&runner_path, "#!/bin/sh\nexit 0\n").expect("mock runner should be written");
        #[cfg(unix)]
        {
            let mut permissions =
                fs::metadata(&runner_path).expect("mock runner metadata should be available").permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(&runner_path, permissions).expect("mock runner should be executable");
        }

        let runner_override = runner_path.to_string_lossy();
        let _runner_guard = EnvVarGuard::set("ANIMUS_WORKFLOW_RUNNER_BIN", Some(runner_override.as_ref()));

        let dispatch = SubjectDispatch::for_subject_with_metadata(
            protocol::SubjectRef::new("pack.review", "REV-7"),
            "review",
            "manual",
            chrono::Utc::now(),
        );

        let mut manager = ProcessManager::new();
        manager
            .spawn_workflow_runner(&dispatch, temp_dir.path().to_string_lossy().as_ref())
            .expect("mock runner should spawn");

        let active_subject_ids = manager.active_subject_ids();
        assert!(active_subject_ids.contains("REV-7"));
        assert!(active_subject_ids.contains("pack.review::REV-7"));

        let mut completed = Vec::new();
        for _ in 0..100 {
            completed = manager.check_running().await;
            if !completed.is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }

        assert_eq!(completed.len(), 1);
        assert_eq!(completed[0].subject_id, "pack.review::REV-7");
        assert_eq!(completed[0].subject_kind.as_deref(), Some("pack.review"));
    }

    #[test]
    fn workflow_skills_env_payload_resolves_declared_phase_skills() {
        let _lock = test_env_lock().lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        crate::test_env::stable_test_home();

        let temp_dir = TempDir::new().expect("temp directory");
        let project_root = temp_dir.path().to_str().expect("utf-8 tempdir");

        // No `.animus` config at all: only the builtin agent-profile skill
        // defaults (which reference the extracted `animus.core-skills`
        // pack) can appear, and none of them resolve in a vanilla project.
        if let Some(json) = super::workflow_skills_env_payload(project_root) {
            let payload: animus_runtime_shared::phase_skills::WorkflowSkillsPayload =
                serde_json::from_str(&json).expect("vanilla payload must parse");
            assert!(
                payload.phases.values().all(|resolution| resolution.resolved.is_empty()),
                "vanilla project must not resolve any skill: {json}"
            );
        }

        let animus = temp_dir.path().join(".animus");
        let skills_dir = animus.join("config").join("skill_definitions");
        fs::create_dir_all(&skills_dir).expect("create skills dir");
        fs::write(skills_dir.join("deep-search.yaml"), "name: deep-search\nprompt:\n  prefix: deep-search prefix\n")
            .expect("write skill");
        fs::write(
            animus.join("workflows.yaml"),
            "phases:\n  research:\n    mode: agent\n    skills:\n      - deep-search\n      - ghost-skill\nworkflows:\n  - id: research-only\n    name: Research Only\n    phases:\n      - research\n",
        )
        .expect("write workflows.yaml");
        let _config = crate::test_env::install_yaml_config_source_fixture(temp_dir.path());

        let json = super::workflow_skills_env_payload(project_root)
            .expect("project with declared phase skills must produce a payload");
        let payload: animus_runtime_shared::phase_skills::WorkflowSkillsPayload =
            serde_json::from_str(&json).expect("payload must round-trip");
        let research = payload.phases.get("research").expect("research phase entry");
        assert_eq!(research.resolved.len(), 1);
        assert_eq!(research.resolved[0].definition.name, "deep-search");
        assert_eq!(research.missing, vec!["ghost-skill"]);
    }
}
