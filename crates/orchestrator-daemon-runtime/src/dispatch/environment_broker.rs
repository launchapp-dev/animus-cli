//! Cross-phase ephemeral-environment broker (daemon side).
//!
//! The daemon dispatches ONE workflow-runner subprocess PER PHASE. Without a
//! broker each runner would prepare its OWN ephemeral node (via the in-runner
//! [`EnvironmentClient`]) and tear it down at exit, so phases of one workflow run
//! never share a workspace. This broker moves node lifecycle into the DAEMON:
//! ONE node per workflow RUN, shared by every phase, torn down once at run end.
//!
//! ## Model
//!
//! - The broker keeps a `run_id -> lease` map. The FIRST [`Self::acquire`] for a
//!   run resolves a daemon-resident [`EnvironmentClient`] and prepares the node
//!   ONCE (single-flight, per-`run_id` mutex); later acquires for the same run
//!   return the SAME handle without re-preparing.
//! - Because the client is resolved DAEMON-side, the process-global
//!   resident-host registry keeps ONE plugin process alive for the whole run
//!   (one relay = one node), pinned across prepare / exec / teardown.
//! - [`Self::teardown`] disposes the node once, at terminal workflow state.
//! - Durable JSON lease records under the scoped state root let a fresh daemon
//!   adopt the exact Ready lease still claimed by a Running checkpoint, while
//!   cold-reaping unclaimed nodes leaked by a PRIOR daemon instance.
//!
//! ## IPC
//!
//! Each per-phase runner talks to the broker over a private local socket
//! (newline-delimited JSON, [`interprocess::local_socket`]). The wire is a
//! PRIVATE daemon<->runner contract — NOT `animus-protocol` — with two RPCs:
//! `acquire` (one request / one response) and `exec` (one request then a stream
//! of output frames and a terminal frame). The serde structs are defined
//! independently here and in the runner; see `broker-wire-contract.md`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use animus_runtime_shared::phase_session::{list_running_checkpoints, update_session_environment, EnvironmentBinding};
use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::Mutex as AsyncMutex;

use orchestrator_core::environment::{EnvironmentHandle, EnvironmentSpec, ExecResponse, ExecStream, HarnessCommand};
use orchestrator_core::EnvironmentClient;

/// Local-socket path the per-phase runner dials to reach the broker.
pub const ANIMUS_ENVIRONMENT_BROKER_SOCKET_ENV: &str = "ANIMUS_ENVIRONMENT_BROKER_SOCKET";
/// Per-daemon bearer capability echoed on every broker frame.
pub const ANIMUS_ENVIRONMENT_BROKER_TOKEN_ENV: &str = "ANIMUS_ENVIRONMENT_BROKER_TOKEN";
/// Workflow run id this dispatch belongs to (the broker's single-flight key).
pub const ANIMUS_ENVIRONMENT_BROKER_RUN_ID_ENV: &str = "ANIMUS_ENVIRONMENT_BROKER_RUN_ID";
/// Resolved environment plugin id (e.g. `animus-environment-railway`).
pub const ANIMUS_ENVIRONMENT_BROKER_ENVIRONMENT_ID_ENV: &str = "ANIMUS_ENVIRONMENT_BROKER_ENVIRONMENT_ID";

/// Environment plugin ids that materialize a LOCAL workspace on the daemon host.
/// A run routed to one of these does NOT go through the broker — the per-phase
/// node-sharing problem is specific to remote/ephemeral environments.
const LOCAL_ENVIRONMENT_IDS: &[&str] = &["worktree", "local"];

/// `true` when `environment_id` names a local (non-brokered) environment.
pub fn is_local_environment(environment_id: &str) -> bool {
    LOCAL_ENVIRONMENT_IDS.contains(&environment_id)
}

/// Metadata key the railway (and any deterministic-naming) environment plugin
/// reads to name the node stably per workflow run. Injected by the broker into
/// the spec before `prepare` so every phase of a run maps to the same node name.
const ANIMUS_RUN_ID_METADATA_KEY: &str = "animus_run_id";

// ---------------------------------------------------------------------------
// Wire types (private daemon<->runner IPC — mirror broker-wire-contract.md).
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(tag = "op", rename_all = "lowercase")]
enum BrokerRequest {
    Acquire {
        token: String,
        run_id: String,
        environment_id: String,
        spec: EnvironmentSpec,
    },
    Exec {
        token: String,
        run_id: String,
        handle_id: String,
        command: HarnessCommand,
        #[serde(default)]
        stdin: Option<String>,
        #[serde(default)]
        timeout_secs: Option<u64>,
    },
}

// ---------------------------------------------------------------------------
// Durable lease record (survives a daemon restart so the new broker can adopt
// an exact claimed lease or cold-tear down an unclaimed leaked node).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum LeaseState {
    Preparing,
    Ready,
    TearingDown,
    TornDown,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LeaseRecord {
    run_id: String,
    daemon_instance_id: String,
    environment_id: String,
    project_root: String,
    state: LeaseState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    handle: Option<EnvironmentHandle>,
    created_at: String,
    updated_at: String,
}

/// Exact delegated environment whose previously failed teardown was confirmed.
///
/// Cleanup callers must use all four fields when updating durable phase
/// checkpoints: one workflow can retain multiple historical bindings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetriedEnvironmentTeardown {
    pub run_id: String,
    pub environment_id: String,
    pub project_root: String,
    pub handle: EnvironmentHandle,
}

// ---------------------------------------------------------------------------
// In-memory lease.
// ---------------------------------------------------------------------------

/// A ready, prepared node for a run: the pinned daemon-resident client plus the
/// environment handle every phase of the run execs against.
#[derive(Clone)]
struct ReadyLease {
    environment_id: String,
    project_root: String,
    client: Arc<dyn EnvironmentLeaseClient>,
    handle: EnvironmentHandle,
}

/// Object-safe surface the broker needs from an environment client. Keeping
/// resolution behind this seam lets the restart lifecycle be tested as one
/// broker-to-broker sequence without installing or spawning a real plugin.
trait EnvironmentLeaseClient: Send + Sync {
    fn prepare(&self, spec: EnvironmentSpec) -> Result<EnvironmentHandle>;
    fn exec_stream(
        &self,
        handle: &EnvironmentHandle,
        command: HarnessCommand,
        stdin: Option<String>,
        timeout: Option<Duration>,
        on_output: &(dyn Fn(ExecStream, &str) + Send + Sync),
    ) -> Result<ExecResponse>;
    fn teardown(&self, handle: &EnvironmentHandle) -> Result<()>;
}

impl EnvironmentLeaseClient for EnvironmentClient {
    fn prepare(&self, spec: EnvironmentSpec) -> Result<EnvironmentHandle> {
        EnvironmentClient::prepare(self, spec)
    }

    fn exec_stream(
        &self,
        handle: &EnvironmentHandle,
        command: HarnessCommand,
        stdin: Option<String>,
        timeout: Option<Duration>,
        on_output: &(dyn Fn(ExecStream, &str) + Send + Sync),
    ) -> Result<ExecResponse> {
        EnvironmentClient::exec_stream(
            self,
            handle,
            command,
            std::collections::BTreeMap::new(),
            stdin,
            timeout,
            on_output,
        )
    }

    fn teardown(&self, handle: &EnvironmentHandle) -> Result<()> {
        EnvironmentClient::teardown(self, handle)
    }
}

type ClientResolver = dyn Fn(&Path, &str) -> Result<Arc<dyn EnvironmentLeaseClient>> + Send + Sync;

/// Context the daemon registers at spawn time (it, not the runner, is the
/// authority on `project_root`). `acquire` resolves the [`EnvironmentClient`]
/// against this.
#[derive(Clone)]
struct PendingContext {
    project_root: String,
    environment_id: String,
}

struct Inner {
    daemon_instance_id: String,
    token: String,
    socket_path: String,
    /// Directory holding `<run_id>.json` durable lease records + the socket.
    records_dir: PathBuf,
    /// run_id -> ready lease. Only READY leases live here; a failed/torn-down
    /// run is absent (a later acquire re-prepares).
    leases: AsyncMutex<HashMap<String, ReadyLease>>,
    /// Per-run single-flight mutex so concurrent first-acquires prepare once.
    key_locks: StdMutex<HashMap<String, Arc<AsyncMutex<()>>>>,
    /// run_id -> spawn-time context (project_root + expected environment id).
    pending: StdMutex<HashMap<String, PendingContext>>,
    client_resolver: Arc<ClientResolver>,
    /// The socket acceptor task; aborted + socket unlinked on drop.
    acceptor: StdMutex<Option<tokio::task::JoinHandle<()>>>,
}

impl Drop for Inner {
    fn drop(&mut self) {
        if let Some(handle) = self.acceptor.lock().unwrap_or_else(|p| p.into_inner()).take() {
            handle.abort();
        }
        // Best-effort: unlink the socket file so a restart can rebind cleanly.
        if looks_like_filesystem(&self.socket_path) {
            let _ = std::fs::remove_file(&self.socket_path);
        }
    }
}

/// Daemon-side broker handle. Cheap to clone (`Arc`); every clone shares the one
/// lease map + socket server.
#[derive(Clone)]
pub struct EnvironmentBroker {
    inner: Arc<Inner>,
}

impl EnvironmentBroker {
    /// Bind the broker's local socket under `project_root`'s scoped state root,
    /// start the accept loop on the current Tokio runtime, adopt any exact
    /// Ready lease still claimed by a Running checkpoint, and reap unclaimed
    /// records owned by a prior daemon instance.
    ///
    /// Must be called from within the daemon's multi-threaded runtime: the
    /// resident [`EnvironmentClient`] the broker drives spawns its warm plugin
    /// host onto THIS runtime and is pinned there for the run's lifetime.
    pub async fn start(project_root: &str) -> std::io::Result<Self> {
        Self::start_with_resolver(
            project_root,
            Arc::new(|project_root, environment_id| {
                Ok(Arc::new(EnvironmentClient::resolve(project_root, environment_id)?))
            }),
        )
        .await
    }

    async fn start_with_resolver(project_root: &str, client_resolver: Arc<ClientResolver>) -> std::io::Result<Self> {
        let records_dir = broker_records_dir(project_root);
        std::fs::create_dir_all(&records_dir)?;
        let socket_path = broker_socket_path(&records_dir);

        ensure_socket_parent(&socket_path)?;
        let listener = bind_listener(&socket_path)?;

        let inner = Arc::new(Inner {
            daemon_instance_id: uuid::Uuid::new_v4().simple().to_string(),
            token: uuid::Uuid::new_v4().simple().to_string(),
            socket_path,
            records_dir,
            leases: AsyncMutex::new(HashMap::new()),
            key_locks: StdMutex::new(HashMap::new()),
            pending: StdMutex::new(HashMap::new()),
            client_resolver,
            acceptor: StdMutex::new(None),
        });

        let broker = Self { inner: inner.clone() };
        let accept_broker = broker.clone();
        let handle = tokio::spawn(async move { accept_loop(listener, accept_broker).await });
        *inner.acceptor.lock().unwrap_or_else(|p| p.into_inner()) = Some(handle);

        broker.reap_prior_daemon_records().await;
        Ok(broker)
    }

    /// Local-socket path handed to the runner via
    /// [`ANIMUS_ENVIRONMENT_BROKER_SOCKET_ENV`].
    pub fn socket_path(&self) -> &str {
        &self.inner.socket_path
    }

    /// Per-daemon bearer token handed to the runner via
    /// [`ANIMUS_ENVIRONMENT_BROKER_TOKEN_ENV`] and echoed on every frame.
    pub fn token(&self) -> &str {
        &self.inner.token
    }

    /// Record the spawn-time context for `run_id` so the runner's later
    /// `acquire` resolves the [`EnvironmentClient`] against the daemon-authored
    /// `project_root` (the wire frame never carries it). Idempotent per run.
    pub fn register_run(&self, run_id: &str, project_root: &str, environment_id: &str) {
        self.inner.pending.lock().unwrap_or_else(|p| p.into_inner()).insert(
            run_id.to_string(),
            PendingContext { project_root: project_root.to_string(), environment_id: environment_id.to_string() },
        );
    }

    /// Whether this daemon owns the exact durable lease a restart checkpoint
    /// names. Resume callers use this as a fail-closed gate: a live node is not
    /// enough; the new broker must have adopted its client + handle so later
    /// phases and terminal cleanup stay on the same workflow-scoped lease.
    pub async fn owns_ready_lease(&self, run_id: &str, environment_id: &str, handle: &EnvironmentHandle) -> bool {
        self.inner
            .leases
            .lock()
            .await
            .get(run_id)
            .is_some_and(|lease| lease.environment_id == environment_id && lease.handle == *handle)
    }

    #[cfg(test)]
    fn stop_acceptor_for_restart_test(&self) {
        if let Some(handle) = self.inner.acceptor.lock().unwrap_or_else(|p| p.into_inner()).take() {
            handle.abort();
        }
        if looks_like_filesystem(&self.inner.socket_path) {
            let _ = std::fs::remove_file(&self.inner.socket_path);
        }
    }

    /// Idempotently dispose the node for `run_id`: `Ready -> TearingDown ->
    /// TornDown`, delete the durable record, forget the run's pending context.
    /// A failed teardown retains a retryable durable record.
    /// A no-op when no lease exists (already torn down, or never prepared).
    /// Returns `true` only when cleanup is complete (including the idempotent
    /// no-lease case), so checkpoint owners do not record `torn_down` after a
    /// failed plugin RPC.
    pub async fn teardown(&self, run_id: &str) -> bool {
        // Serialize the whole remove -> RPC -> restore/delete transition per
        // run. Without this guard, a concurrent terminal-cleanup caller can
        // observe the temporarily removed lease as "already torn down" and
        // return true while the in-flight plugin RPC subsequently fails.
        let key_lock = self.key_lock(run_id);
        let _guard = key_lock.lock().await;

        let lease = self.inner.leases.lock().await.remove(run_id);
        if let Some(lease) = lease {
            self.write_record(run_id, &lease.environment_id, &lease.project_root, LeaseState::TearingDown, None);
            let client = lease.client.clone();
            let handle = lease.handle.clone();
            // Sync RPC against the pinned host. `teardown` bridges async→sync
            // internally (its own `block_in_place`), so it is called directly —
            // wrapping it in another `block_in_place` would nest and is not
            // needed.
            if let Err(error) = client.teardown(&handle) {
                tracing::warn!(
                    target: "animus.runtime.environment_broker",
                    run_id,
                    %error,
                    "environment teardown failed; retaining lease record for startup retry"
                );
                self.write_record(
                    run_id,
                    &lease.environment_id,
                    &lease.project_root,
                    LeaseState::TearingDown,
                    Some(&handle),
                );
                // Keep the exact client + handle retryable in this daemon.
                // Dropping the lease here makes the next teardown look like an
                // idempotent no-op, which would delete the durable record even
                // though the remote node was never confirmed torn down.
                self.inner.leases.lock().await.insert(run_id.to_string(), lease);
                return false;
            }
        }
        self.delete_record(run_id);
        self.forget_run(run_id);
        true
    }

    /// Retry teardown RPCs which failed earlier in this daemon lifetime.
    ///
    /// Only durable `TearingDown` records are selected: Ready leases still
    /// belong to active workflows and must never be swept merely because they
    /// are present in memory. Returns the exact bindings whose cleanup was
    /// confirmed so checkpoint owners do not mark unrelated historical
    /// bindings torn down.
    pub async fn retry_failed_teardowns(&self) -> Vec<RetriedEnvironmentTeardown> {
        let entries = match std::fs::read_dir(&self.inner.records_dir) {
            Ok(entries) => entries,
            Err(_) => return Vec::new(),
        };
        let mut retried = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|extension| extension.to_str()) != Some("json") {
                continue;
            }
            let Some(record) =
                std::fs::read_to_string(&path).ok().and_then(|raw| serde_json::from_str::<LeaseRecord>(&raw).ok())
            else {
                continue;
            };
            let Some(handle) = record.handle.clone() else {
                continue;
            };
            if record.daemon_instance_id == self.inner.daemon_instance_id
                && record.state == LeaseState::TearingDown
                && self.teardown(&record.run_id).await
            {
                retried.push(RetriedEnvironmentTeardown {
                    run_id: record.run_id,
                    environment_id: record.environment_id,
                    project_root: record.project_root,
                    handle,
                });
            }
        }
        retried
    }

    fn forget_run(&self, run_id: &str) {
        self.inner.pending.lock().unwrap_or_else(|p| p.into_inner()).remove(run_id);
        self.inner.key_locks.lock().unwrap_or_else(|p| p.into_inner()).remove(run_id);
    }

    /// SINGLE-FLIGHT acquire: return the shared workspace_root + handle_id for
    /// `run_id`, preparing the node ONCE on the first call. Rejects a different
    /// `environment_id` for an already-bound run.
    async fn acquire(&self, run_id: &str, environment_id: &str, spec: EnvironmentSpec) -> Result<(String, String)> {
        let pending =
            self.inner.pending.lock().unwrap_or_else(|p| p.into_inner()).get(run_id).cloned().ok_or_else(|| {
                anyhow!("no pending environment context for run {run_id} (daemon did not register this run)")
            })?;
        if pending.environment_id != environment_id {
            bail!("run {run_id} is bound to environment '{}', not '{environment_id}'", pending.environment_id);
        }

        let key_lock = self.key_lock(run_id);
        let _guard = key_lock.lock().await;

        // Fast path: an already-prepared lease is reused by every later phase.
        let existing = self
            .inner
            .leases
            .lock()
            .await
            .get(run_id)
            .map(|lease| (lease.environment_id.clone(), lease.handle.clone()));
        if let Some((lease_environment_id, handle)) = existing {
            if lease_environment_id != environment_id {
                bail!("run {run_id} is already bound to environment '{lease_environment_id}'");
            }
            // Every phase gets its own checkpoint. Reusing an existing
            // workflow lease must bind the current Running phase too, or a
            // second restart after the phase boundary cannot prove ownership.
            bind_running_phase_checkpoint(&pending.project_root, run_id, environment_id, &handle)
                .with_context(|| format!("persisting reused phase environment binding for run {run_id}"))?;
            return Ok((handle.workspace_root.clone(), handle.id.clone()));
        }

        // Slow path: prepare the node ONCE. Record BEFORE prepare so a crash
        // mid-prepare still leaves a durable marker for the startup reaper.
        self.write_record_required(run_id, environment_id, &pending.project_root, LeaseState::Preparing, None)
            .with_context(|| format!("persisting preparing lease for run {run_id}"))?;

        let mut spec = spec;
        set_run_id_metadata(&mut spec, run_id);

        let project_root = pending.project_root.clone();
        let environment_id_owned = environment_id.to_string();
        // `resolve` is pure discovery; `prepare` bridges async→sync internally
        // (its own `block_in_place`), so both are called directly — the daemon
        // worker is handed off for the duration of the prepare RPC by the client.
        let prepared = (|| {
            let client = (self.inner.client_resolver)(Path::new(&project_root), &environment_id_owned)
                .with_context(|| format!("resolving environment '{environment_id_owned}' for run {run_id}"))?;
            let handle = client.prepare(spec).with_context(|| format!("preparing environment for run {run_id}"))?;
            Ok::<_, anyhow::Error>((client, handle))
        })();

        match prepared {
            Ok((client, handle)) => {
                if let Err(error) = self.write_record_required(
                    run_id,
                    environment_id,
                    &pending.project_root,
                    LeaseState::Ready,
                    Some(&handle),
                ) {
                    // Never expose/execute a prepared node whose complete
                    // handle is not durable. Best-effort rollback keeps the
                    // failure contained to acquire.
                    let cleanup = client.teardown(&handle).err();
                    return Err(error).with_context(|| {
                        format!(
                            "persisting ready lease for run {run_id}; prepared node cleanup: {}",
                            cleanup
                                .map(|error| format!("failed: {error:#}"))
                                .unwrap_or_else(|| "succeeded".to_string())
                        )
                    });
                }
                if let Err(error) =
                    bind_running_phase_checkpoint(&pending.project_root, run_id, environment_id, &handle)
                {
                    // The lease record is sufficient for cold reaping, but
                    // restart resume and liveness reconciliation use the phase
                    // checkpoint as their recovery oracle. Never let the
                    // runner execute until that binding is durable too.
                    let cleanup = client.teardown(&handle).err();
                    if cleanup.is_none() {
                        self.delete_record(run_id);
                    }
                    return Err(error).with_context(|| {
                        format!(
                            "persisting phase environment binding for run {run_id}; prepared node cleanup: {}",
                            cleanup
                                .map(|error| format!("failed: {error:#}"))
                                .unwrap_or_else(|| "succeeded".to_string())
                        )
                    });
                }
                let response = (handle.workspace_root.clone(), handle.id.clone());
                self.inner.leases.lock().await.insert(
                    run_id.to_string(),
                    ReadyLease {
                        environment_id: environment_id.to_string(),
                        project_root: pending.project_root.clone(),
                        client,
                        handle,
                    },
                );
                Ok(response)
            }
            Err(error) => {
                // Mark failed + reap any partial node the plugin left behind, so
                // a failed prepare never leaks a durable record.
                self.write_record(run_id, environment_id, &pending.project_root, LeaseState::Failed, None);
                self.delete_record(run_id);
                Err(error)
            }
        }
    }

    /// Look up the run's lease, assert `handle_id` matches it (a runner can
    /// NEVER exec into another run's node), and return the pinned client +
    /// handle to drive `exec_stream` against. The lease lock is NOT held across
    /// the exec, so a long command does not block other broker ops.
    async fn exec_target(
        &self,
        run_id: &str,
        handle_id: &str,
    ) -> Result<(Arc<dyn EnvironmentLeaseClient>, EnvironmentHandle)> {
        let leases = self.inner.leases.lock().await;
        let lease =
            leases.get(run_id).ok_or_else(|| anyhow!("no prepared environment for run {run_id} (acquire first)"))?;
        if lease.handle.id != handle_id {
            bail!("handle '{handle_id}' does not match the lease for run {run_id}");
        }
        Ok((lease.client.clone(), lease.handle.clone()))
    }

    fn key_lock(&self, run_id: &str) -> Arc<AsyncMutex<()>> {
        self.inner
            .key_locks
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .entry(run_id.to_string())
            .or_insert_with(|| Arc::new(AsyncMutex::new(())))
            .clone()
    }

    // -- durable records ----------------------------------------------------

    fn record_path(&self, run_id: &str) -> PathBuf {
        self.inner.records_dir.join(format!("{}.json", protocol::sanitize_identifier(run_id, "run")))
    }

    fn write_record(
        &self,
        run_id: &str,
        environment_id: &str,
        project_root: &str,
        state: LeaseState,
        handle: Option<&EnvironmentHandle>,
    ) {
        let now = chrono::Utc::now().to_rfc3339();
        let record = LeaseRecord {
            run_id: run_id.to_string(),
            daemon_instance_id: self.inner.daemon_instance_id.clone(),
            environment_id: environment_id.to_string(),
            project_root: project_root.to_string(),
            state,
            handle: handle.cloned(),
            created_at: now.clone(),
            updated_at: now,
        };
        let path = self.record_path(run_id);
        if let Err(error) = write_record_atomic(&path, &record) {
            tracing::warn!(
                target: "animus.runtime.environment_broker",
                run_id,
                %error,
                "failed to persist environment lease record (best-effort; startup reap may miss this node)"
            );
        }
    }

    fn write_record_required(
        &self,
        run_id: &str,
        environment_id: &str,
        project_root: &str,
        state: LeaseState,
        handle: Option<&EnvironmentHandle>,
    ) -> std::io::Result<()> {
        let now = chrono::Utc::now().to_rfc3339();
        let record = LeaseRecord {
            run_id: run_id.to_string(),
            daemon_instance_id: self.inner.daemon_instance_id.clone(),
            environment_id: environment_id.to_string(),
            project_root: project_root.to_string(),
            state,
            handle: handle.cloned(),
            created_at: now.clone(),
            updated_at: now,
        };
        write_record_atomic(&self.record_path(run_id), &record)
    }

    fn delete_record(&self, run_id: &str) {
        let _ = std::fs::remove_file(self.record_path(run_id));
    }

    /// Reconcile every lease record owned by a prior daemon instance. Adopt an
    /// exact Ready lease still claimed by a Running checkpoint; otherwise use
    /// a fresh `resolve` + `teardown(handle)` to reclaim the leaked node and
    /// delete its record. Records owned by this instance are left untouched.
    async fn reap_prior_daemon_records(&self) {
        let entries = match std::fs::read_dir(&self.inner.records_dir) {
            Ok(entries) => entries,
            Err(_) => return,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let record: LeaseRecord =
                match std::fs::read_to_string(&path).ok().and_then(|raw| serde_json::from_str(&raw).ok()) {
                    Some(record) => record,
                    None => {
                        let _ = std::fs::remove_file(&path);
                        continue;
                    }
                };
            if record.daemon_instance_id == self.inner.daemon_instance_id {
                continue;
            }
            // A Running delegated checkpoint is a durable claim on this exact
            // node. Preserve it until startup reconciliation can probe/resume
            // it; otherwise broker startup destroys the node before TASK-933
            // recovery runs. Unclaimed records retain the cold-reap behavior.
            if prior_record_is_claimed_for_resume(&record) {
                let Some(handle) = record.handle.clone() else {
                    continue;
                };
                match (self.inner.client_resolver)(Path::new(&record.project_root), &record.environment_id) {
                    Ok(client) => {
                        if let Err(error) = self.write_record_required(
                            &record.run_id,
                            &record.environment_id,
                            &record.project_root,
                            LeaseState::Ready,
                            Some(&handle),
                        ) {
                            tracing::warn!(
                                target: "animus.runtime.environment_broker",
                                run_id = %record.run_id,
                                %error,
                                "startup adoption could not rewrite lease ownership; keeping prior durable record"
                            );
                            continue;
                        }
                        self.inner.leases.lock().await.insert(
                            record.run_id.clone(),
                            ReadyLease {
                                environment_id: record.environment_id.clone(),
                                project_root: record.project_root.clone(),
                                client,
                                handle: handle.clone(),
                            },
                        );
                        tracing::info!(
                            target: "animus.runtime.environment_broker",
                            run_id = %record.run_id,
                            node = %handle.id,
                            "startup adopted prior node claimed by a running delegated checkpoint"
                        );
                    }
                    Err(error) => {
                        tracing::warn!(
                            target: "animus.runtime.environment_broker",
                            run_id = %record.run_id,
                            %error,
                            "startup could not adopt claimed prior node; preserving durable record for retry"
                        );
                    }
                }
                continue;
            }
            if let Some(handle) = record.handle.clone() {
                let environment_id = record.environment_id.clone();
                let project_root = record.project_root.clone();
                let run_id = record.run_id.clone();
                // `resolve` + `teardown` bridge async→sync internally; call
                // directly (no outer `block_in_place`).
                let outcome = (|| {
                    let client = (self.inner.client_resolver)(Path::new(&project_root), &environment_id)?;
                    client.teardown(&handle)
                })();
                match outcome {
                    Err(error) => {
                        tracing::warn!(
                            target: "animus.runtime.environment_broker",
                            run_id = %run_id,
                            %error,
                            "startup reap: cold teardown failed; retaining the lease record for retry"
                        );
                        let client = match (self.inner.client_resolver)(Path::new(&project_root), &environment_id) {
                            Ok(client) => client,
                            Err(resolve_error) => {
                                tracing::warn!(
                                    target: "animus.runtime.environment_broker",
                                    run_id = %run_id,
                                    %resolve_error,
                                    "startup reap: failed cleanup could not be adopted for housekeeping retry"
                                );
                                continue;
                            }
                        };
                        if let Err(write_error) = self.write_record_required(
                            &run_id,
                            &environment_id,
                            &project_root,
                            LeaseState::TearingDown,
                            Some(&handle),
                        ) {
                            tracing::warn!(
                                target: "animus.runtime.environment_broker",
                                run_id = %run_id,
                                %write_error,
                                "startup reap: failed cleanup could not transfer durable ownership"
                            );
                            continue;
                        }
                        self.inner
                            .leases
                            .lock()
                            .await
                            .insert(run_id.clone(), ReadyLease { environment_id, project_root, client, handle });
                        continue;
                    }
                    Ok(()) => {
                        tracing::info!(
                            target: "animus.runtime.environment_broker",
                            run_id = %run_id,
                            "startup reap: cold tore down a node leaked by a prior daemon instance"
                        );
                        if let Some(scoped_root) =
                            protocol::repository_scope::scoped_state_root(Path::new(&project_root))
                        {
                            if let Err(error) =
                                animus_runtime_shared::phase_session::mark_workflow_environment_torn_down(
                                    &scoped_root,
                                    &run_id,
                                    &environment_id,
                                    &handle,
                                )
                            {
                                tracing::warn!(
                                    target: "animus.runtime.environment_broker",
                                    run_id = %run_id,
                                    %error,
                                    "startup reap succeeded but checkpoint binding could not be marked torn down"
                                );
                            }
                        }
                    }
                }
            }
            let _ = std::fs::remove_file(&path);
        }
    }
}

/// Persist the production broker's prepared handle into the phase checkpoint
/// before the acquire response exposes the node to a runner. A workflow has at
/// most one Running phase checkpoint; refusing ambiguous/missing ownership
/// closes the prepare-to-persistence crash window instead of executing work
/// that restart reconciliation cannot resume or reap by handle.
fn bind_running_phase_checkpoint(
    project_root: &str,
    run_id: &str,
    environment_id: &str,
    handle: &EnvironmentHandle,
) -> Result<()> {
    let scoped_root = protocol::repository_scope::scoped_state_root(Path::new(project_root))
        .ok_or_else(|| anyhow!("project has no scoped state root"))?;
    let mut matches = list_running_checkpoints(&scoped_root)
        .context("listing running phase checkpoints")?
        .into_iter()
        .map(|(_, checkpoint)| checkpoint)
        .filter(|checkpoint| checkpoint.workflow_id == run_id);
    let checkpoint =
        matches.next().ok_or_else(|| anyhow!("no Running phase checkpoint found for brokered workflow"))?;
    if matches.next().is_some() {
        bail!("multiple Running phase checkpoints found for brokered workflow");
    }
    update_session_environment(
        &scoped_root,
        &checkpoint.workflow_id,
        &checkpoint.phase_id,
        EnvironmentBinding {
            environment_id: environment_id.to_string(),
            handle: handle.clone(),
            bound_at: chrono::Utc::now().to_rfc3339(),
            torn_down: false,
        },
    )
    .context("writing delegated environment binding")
}

fn prior_record_is_claimed_for_resume(record: &LeaseRecord) -> bool {
    // Only a fully prepared lease can be resumed. In particular, preserving a
    // TearingDown record would allow already-completed coding work to be
    // reattached after restart instead of letting startup finish reaping it.
    if record.state != LeaseState::Ready {
        return false;
    }
    let Some(handle) = record.handle.as_ref() else {
        return false;
    };
    let Some(scoped_root) = protocol::repository_scope::scoped_state_root(Path::new(&record.project_root)) else {
        return false;
    };
    animus_runtime_shared::phase_session::list_running_checkpoints(&scoped_root).ok().into_iter().flatten().any(
        |(_, checkpoint)| {
            checkpoint.workflow_id == record.run_id
                && checkpoint.environment.as_ref().is_some_and(|binding| {
                    !binding.torn_down && binding.environment_id == record.environment_id && binding.handle == *handle
                })
        },
    )
}

// ---------------------------------------------------------------------------
// Socket server.
// ---------------------------------------------------------------------------

fn bind_listener(socket_path: &str) -> std::io::Result<interprocess::local_socket::tokio::Listener> {
    use animus_runtime_shared::reattach::local_socket_name_for;
    use interprocess::local_socket::ListenerOptions;

    if looks_like_filesystem(socket_path) && Path::new(socket_path).exists() {
        // Clear a corpse socket from a prior run so bind succeeds.
        let _ = std::fs::remove_file(socket_path);
    }
    let name = local_socket_name_for(socket_path)?;
    let listener = ListenerOptions::new().name(name).create_tokio()?;

    #[cfg(unix)]
    if looks_like_filesystem(socket_path) {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = std::fs::metadata(socket_path) {
            let mut perms = meta.permissions();
            perms.set_mode(0o600);
            let _ = std::fs::set_permissions(socket_path, perms);
        }
    }
    Ok(listener)
}

async fn accept_loop(listener: interprocess::local_socket::tokio::Listener, broker: EnvironmentBroker) {
    use interprocess::local_socket::traits::tokio::Listener as _;
    loop {
        let conn = match listener.accept().await {
            Ok(conn) => conn,
            Err(error) => {
                tracing::debug!(
                    target: "animus.runtime.environment_broker",
                    %error,
                    "environment broker accept loop exiting on accept error"
                );
                return;
            }
        };
        let conn_broker = broker.clone();
        tokio::spawn(async move {
            if let Err(error) = handle_connection(conn, conn_broker).await {
                tracing::debug!(
                    target: "animus.runtime.environment_broker",
                    %error,
                    "environment broker connection handler exited with error"
                );
            }
        });
    }
}

async fn handle_connection(
    conn: interprocess::local_socket::tokio::Stream,
    broker: EnvironmentBroker,
) -> std::io::Result<()> {
    use tokio::io::{AsyncBufReadExt, BufReader};

    let mut reader = BufReader::new(&conn);
    let mut line = String::new();
    if reader.read_line(&mut line).await? == 0 {
        return Ok(());
    }

    let request: BrokerRequest = match serde_json::from_str(line.trim()) {
        Ok(request) => request,
        Err(error) => {
            write_json_line(&conn, &json!({ "ok": false, "error": format!("malformed broker request: {error}") }))
                .await?;
            return Ok(());
        }
    };

    match request {
        BrokerRequest::Acquire { token, run_id, environment_id, spec } => {
            if token != broker.token() {
                write_json_line(&conn, &json!({ "ok": false, "error": "unauthorized" })).await?;
                return Ok(());
            }
            match broker.acquire(&run_id, &environment_id, spec).await {
                Ok((workspace_root, handle_id)) => {
                    write_json_line(
                        &conn,
                        &json!({ "ok": true, "workspace_root": workspace_root, "handle_id": handle_id }),
                    )
                    .await?;
                }
                Err(error) => {
                    write_json_line(&conn, &json!({ "ok": false, "error": error.to_string() })).await?;
                }
            }
        }
        BrokerRequest::Exec { token, run_id, handle_id, command, stdin, timeout_secs } => {
            if token != broker.token() {
                write_json_line(&conn, &json!({ "error": "unauthorized" })).await?;
                return Ok(());
            }
            handle_exec(&conn, broker, run_id, handle_id, command, stdin, timeout_secs).await?;
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn handle_exec(
    conn: &interprocess::local_socket::tokio::Stream,
    broker: EnvironmentBroker,
    run_id: String,
    handle_id: String,
    command: HarnessCommand,
    stdin: Option<String>,
    timeout_secs: Option<u64>,
) -> std::io::Result<()> {
    let (client, handle) = match broker.exec_target(&run_id, &handle_id).await {
        Ok(target) => target,
        Err(error) => {
            return write_json_line(conn, &json!({ "error": error.to_string() })).await;
        }
    };

    // Forward incremental output through a channel so the streamed exec (a
    // blocking RPC that bridges async→sync internally) and the socket writer run
    // concurrently: `exec_stream`'s own `block_in_place` hands off the runtime
    // worker for the command's duration while this task's writer loop drains rx.
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<serde_json::Value>();
    let timeout = timeout_secs.map(Duration::from_secs);
    let exec_task = tokio::spawn(async move {
        let on_output = |stream: ExecStream, text: &str| {
            let _ = tx.send(json!({ "out": exec_stream_str(stream), "text": text }));
        };
        client.exec_stream(&handle, command, stdin, timeout, &on_output)
    });

    while let Some(frame) = rx.recv().await {
        write_json_line(conn, &frame).await?;
    }

    match exec_task.await {
        Ok(Ok(response)) => {
            let done = serde_json::to_value(&response).unwrap_or(serde_json::Value::Null);
            write_json_line(conn, &json!({ "done": done })).await?;
        }
        Ok(Err(error)) => {
            write_json_line(conn, &json!({ "error": error.to_string() })).await?;
        }
        Err(join_error) => {
            write_json_line(conn, &json!({ "error": format!("exec task panicked: {join_error}") })).await?;
        }
    }
    Ok(())
}

async fn write_json_line(
    mut conn: &interprocess::local_socket::tokio::Stream,
    value: &serde_json::Value,
) -> std::io::Result<()> {
    use tokio::io::AsyncWriteExt;
    let mut bytes = serde_json::to_vec(value).unwrap_or_default();
    bytes.push(b'\n');
    conn.write_all(&bytes).await?;
    conn.flush().await
}

fn exec_stream_str(stream: ExecStream) -> &'static str {
    match stream {
        ExecStream::Stdout => "stdout",
        ExecStream::Stderr => "stderr",
    }
}

// ---------------------------------------------------------------------------
// Helpers.
// ---------------------------------------------------------------------------

fn looks_like_filesystem(value: &str) -> bool {
    value.contains(std::path::MAIN_SEPARATOR) || value.contains('/')
}

/// Inject `metadata.animus_run_id = run_id` so a deterministic-naming plugin
/// (e.g. railway) names the node stably per run. Preserves any existing
/// metadata object; replaces a non-object metadata value.
fn set_run_id_metadata(spec: &mut EnvironmentSpec, run_id: &str) {
    match spec.metadata.as_object_mut() {
        Some(map) => {
            map.insert(ANIMUS_RUN_ID_METADATA_KEY.to_string(), json!(run_id));
        }
        None => {
            spec.metadata = json!({ ANIMUS_RUN_ID_METADATA_KEY: run_id });
        }
    }
}

/// Directory holding the broker's durable records and (length permitting) its
/// socket. Scoped state root when resolvable; a `$TMPDIR` fallback otherwise
/// (tests / non-git contexts) so the broker still works without cross-restart
/// reaping.
fn broker_records_dir(project_root: &str) -> PathBuf {
    protocol::scoped_state_root(Path::new(project_root)).map(|root| root.join("workflow-environments")).unwrap_or_else(
        || std::env::temp_dir().join("animus-workflow-environments").join(std::process::id().to_string()),
    )
}

/// Conservative Unix-domain-socket path cap (SUN_LEN is ~104 on macOS, 108 on
/// Linux); leave headroom for platform quirks.
const MAX_UNIX_SOCKET_PATH_BYTES: usize = 100;

/// Pick a socket path under `records_dir`, falling back to `$TMPDIR` when the
/// canonical path would exceed the SUN_LEN budget.
fn broker_socket_path(records_dir: &Path) -> String {
    let canonical = records_dir.join("broker.sock");
    if canonical.as_os_str().len() <= MAX_UNIX_SOCKET_PATH_BYTES {
        return canonical.to_string_lossy().into_owned();
    }
    std::env::temp_dir()
        .join("animus-env-broker")
        .join(std::process::id().to_string())
        .join("broker.sock")
        .to_string_lossy()
        .into_owned()
}

/// Ensure a filesystem-backed local socket can be bound. The short-path
/// fallback may live outside `records_dir`, so creating only that directory is
/// insufficient on a clean machine.
fn ensure_socket_parent(socket_path: &str) -> std::io::Result<()> {
    if looks_like_filesystem(socket_path) {
        if let Some(parent) = Path::new(socket_path).parent() {
            std::fs::create_dir_all(parent)?;
        }
    }
    Ok(())
}

fn write_record_atomic(path: &Path, record: &LeaseRecord) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("json.tmp");
    let bytes = serde_json::to_vec_pretty(record).map_err(std::io::Error::other)?;
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Condvar, Mutex,
    };

    struct FakeLeaseClient {
        handle: EnvironmentHandle,
        prepares: AtomicUsize,
        execs: AtomicUsize,
        teardowns: AtomicUsize,
        teardown_failures_remaining: AtomicUsize,
    }

    impl EnvironmentLeaseClient for FakeLeaseClient {
        fn prepare(&self, _spec: EnvironmentSpec) -> Result<EnvironmentHandle> {
            self.prepares.fetch_add(1, Ordering::SeqCst);
            Ok(self.handle.clone())
        }

        fn exec_stream(
            &self,
            _handle: &EnvironmentHandle,
            _command: HarnessCommand,
            _stdin: Option<String>,
            _timeout: Option<Duration>,
            _on_output: &(dyn Fn(ExecStream, &str) + Send + Sync),
        ) -> Result<ExecResponse> {
            self.execs.fetch_add(1, Ordering::SeqCst);
            Ok(ExecResponse { exit_code: Some(0), stdout: String::new(), stderr: String::new(), timed_out: false })
        }

        fn teardown(&self, _handle: &EnvironmentHandle) -> Result<()> {
            self.teardowns.fetch_add(1, Ordering::SeqCst);
            if self
                .teardown_failures_remaining
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| remaining.checked_sub(1))
                .is_ok()
            {
                bail!("injected teardown failure");
            }
            Ok(())
        }
    }

    struct BlockingFailTeardownClient {
        handle: EnvironmentHandle,
        teardown_calls: AtomicUsize,
        entered: (Mutex<bool>, Condvar),
        release: (Mutex<bool>, Condvar),
    }

    impl BlockingFailTeardownClient {
        fn wait_until_teardown_entered(&self) {
            let (entered, wake) = &self.entered;
            let mut entered = entered.lock().expect("entered mutex");
            while !*entered {
                entered = wake.wait(entered).expect("entered condvar");
            }
        }

        fn release_teardown(&self) {
            let (release, wake) = &self.release;
            *release.lock().expect("release mutex") = true;
            wake.notify_all();
        }
    }

    impl EnvironmentLeaseClient for BlockingFailTeardownClient {
        fn prepare(&self, _spec: EnvironmentSpec) -> Result<EnvironmentHandle> {
            Ok(self.handle.clone())
        }

        fn exec_stream(
            &self,
            _handle: &EnvironmentHandle,
            _command: HarnessCommand,
            _stdin: Option<String>,
            _timeout: Option<Duration>,
            _on_output: &(dyn Fn(ExecStream, &str) + Send + Sync),
        ) -> Result<ExecResponse> {
            unreachable!("concurrent teardown regression does not exec")
        }

        fn teardown(&self, _handle: &EnvironmentHandle) -> Result<()> {
            self.teardown_calls.fetch_add(1, Ordering::SeqCst);
            let (entered, entered_wake) = &self.entered;
            *entered.lock().expect("entered mutex") = true;
            entered_wake.notify_all();

            let (release, release_wake) = &self.release;
            let mut release = release.lock().expect("release mutex");
            while !*release {
                release = release_wake.wait(release).expect("release condvar");
            }
            bail!("injected teardown failure")
        }
    }

    #[test]
    fn local_environment_ids_are_recognized() {
        assert!(is_local_environment("worktree"));
        assert!(is_local_environment("local"));
        assert!(!is_local_environment("railway"));
        assert!(!is_local_environment("animus-environment-railway"));
    }

    #[test]
    fn set_run_id_metadata_preserves_existing_object() {
        let mut spec = EnvironmentSpec {
            kind: "railway".to_string(),
            repos: Vec::new(),
            image: None,
            resources: None,
            env: std::collections::BTreeMap::new(),
            metadata: json!({ "region": "us-west" }),
        };
        set_run_id_metadata(&mut spec, "wf-abc");
        assert_eq!(spec.metadata["animus_run_id"], json!("wf-abc"));
        assert_eq!(spec.metadata["region"], json!("us-west"));
    }

    #[test]
    fn set_run_id_metadata_replaces_null_metadata() {
        let mut spec = EnvironmentSpec {
            kind: "railway".to_string(),
            repos: Vec::new(),
            image: None,
            resources: None,
            env: std::collections::BTreeMap::new(),
            metadata: serde_json::Value::Null,
        };
        set_run_id_metadata(&mut spec, "wf-xyz");
        assert_eq!(spec.metadata["animus_run_id"], json!("wf-xyz"));
    }

    #[test]
    fn acquire_request_parses_from_wire() {
        let line =
            r#"{"op":"acquire","token":"t","run_id":"wf-1","environment_id":"railway","spec":{"kind":"railway"}}"#;
        let request: BrokerRequest = serde_json::from_str(line).expect("parse acquire");
        match request {
            BrokerRequest::Acquire { token, run_id, environment_id, spec } => {
                assert_eq!(token, "t");
                assert_eq!(run_id, "wf-1");
                assert_eq!(environment_id, "railway");
                assert_eq!(spec.kind, "railway");
            }
            BrokerRequest::Exec { .. } => panic!("expected acquire"),
        }
    }

    #[test]
    fn exec_request_parses_from_wire() {
        let line = r#"{"op":"exec","token":"t","run_id":"wf-1","handle_id":"h-1","command":{"program":"echo","args":["hi"]},"stdin":null,"timeout_secs":30}"#;
        let request: BrokerRequest = serde_json::from_str(line).expect("parse exec");
        match request {
            BrokerRequest::Exec { run_id, handle_id, command, timeout_secs, .. } => {
                assert_eq!(run_id, "wf-1");
                assert_eq!(handle_id, "h-1");
                assert_eq!(command.program, "echo");
                assert_eq!(timeout_secs, Some(30));
            }
            BrokerRequest::Acquire { .. } => panic!("expected exec"),
        }
    }

    #[test]
    fn lease_record_round_trips() {
        let record = LeaseRecord {
            run_id: "wf-1".to_string(),
            daemon_instance_id: "daemon-a".to_string(),
            environment_id: "railway".to_string(),
            project_root: "/tmp/project".to_string(),
            state: LeaseState::Ready,
            handle: Some(EnvironmentHandle {
                id: "node-1".to_string(),
                workspace_root: "/workspace".to_string(),
                metadata: serde_json::Value::Null,
            }),
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-01T00:00:01Z".to_string(),
        };
        let json = serde_json::to_string(&record).expect("serialize");
        let decoded: LeaseRecord = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(decoded.run_id, "wf-1");
        assert_eq!(decoded.state, LeaseState::Ready);
        assert_eq!(decoded.handle.unwrap().id, "node-1");
    }

    #[test]
    fn broker_binding_is_written_to_the_running_phase_checkpoint() {
        use animus_runtime_shared::phase_session::{read_checkpoint, update_session_running, write_session_pending};

        let temp = tempfile::tempdir().expect("tempdir");
        let project_root = temp.path().to_string_lossy();
        let scoped_root =
            protocol::repository_scope::scoped_state_root(temp.path()).expect("scoped project state root");
        write_session_pending(
            &scoped_root,
            "wf-bound",
            "implementation",
            "claude",
            "agent-run",
            Some(json!({"context": {"prompt": "continue"}})),
        )
        .expect("pending checkpoint");
        update_session_running(&scoped_root, "wf-bound", "implementation").expect("running checkpoint");
        let handle = EnvironmentHandle {
            id: "node-bound".to_string(),
            workspace_root: "/workspace".to_string(),
            metadata: json!({"relay": "opaque"}),
        };

        bind_running_phase_checkpoint(&project_root, "wf-bound", "railway", &handle).expect("persist broker binding");

        let checkpoint = read_checkpoint(&scoped_root, "wf-bound", "implementation")
            .expect("read checkpoint")
            .expect("checkpoint exists");
        let binding = checkpoint.environment.expect("environment binding");
        assert_eq!(binding.environment_id, "railway");
        assert_eq!(binding.handle, handle);
        assert!(!binding.torn_down);
    }

    #[test]
    fn only_ready_prior_record_is_claimed_for_resume() {
        use animus_runtime_shared::phase_session::{update_session_running, write_session_pending};

        let temp = tempfile::tempdir().expect("tempdir");
        let project_root = temp.path().to_string_lossy().into_owned();
        let scoped_root =
            protocol::repository_scope::scoped_state_root(temp.path()).expect("scoped project state root");
        write_session_pending(&scoped_root, "wf-resume", "implementation", "claude", "agent-run", None)
            .expect("pending checkpoint");
        update_session_running(&scoped_root, "wf-resume", "implementation").expect("running checkpoint");
        let handle = EnvironmentHandle {
            id: "node-resume".to_string(),
            workspace_root: "/workspace".to_string(),
            metadata: json!({"relay": "opaque"}),
        };
        bind_running_phase_checkpoint(&project_root, "wf-resume", "railway", &handle).expect("persist broker binding");
        let record = LeaseRecord {
            run_id: "wf-resume".to_string(),
            daemon_instance_id: "prior-daemon".to_string(),
            environment_id: "railway".to_string(),
            project_root,
            state: LeaseState::Ready,
            handle: Some(handle),
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-01T00:00:01Z".to_string(),
        };

        assert!(prior_record_is_claimed_for_resume(&record));
        for state in [LeaseState::Preparing, LeaseState::TearingDown, LeaseState::TornDown, LeaseState::Failed] {
            let mut non_ready = record.clone();
            non_ready.state = state;
            assert!(!prior_record_is_claimed_for_resume(&non_ready), "{state:?} record was claimed");
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn restart_adopts_exact_lease_reuses_it_for_next_phase_and_tears_down_once() {
        use animus_runtime_shared::phase_session::{
            read_checkpoint, update_session_completed, update_session_running, write_session_pending,
        };

        let temp = tempfile::tempdir().expect("tempdir");
        let project_root = temp.path().to_string_lossy().into_owned();
        let scoped_root =
            protocol::repository_scope::scoped_state_root(temp.path()).expect("scoped project state root");
        write_session_pending(&scoped_root, "wf-restart", "code-implement", "claude", "agent-1", None)
            .expect("first pending checkpoint");
        update_session_running(&scoped_root, "wf-restart", "code-implement").expect("first running checkpoint");

        let fake = Arc::new(FakeLeaseClient {
            handle: EnvironmentHandle {
                id: "node-h1".to_string(),
                workspace_root: "/workspace".to_string(),
                metadata: json!({"relay": "opaque"}),
            },
            prepares: AtomicUsize::new(0),
            execs: AtomicUsize::new(0),
            teardowns: AtomicUsize::new(0),
            teardown_failures_remaining: AtomicUsize::new(0),
        });
        let resolver: Arc<ClientResolver> = {
            let fake = fake.clone();
            Arc::new(move |_, _| Ok(fake.clone()))
        };

        let first =
            EnvironmentBroker::start_with_resolver(&project_root, resolver.clone()).await.expect("start first broker");
        first.register_run("wf-restart", &project_root, "railway");
        let spec = EnvironmentSpec {
            kind: "railway".to_string(),
            repos: Vec::new(),
            image: None,
            resources: None,
            env: std::collections::BTreeMap::new(),
            metadata: serde_json::Value::Null,
        };
        let (_, first_handle) = first.acquire("wf-restart", "railway", spec.clone()).await.expect("first acquire");
        assert_eq!(first_handle, "node-h1");
        assert_eq!(fake.prepares.load(Ordering::SeqCst), 1);

        // Simulate daemon replacement without terminal cleanup: the durable
        // Ready record + Running checkpoint remain, while the old broker socket
        // disappears with its process.
        first.stop_acceptor_for_restart_test();
        drop(first);
        let replacement =
            EnvironmentBroker::start_with_resolver(&project_root, resolver).await.expect("start replacement broker");
        assert!(
            replacement.owns_ready_lease("wf-restart", "railway", &fake.handle).await,
            "replacement daemon adopts the exact persisted handle"
        );
        let (resume_client, resume_handle) =
            replacement.exec_target("wf-restart", "node-h1").await.expect("adopted lease is executable");
        resume_client
            .exec_stream(
                &resume_handle,
                HarnessCommand {
                    program: "resume-provider".to_string(),
                    args: Vec::new(),
                    env: std::collections::BTreeMap::new(),
                    cwd: None,
                },
                None,
                None,
                &|_, _| {},
            )
            .expect("resume non-terminal phase on adopted lease");
        assert_eq!(fake.execs.load(Ordering::SeqCst), 1);
        assert_eq!(fake.teardowns.load(Ordering::SeqCst), 0, "resumed phase must not teardown workflow lease");

        // The resumed phase completes and the workflow advances. The next
        // phase's acquire must reuse H1 and durably bind that new checkpoint.
        update_session_completed(&scoped_root, "wf-restart", "code-implement").expect("complete first phase");
        write_session_pending(&scoped_root, "wf-restart", "code-check", "codex", "agent-2", None)
            .expect("second pending checkpoint");
        update_session_running(&scoped_root, "wf-restart", "code-check").expect("second running checkpoint");
        replacement.register_run("wf-restart", &project_root, "railway");
        let (_, second_handle) =
            replacement.acquire("wf-restart", "railway", spec).await.expect("second phase acquire");
        assert_eq!(second_handle, "node-h1");
        assert_eq!(fake.prepares.load(Ordering::SeqCst), 1, "no replacement node was prepared");
        let second_checkpoint = read_checkpoint(&scoped_root, "wf-restart", "code-check")
            .expect("read second checkpoint")
            .expect("second checkpoint exists");
        assert_eq!(second_checkpoint.environment.expect("second phase binding").handle.id, "node-h1");

        assert!(replacement.teardown("wf-restart").await);
        assert!(replacement.teardown("wf-restart").await);
        assert_eq!(fake.teardowns.load(Ordering::SeqCst), 1, "terminal cleanup tears the adopted lease down once");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn teardown_is_idempotent_without_a_lease() {
        let temp = tempfile::tempdir().expect("tempdir");
        let broker = EnvironmentBroker::start(temp.path().to_string_lossy().as_ref()).await.expect("start broker");
        // No lease for this run: teardown must be a clean no-op.
        assert!(broker.teardown("wf-missing").await);
        assert!(broker.teardown("wf-missing").await);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn failed_teardown_retains_lease_and_record_for_retry() {
        use animus_runtime_shared::phase_session::{
            mark_workflow_environment_torn_down, read_checkpoint, update_session_environment, write_session_pending,
            EnvironmentBinding,
        };

        let temp = tempfile::tempdir().expect("tempdir");
        let project_root = temp.path().to_string_lossy().into_owned();
        let scoped_root =
            protocol::repository_scope::scoped_state_root(temp.path()).expect("scoped project state root");
        let fake = Arc::new(FakeLeaseClient {
            handle: EnvironmentHandle {
                id: "node-retry".to_string(),
                workspace_root: "/workspace".to_string(),
                metadata: json!({"relay": "opaque"}),
            },
            prepares: AtomicUsize::new(0),
            execs: AtomicUsize::new(0),
            teardowns: AtomicUsize::new(0),
            teardown_failures_remaining: AtomicUsize::new(1),
        });
        let resolver: Arc<ClientResolver> = {
            let fake = fake.clone();
            Arc::new(move |_, _| Ok(fake.clone()))
        };
        let broker = EnvironmentBroker::start_with_resolver(&project_root, resolver).await.expect("start broker");
        broker.register_run("wf-retry", &project_root, "railway");
        let spec = EnvironmentSpec {
            kind: "railway".to_string(),
            repos: Vec::new(),
            image: None,
            resources: None,
            env: std::collections::BTreeMap::new(),
            metadata: serde_json::Value::Null,
        };
        broker.acquire("wf-retry", "railway", spec).await.expect("prepare lease");
        let historical_handle = EnvironmentHandle {
            id: "node-historical".to_string(),
            workspace_root: "/old-workspace".to_string(),
            metadata: json!({"relay": "old"}),
        };
        for (phase_id, handle) in [("old-phase", historical_handle.clone()), ("current-phase", fake.handle.clone())] {
            write_session_pending(&scoped_root, "wf-retry", phase_id, "claude", phase_id, None)
                .expect("pending checkpoint");
            update_session_environment(
                &scoped_root,
                "wf-retry",
                phase_id,
                EnvironmentBinding {
                    environment_id: "railway".to_string(),
                    handle,
                    bound_at: chrono::Utc::now().to_rfc3339(),
                    torn_down: false,
                },
            )
            .expect("environment binding");
        }

        assert!(!broker.teardown("wf-retry").await, "failed RPC must remain retryable");
        assert!(broker.record_path("wf-retry").exists(), "failed cleanup must retain its durable recovery record");
        assert!(
            broker.owns_ready_lease("wf-retry", "railway", &fake.handle).await,
            "failed cleanup must restore the in-memory lease"
        );

        let retried = broker.retry_failed_teardowns().await;
        assert_eq!(
            retried,
            vec![RetriedEnvironmentTeardown {
                run_id: "wf-retry".to_string(),
                environment_id: "railway".to_string(),
                project_root: project_root.clone(),
                handle: fake.handle.clone(),
            }],
            "next ordinary cleanup sweep retries the plugin RPC"
        );
        for cleanup in &retried {
            mark_workflow_environment_torn_down(
                &scoped_root,
                &cleanup.run_id,
                &cleanup.environment_id,
                &cleanup.handle,
            )
            .expect("mark exact retried binding");
        }
        let old_checkpoint = read_checkpoint(&scoped_root, "wf-retry", "old-phase")
            .expect("read old checkpoint")
            .expect("old checkpoint");
        let current_checkpoint = read_checkpoint(&scoped_root, "wf-retry", "current-phase")
            .expect("read current checkpoint")
            .expect("current checkpoint");
        assert!(
            !old_checkpoint.environment.expect("old binding").torn_down,
            "retrying one node must not mark another historical binding"
        );
        assert!(
            current_checkpoint.environment.expect("current binding").torn_down,
            "the binding matching the successful retry must be marked torn down"
        );
        assert!(!broker.record_path("wf-retry").exists(), "successful retry removes the durable record");
        assert!(
            !broker.owns_ready_lease("wf-retry", "railway", &fake.handle).await,
            "successful retry removes the in-memory lease"
        );
        assert_eq!(fake.teardowns.load(Ordering::SeqCst), 2);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn failed_startup_reap_is_adopted_for_housekeeping_retry() {
        let temp = tempfile::tempdir().expect("tempdir");
        let project_root = temp.path().to_string_lossy().into_owned();
        let fake = Arc::new(FakeLeaseClient {
            handle: EnvironmentHandle {
                id: "node-startup-retry".to_string(),
                workspace_root: "/workspace".to_string(),
                metadata: json!({"relay": "opaque"}),
            },
            prepares: AtomicUsize::new(0),
            execs: AtomicUsize::new(0),
            teardowns: AtomicUsize::new(0),
            teardown_failures_remaining: AtomicUsize::new(1),
        });
        let resolver: Arc<ClientResolver> = {
            let fake = fake.clone();
            Arc::new(move |_, _| Ok(fake.clone()))
        };
        let first =
            EnvironmentBroker::start_with_resolver(&project_root, resolver.clone()).await.expect("start first broker");
        first.write_record("wf-startup-retry", "railway", &project_root, LeaseState::TearingDown, Some(&fake.handle));
        first.stop_acceptor_for_restart_test();

        let replacement =
            EnvironmentBroker::start_with_resolver(&project_root, resolver).await.expect("start replacement broker");
        let retried = replacement.retry_failed_teardowns().await;

        assert_eq!(
            retried,
            vec![RetriedEnvironmentTeardown {
                run_id: "wf-startup-retry".to_string(),
                environment_id: "railway".to_string(),
                project_root: project_root.clone(),
                handle: fake.handle.clone(),
            }]
        );
        assert_eq!(fake.teardowns.load(Ordering::SeqCst), 2);
        assert!(
            !replacement.record_path("wf-startup-retry").exists(),
            "successful housekeeping retry removes the adopted startup record"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn successful_startup_reap_marks_the_exact_checkpoint_binding_torn_down() {
        use animus_runtime_shared::phase_session::{
            read_checkpoint, update_session_environment, write_session_pending, EnvironmentBinding,
        };

        let temp = tempfile::tempdir().expect("tempdir");
        let project_root = temp.path().to_string_lossy().into_owned();
        let scoped_root =
            protocol::repository_scope::scoped_state_root(temp.path()).expect("scoped project state root");
        let handle = EnvironmentHandle {
            id: "node-startup-success".to_string(),
            workspace_root: "/workspace".to_string(),
            metadata: json!({"relay": "opaque"}),
        };
        write_session_pending(&scoped_root, "wf-startup-success", "code", "claude", "agent", None)
            .expect("pending checkpoint");
        update_session_environment(
            &scoped_root,
            "wf-startup-success",
            "code",
            EnvironmentBinding {
                environment_id: "railway".to_string(),
                handle: handle.clone(),
                bound_at: chrono::Utc::now().to_rfc3339(),
                torn_down: false,
            },
        )
        .expect("binding");

        let fake = Arc::new(FakeLeaseClient {
            handle: handle.clone(),
            prepares: AtomicUsize::new(0),
            execs: AtomicUsize::new(0),
            teardowns: AtomicUsize::new(0),
            teardown_failures_remaining: AtomicUsize::new(0),
        });
        let resolver: Arc<ClientResolver> = {
            let fake = fake.clone();
            Arc::new(move |_, _| Ok(fake.clone()))
        };
        let first =
            EnvironmentBroker::start_with_resolver(&project_root, resolver.clone()).await.expect("start first broker");
        first.write_record("wf-startup-success", "railway", &project_root, LeaseState::TearingDown, Some(&handle));
        first.stop_acceptor_for_restart_test();

        let _replacement =
            EnvironmentBroker::start_with_resolver(&project_root, resolver).await.expect("start replacement broker");
        let checkpoint =
            read_checkpoint(&scoped_root, "wf-startup-success", "code").expect("read checkpoint").expect("checkpoint");
        assert!(checkpoint.environment.expect("binding").torn_down);
        assert_eq!(fake.teardowns.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_teardown_waits_for_failed_rpc_before_reporting_result() {
        let temp = tempfile::tempdir().expect("tempdir");
        let project_root = temp.path().to_string_lossy().into_owned();
        let fake = Arc::new(BlockingFailTeardownClient {
            handle: EnvironmentHandle {
                id: "node-concurrent-retry".to_string(),
                workspace_root: "/workspace".to_string(),
                metadata: json!({"relay": "opaque"}),
            },
            teardown_calls: AtomicUsize::new(0),
            entered: (Mutex::new(false), Condvar::new()),
            release: (Mutex::new(false), Condvar::new()),
        });
        let resolver: Arc<ClientResolver> = {
            let fake = fake.clone();
            Arc::new(move |_, _| Ok(fake.clone()))
        };
        let broker = EnvironmentBroker::start_with_resolver(&project_root, resolver).await.expect("start broker");
        broker.register_run("wf-concurrent-retry", &project_root, "railway");
        let spec = EnvironmentSpec {
            kind: "railway".to_string(),
            repos: Vec::new(),
            image: None,
            resources: None,
            env: std::collections::BTreeMap::new(),
            metadata: serde_json::Value::Null,
        };
        broker.acquire("wf-concurrent-retry", "railway", spec).await.expect("prepare lease");

        let first_broker = broker.clone();
        let first = tokio::spawn(async move { first_broker.teardown("wf-concurrent-retry").await });
        let wait_fake = fake.clone();
        tokio::task::spawn_blocking(move || wait_fake.wait_until_teardown_entered())
            .await
            .expect("wait for first teardown");

        let second_broker = broker.clone();
        let mut second = tokio::spawn(async move { second_broker.teardown("wf-concurrent-retry").await });
        assert!(
            tokio::time::timeout(Duration::from_millis(100), &mut second).await.is_err(),
            "a concurrent caller must not report a no-lease success while teardown is in flight"
        );

        fake.release_teardown();
        assert!(!first.await.expect("first teardown task"), "the first failed RPC remains retryable");
        assert!(
            !second.await.expect("second teardown task"),
            "the serialized caller retries the retained lease instead of reporting false success"
        );
        assert_eq!(fake.teardown_calls.load(Ordering::SeqCst), 2);
        assert!(
            broker.record_path("wf-concurrent-retry").exists(),
            "both failed attempts retain the durable cleanup record"
        );
        assert!(
            broker.owns_ready_lease("wf-concurrent-retry", "railway", &fake.handle).await,
            "both failed attempts leave the delegated environment live and retryable"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn acquire_rejects_unregistered_run() {
        let temp = tempfile::tempdir().expect("tempdir");
        let broker = EnvironmentBroker::start(temp.path().to_string_lossy().as_ref()).await.expect("start broker");
        let spec = EnvironmentSpec {
            kind: "railway".to_string(),
            repos: Vec::new(),
            image: None,
            resources: None,
            env: std::collections::BTreeMap::new(),
            metadata: serde_json::Value::Null,
        };
        let err = broker.acquire("wf-unregistered", "railway", spec).await.expect_err("must reject");
        assert!(err.to_string().contains("no pending environment context"), "got: {err}");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn acquire_rejects_environment_mismatch_with_registration() {
        let temp = tempfile::tempdir().expect("tempdir");
        let broker = EnvironmentBroker::start(temp.path().to_string_lossy().as_ref()).await.expect("start broker");
        broker.register_run("wf-1", temp.path().to_string_lossy().as_ref(), "railway");
        let spec = EnvironmentSpec {
            kind: "container".to_string(),
            repos: Vec::new(),
            image: None,
            resources: None,
            env: std::collections::BTreeMap::new(),
            metadata: serde_json::Value::Null,
        };
        let err = broker.acquire("wf-1", "container", spec).await.expect_err("must reject mismatch");
        assert!(err.to_string().contains("is bound to environment 'railway'"), "got: {err}");
    }
}
