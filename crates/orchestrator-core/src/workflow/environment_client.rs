//! v0.7: the runtime client for `environment` plugins (prepare / exec /
//! exec_stream / teardown).
//!
//! An *environment plugin* owns the execution context a provider harness runs
//! inside: a git-worktree environment (local, the default), a container
//! environment (Docker / OCI), or a remote environment (Railway / SSH / cloud
//! sandbox). This [`EnvironmentClient`] is the host-side driver for that
//! three-call contract:
//!
//! 1. [`EnvironmentClient::prepare`] → `environment/prepare`: materialize the
//!    context and return an [`EnvironmentHandle`].
//! 2. [`EnvironmentClient::exec`] → `environment/exec`: run a
//!    [`HarnessCommand`] inside the prepared context, buffered, returning its
//!    [`ExecResponse`]. [`EnvironmentClient::exec_stream`] →
//!    `environment/exec_stream` is the streaming upgrade: it forwards
//!    incremental `environment/output` notifications to a callback and returns
//!    the same aggregated [`ExecResponse`] as the final reply.
//! 3. [`EnvironmentClient::teardown`] → `environment/teardown`: dispose of the
//!    context by handle.
//!
//! ## Resident-host model
//!
//! Draws on the process-global [`ResidentHostRegistry`] like
//! [`super::journal_client`] and
//! `orchestrator_config::workflow_config::config_source_client`, but with a
//! crucial difference: those roles are STATELESS, so they take a fresh lease per
//! call and let the host be shared / evicted between calls. Environment plugins
//! are STATEFUL across the three-call contract — `prepare` registers an in-memory
//! run (e.g. a live WS relay to a remote node) that `exec` / `exec_stream` /
//! `teardown` reuse by handle — so this client instead acquires ONE
//! [`ResidentHostLease`] on the first RPC and PINS it for its lifetime (see
//! [`EnvironmentClient::pinned_host`]). Every RPC therefore lands on the SAME warm
//! process, and the lease keeps that process safe from LRU eviction /
//! ping-reaping between `prepare` and `exec` (which would otherwise drop the
//! in-memory registration and surface as `no live relay connection for handle`).
//! A sync↔async bridge ([`run_blocking`]) drives the calls from the sync API.
//!
//! Death handling splits by call kind. `exec` / `exec_stream` are AT-MOST-ONCE: a
//! death-like failure is NOT retried — the RPC may already have run with side
//! effects, and the run's in-memory state died with the process, so the call
//! fails and any orphaned remote node is reclaimed by the plugin's own GC sweep.
//! The idempotent CONTROL ops (`prepare` / `teardown`) DO retry once: a `prepare`
//! that died before returning its handle left nothing usable, so a fresh prepare
//! on a fresh host is the correct recovery (and re-pins that host for the rest of
//! the run), and `teardown` is a dispose-by-id. Either way a dead lease is REAPED
//! — dropped from this client and its generation invalidated in the registry — so
//! the retry (or the NEXT run) spawns a live process instead of re-leasing the
//! corpse (which would otherwise keep failing with `ConnectionLost` until
//! LRU/shutdown).
//!
//! Environment plugins spawn with the same base context as `config_source` /
//! `workflow_journal` (full parent env forwarded, no working dir); the pinned
//! lease is keyed by the same binary-path + mtime + spawn-context fingerprint
//! (see [`orchestrator_plugin_host::resident_host_registry`]). Environment plugins
//! run full-env because they materialize workspaces (clone URLs, region/host
//! config) the same way config_source replaces the kernel's env-reading
//! interpolator. Unlike those non-streaming roles, environment honors the
//! manifest's `notification_buffer_size` (it is the streaming role — `exec_stream`
//! fans `environment/output` through the host broadcast channel), so a plugin that
//! declares a larger buffer gets a correctly-sized host.
//!
//! ## Discovery
//!
//! [`EnvironmentClient::resolve`] takes the environment plugin id produced by
//! [`orchestrator_config::resolve_environment`] and binds the matching installed
//! `environment` plugin: an exact match on the discovered plugin `name` wins;
//! failing that, when exactly one `environment` plugin is installed it is used
//! (a single-environment deployment needn't name it); otherwise an actionable
//! error lists the candidates.
//!
//! This client is NOT wired into the daemon / workflow runner here — the
//! workflow_runner integration is out-of-tree and a separate step. This module
//! is the reusable client + its tests.

use std::collections::BTreeMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::time::Duration;

use animus_environment_protocol::{
    EnvironmentHandle, EnvironmentSpec, ExecRequest, ExecResponse, ExecStream, GetNodeRequest, GetNodeResponse,
    HarnessCommand, ListNodesResponse, PrepareRequest, PrepareResponse, ReapRequest, TeardownNodeRequest,
    TeardownNodeResponse, TeardownRequest, TeardownResponse, METHOD_ENVIRONMENT_EXEC, METHOD_ENVIRONMENT_EXEC_STREAM,
    METHOD_ENVIRONMENT_GET, METHOD_ENVIRONMENT_LIST, METHOD_ENVIRONMENT_PREPARE, METHOD_ENVIRONMENT_REAP,
    METHOD_ENVIRONMENT_TEARDOWN, METHOD_ENVIRONMENT_TEARDOWN_NODE, NOTIFICATION_ENVIRONMENT_OUTPUT,
};
// Re-export the canonical node-management types so consumers (the `animus
// environment` CLI) get them through orchestrator-core (TASK-807).
pub use animus_environment_protocol::{EnvironmentNode, ReapResponse};
use animus_plugin_protocol::PLUGIN_KIND_ENVIRONMENT;
use anyhow::{anyhow, Context, Result};
use serde_json::Value;

use orchestrator_plugin_host::resident_host_registry::{
    binary_mtime_nanos, global_resident_host_registry, spawn_context_fingerprint, ResidentHostLease,
    ResidentHostRegistry,
};
use orchestrator_plugin_host::session::plugin_supervisor::{classify, RetryDecision};
use orchestrator_plugin_host::{discover_by_kind, DiscoveredPlugin, HostError, PluginHost, PluginSpawnOptions};

/// Default wall-clock timeout for a single `environment/*` RPC round-trip. This
/// bounds the JSON-RPC call itself, NOT the wrapped command — a long-running
/// command carries its own [`HarnessCommand`] timeout via
/// [`ExecRequest::timeout_secs`], and `exec_stream` overrides this per call from
/// that value so a legitimately slow command is not cut off by the RPC bound.
// `from_secs` (not `from_mins`, which is still unstable) keeps the RPC-timeout
// units uniform with the sibling clients (journal / config_source).
#[allow(clippy::duration_suboptimal_units)]
const ENVIRONMENT_RPC_TIMEOUT: Duration = Duration::from_secs(60);

/// Headroom added to a command's own `timeout_secs` to derive the RPC timeout
/// for `exec` / `exec_stream`, so the plugin has time to kill the command and
/// return `timed_out = true` before the host gives up on the RPC.
const EXEC_RPC_TIMEOUT_HEADROOM: Duration = Duration::from_secs(30);

/// `environment/prepare` gets its OWN (much longer) RPC timeout, separate from
/// the 60s control-op bound. Materializing a real execution context can be slow
/// and fail-prone: the Railway environment plugin, for example, creates a
/// service, waits for it to deploy, and blocks up to its dial timeout (default
/// 300s) for the container to dial home — plus workspace clone time on top. If
/// the host-side timeout fired mid-create, `control_rpc`'s retry-once would spin
/// up a SECOND node while the first was still deploying (a guaranteed leak +
/// failure on slow deploys). Six minutes comfortably covers dial + clone + margin.
#[allow(clippy::duration_suboptimal_units)]
const ENVIRONMENT_PREPARE_TIMEOUT: Duration = Duration::from_secs(360);

/// Host-side client bound to one installed `environment` plugin for one project
/// root. Cheap to construct (discovery only, no spawn); the warm plugin process
/// is spawned lazily on the first RPC and PINNED for the client's lifetime.
///
/// Environment plugins are STATEFUL across the three-call contract: `prepare`
/// registers an in-memory run (e.g. a live WS relay connection to a remote node)
/// that `exec` / `exec_stream` / `teardown` then reuse by handle. A per-call
/// resident-host lease (as the stateless `config_source` / `journal` roles use)
/// would drop the lease between calls, letting the shared registry LRU-evict or
/// ping-reap the process BETWEEN `prepare` and `exec` — losing that in-memory
/// state (surfacing as `no live relay connection for handle`). This client
/// therefore holds ONE [`ResidentHostLease`] for its own lifetime, so every RPC
/// lands on the SAME pinned process. Non-`Clone` on purpose: callers share it via
/// `Arc` so the single lease is not duplicated.
pub struct EnvironmentClient {
    plugin: DiscoveredPlugin,
    project_root: PathBuf,
    /// The resident-host lease pinned for this client's lifetime, acquired lazily
    /// on the first RPC (see [`Self::pinned_host`]) and then reused by every
    /// subsequent call so `prepare` → `exec`/`exec_stream` → `teardown` share the
    /// SAME plugin process (and thus its in-memory run state).
    pinned: tokio::sync::Mutex<Option<ResidentHostLease>>,
}

impl std::fmt::Debug for EnvironmentClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // `pinned` holds a `ResidentHostLease` (not `Debug`); omit it.
        f.debug_struct("EnvironmentClient")
            .field("plugin", &self.plugin)
            .field("project_root", &self.project_root)
            .finish_non_exhaustive()
    }
}

impl EnvironmentClient {
    /// Bind the installed `environment` plugin identified by `plugin_id` (the id
    /// produced by [`orchestrator_config::resolve_environment`]) for
    /// `project_root`.
    ///
    /// Selection: an exact match on the discovered plugin `name` wins; failing
    /// that, when exactly one `environment` plugin is installed it is used;
    /// otherwise an error lists the installed candidates so the caller can fix
    /// the `environment:` id (or install the plugin).
    pub fn resolve(project_root: &Path, plugin_id: &str) -> Result<Self> {
        let plugins = discover_by_kind(project_root.to_path_buf(), PLUGIN_KIND_ENVIRONMENT)
            .with_context(|| format!("discovering environment plugins for {}", project_root.display()))?;
        if plugins.is_empty() {
            return Err(anyhow!(
                "no environment plugin is installed, so environment id '{plugin_id}' cannot be resolved; install one with `animus plugin install <environment-plugin>` (environment plugins are optional — the workflow runner executes locally by default)"
            ));
        }
        let plugin = select_environment_plugin(plugins, plugin_id)?;
        Ok(Self { plugin, project_root: project_root.to_path_buf(), pinned: tokio::sync::Mutex::new(None) })
    }

    /// The bound plugin's discovered name.
    pub fn plugin_name(&self) -> &str {
        &self.plugin.name
    }

    /// `environment/prepare`: materialize the execution context described by
    /// `spec` and return its [`EnvironmentHandle`].
    pub fn prepare(&self, spec: EnvironmentSpec) -> Result<EnvironmentHandle> {
        let request = PrepareRequest { spec };
        let value = self.call_blocking(METHOD_ENVIRONMENT_PREPARE, request, ENVIRONMENT_PREPARE_TIMEOUT)?;
        let resp: PrepareResponse = serde_json::from_value(value)
            .with_context(|| format!("decoding PrepareResponse from environment plugin {}", self.plugin.name))?;
        Ok(resp.handle)
    }

    /// `environment/exec`: run `command` inside `handle`, buffered, returning the
    /// full [`ExecResponse`].
    ///
    /// `extra_env` is merged over (and overrides) [`HarnessCommand::env`];
    /// `stdin` is fed to the command up front; `timeout` is the command's hard
    /// wall-clock limit (the plugin kills the command and returns
    /// [`ExecResponse::timed_out`] on exceed). The RPC-level timeout is derived
    /// from `timeout` plus headroom so a legitimately slow command is bounded by
    /// its own limit; `timeout = None` leaves the RPC unbounded (the environment
    /// enforces its own policy) so an un-timed long command is not aborted.
    ///
    /// AT-MOST-ONCE: exec runs against the client's pinned host and is NOT retried
    /// on a death-like host failure — a harness command may have side effects (file
    /// mutations, git, deploys) and could already have run before the plugin died,
    /// so a blind retry risks double-execution; and the pinned process holds the
    /// run's in-memory state, which a fresh process could not recover anyway. A
    /// failure surfaces as an error for the caller to handle.
    pub fn exec(
        &self,
        handle: &EnvironmentHandle,
        command: HarnessCommand,
        extra_env: BTreeMap<String, String>,
        stdin: Option<String>,
        timeout: Option<Duration>,
    ) -> Result<ExecResponse> {
        let request = build_exec_request(handle, command, extra_env, stdin, timeout);
        let rpc_timeout = exec_rpc_timeout(request.timeout_secs);
        let params = serde_json::to_value(&request).context("serializing ExecRequest for environment/exec")?;
        let value = run_blocking(self.with_pinned_host(move |host| async move {
            environment_rpc(&host, METHOD_ENVIRONMENT_EXEC, params, rpc_timeout).await
        }))??;
        serde_json::from_value(value)
            .with_context(|| format!("decoding ExecResponse from environment plugin {}", self.plugin.name))
    }

    /// `environment/exec_stream`: like [`Self::exec`], but forwards each
    /// incremental `environment/output` notification for THIS handle to
    /// `on_output` as it arrives, then returns the aggregated [`ExecResponse`].
    ///
    /// `on_output` receives `(stream, text)` deltas; it may be invoked from the
    /// async runtime driving the call. A plugin that does not implement
    /// streaming responds with a `METHOD_NOT_SUPPORTED` RPC error (surfaced as
    /// an `Err`) — callers that want a transparent fallback should retry with
    /// [`Self::exec`].
    ///
    /// Notifications are filtered by `handle.id`, so unrelated execs sharing the
    /// same resident host are ignored. Two concurrent streamed execs against the
    /// SAME handle on one shared host cannot be told apart (the protocol keys
    /// output by handle id, not request id) — a rare case the runner avoids by
    /// not running overlapping execs in one prepared context.
    ///
    /// AT-MOST-ONCE, like [`Self::exec`]: a death-like host failure is NOT
    /// retried (the streamed command may already have run with side effects). It
    /// surfaces as an error; `timeout = None` leaves the RPC unbounded.
    pub fn exec_stream<F>(
        &self,
        handle: &EnvironmentHandle,
        command: HarnessCommand,
        extra_env: BTreeMap<String, String>,
        stdin: Option<String>,
        timeout: Option<Duration>,
        on_output: F,
    ) -> Result<ExecResponse>
    where
        F: Fn(ExecStream, &str) + Send + Sync,
    {
        let request = build_exec_request(handle, command, extra_env, stdin, timeout);
        let rpc_timeout = exec_rpc_timeout(request.timeout_secs);
        let handle_id = handle.id.clone();
        let params = serde_json::to_value(&request).context("serializing ExecRequest for environment/exec_stream")?;

        let value = run_blocking(self.with_pinned_host(move |host| async move {
            exec_stream_call(&host, params, &handle_id, rpc_timeout, &on_output).await
        }))??;

        serde_json::from_value(value)
            .with_context(|| format!("decoding ExecResponse from environment plugin {}", self.plugin.name))
    }

    /// `environment/teardown`: dispose of the context named by `handle`.
    pub fn teardown(&self, handle: &EnvironmentHandle) -> Result<()> {
        let request = TeardownRequest { handle: handle.clone() };
        let value = self.call_blocking(METHOD_ENVIRONMENT_TEARDOWN, request, ENVIRONMENT_RPC_TIMEOUT)?;
        // Decode for forward-compat / validation, even though success is empty.
        let _resp: TeardownResponse = serde_json::from_value(value)
            .with_context(|| format!("decoding TeardownResponse from environment plugin {}", self.plugin.name))?;
        Ok(())
    }

    /// `environment/list`: every managed node this environment plugin owns.
    pub fn list_nodes(&self) -> Result<Vec<EnvironmentNode>> {
        let value = self.call_blocking(METHOD_ENVIRONMENT_LIST, serde_json::json!({}), ENVIRONMENT_RPC_TIMEOUT)?;
        let resp: ListNodesResponse = serde_json::from_value(value)
            .with_context(|| format!("decoding ListNodesResponse from environment plugin {}", self.plugin.name))?;
        Ok(resp.nodes)
    }

    /// `environment/get`: describe one managed node by substrate id or name.
    pub fn get_node(&self, id: &str) -> Result<Option<EnvironmentNode>> {
        let request = GetNodeRequest { id: id.to_string() };
        let value = self.call_blocking(METHOD_ENVIRONMENT_GET, request, ENVIRONMENT_RPC_TIMEOUT)?;
        let resp: GetNodeResponse = serde_json::from_value(value)
            .with_context(|| format!("decoding GetNodeResponse from environment plugin {}", self.plugin.name))?;
        Ok(resp.node)
    }

    /// `environment/teardown_node`: destroy one managed node by id or name.
    /// Returns the substrate ids actually deleted (empty when already gone).
    pub fn teardown_node(&self, id: &str) -> Result<Vec<String>> {
        let request = TeardownNodeRequest { id: id.to_string() };
        let value = self.call_blocking(METHOD_ENVIRONMENT_TEARDOWN_NODE, request, ENVIRONMENT_RPC_TIMEOUT)?;
        let resp: TeardownNodeResponse = serde_json::from_value(value)
            .with_context(|| format!("decoding TeardownNodeResponse from environment plugin {}", self.plugin.name))?;
        Ok(resp.deleted)
    }

    /// `environment/reap`: destroy orphaned/dead managed nodes. Default (all=
    /// false) reaps only dead nodes; `all`+`force` also reaps healthy orphans;
    /// `dry_run` reports the plan without deleting.
    pub fn reap(&self, all: bool, force: bool, dry_run: bool, older_than_secs: Option<u64>) -> Result<ReapResponse> {
        let request = ReapRequest { all, force, dry_run, older_than_secs };
        let value = self.call_blocking(METHOD_ENVIRONMENT_REAP, request, ENVIRONMENT_RPC_TIMEOUT)?;
        serde_json::from_value(value)
            .with_context(|| format!("decoding ReapResponse from environment plugin {}", self.plugin.name))
    }

    /// Serialize `params` and run a CONTROL RPC (prepare / teardown) against this
    /// client's PINNED resident host, returning the raw response `Value`.
    ///
    /// Control RPCs go through the single pinned host so the whole `prepare` → …
    /// → `teardown` sequence reuses one process (and its in-memory run state).
    /// Unlike `exec`, a control RPC is RETRIED ONCE on a death-like failure (see
    /// [`Self::control_rpc`]): a control op is safe to replay — a `prepare` that
    /// died before returning its handle left no usable handle (a fresh prepare
    /// on a fresh host is the correct recovery, and it re-pins that fresh host for
    /// the subsequent exec/teardown), and `teardown` is an idempotent
    /// dispose-by-id.
    fn call_blocking<P>(&self, method: &'static str, params: P, timeout: Duration) -> Result<Value>
    where
        P: serde::Serialize + Send + 'static,
    {
        let params = serde_json::to_value(&params).with_context(|| format!("serializing params for {method}"))?;
        run_blocking(self.control_rpc(method, params, timeout))?
    }

    /// A control RPC (prepare / teardown) against the pinned host, retried ONCE on
    /// a death-like failure.
    ///
    /// The first attempt's [`Self::run_once`] already reaped the dead lease, so the
    /// retry's [`Self::pinned_host`] leases a freshly-spawned host — and stores it
    /// in `self.pinned`, so a retried `prepare` re-pins that live host for the rest
    /// of the run. A structured (non-death) error is NOT retried; it would only
    /// fail the same way. Retry is safe ONLY for these idempotent control ops —
    /// `exec` / `exec_stream` deliberately go through [`Self::with_pinned_host`]
    /// (at-most-once) instead.
    async fn control_rpc(&self, method: &'static str, params: Value, timeout: Duration) -> Result<Value> {
        let params_retry = params.clone();
        match self
            .run_once(move |host| async move { environment_rpc(&host, method, params, Some(timeout)).await })
            .await
        {
            Ok(value) => Ok(value),
            Err(ResidentCallError::Other(err)) => Err(err),
            Err(ResidentCallError::Death(_)) => match self
                .run_once(move |host| async move { environment_rpc(&host, method, params_retry, Some(timeout)).await })
                .await
            {
                Ok(value) => Ok(value),
                Err(ResidentCallError::Death(err)) | Err(ResidentCallError::Other(err)) => Err(err),
            },
        }
    }

    /// The client's pinned resident host plus its lease GENERATION, acquiring the
    /// lease on first use.
    ///
    /// The lease is stored in `self.pinned` and held for the client's whole
    /// lifetime, so it never becomes LRU-evictable or ping-reapable between RPCs —
    /// every call returns a clone of the SAME [`PluginHost`], preserving the
    /// plugin's in-memory run registration across `prepare` → exec → `teardown`.
    /// The returned generation identifies exactly this host so a death-like failure
    /// reaps only this generation (see [`Self::reap_pinned`]).
    async fn pinned_host(&self) -> Result<(PluginHost, u64)> {
        let mut guard = self.pinned.lock().await;
        if guard.is_none() {
            let lease = acquire_resident_lease(
                &global_resident_host_registry(),
                &self.plugin,
                binary_mtime_nanos(&self.plugin.path),
                &environment_spawn_context(self.plugin.manifest.notification_buffer_size),
            )
            .await?;
            *guard = Some(lease);
        }
        let lease = guard.as_ref().expect("lease populated above");
        Ok((lease.host().clone(), lease.generation()))
    }

    /// Run `call` against this client's pinned host EXACTLY once (no retry),
    /// mapping a resident-call error back to `anyhow`.
    ///
    /// Used by `exec` / `exec_stream`: the pinned host holds the run's in-memory
    /// state (the WS relay registration), so a death-like failure means that state
    /// is already gone and a fresh process could not recover it, and the RPC may
    /// already have run with side effects — so the call fails (at-most-once) and
    /// any orphaned remote node is reclaimed by the plugin's own GC sweep. The dead
    /// lease is still reaped (inside [`Self::run_once`]) so the NEXT run spawns a
    /// live process; reaping is not a replay.
    async fn with_pinned_host<T, F, Fut>(&self, call: F) -> Result<T>
    where
        F: FnOnce(PluginHost) -> Fut,
        Fut: Future<Output = std::result::Result<T, ResidentCallError>>,
    {
        match self.run_once(call).await {
            Ok(value) => Ok(value),
            Err(ResidentCallError::Death(err)) | Err(ResidentCallError::Other(err)) => Err(err),
        }
    }

    /// Acquire the pinned host and run `call` once, REAPING the pinned lease on a
    /// death-like failure (so the next acquire spawns a fresh host) and returning
    /// the classified error so the caller can decide whether to retry. Never
    /// replays `call`.
    async fn run_once<T, F, Fut>(&self, call: F) -> std::result::Result<T, ResidentCallError>
    where
        F: FnOnce(PluginHost) -> Fut,
        Fut: Future<Output = std::result::Result<T, ResidentCallError>>,
    {
        let (host, generation) = self.pinned_host().await.map_err(ResidentCallError::Other)?;
        match call(host).await {
            Ok(value) => Ok(value),
            Err(ResidentCallError::Death(err)) => {
                self.reap_pinned(generation).await;
                Err(ResidentCallError::Death(err))
            }
            Err(other) => Err(other),
        }
    }

    /// Reap the host generation `generation` that just failed: drop this client's
    /// pinned lease iff it is still that exact generation, and invalidate that
    /// generation in the shared registry, so a dead host is not re-leased by the
    /// next run.
    ///
    /// Strictly generation-scoped, which makes it safe under concurrent RPCs on one
    /// `Arc<EnvironmentClient>`: if another call has meanwhile re-acquired a FRESH
    /// lease (a different generation), this leaves BOTH `self.pinned` and the
    /// registry entry for that new host untouched — a late failure of the OLD
    /// generation can never evict the healthy replacement. Idempotent: reaping the
    /// same generation twice is a no-op the second time.
    async fn reap_pinned(&self, generation: u64) {
        {
            let mut guard = self.pinned.lock().await;
            if guard.as_ref().is_some_and(|lease| lease.generation() == generation) {
                // Drops the dead lease, releasing its `active` ref so the registry
                // can evict the entry below.
                *guard = None;
            }
        }
        global_resident_host_registry()
            .invalidate_generation(
                &self.plugin.path,
                binary_mtime_nanos(&self.plugin.path),
                &environment_spawn_context(self.plugin.manifest.notification_buffer_size),
                generation,
            )
            .await;
    }
}

/// Select the environment plugin matching `plugin_id`: exact `name` match first,
/// else the sole installed environment plugin, else an error listing candidates.
fn select_environment_plugin(plugins: Vec<DiscoveredPlugin>, plugin_id: &str) -> Result<DiscoveredPlugin> {
    if let Some(exact) = plugins.iter().find(|p| p.name == plugin_id) {
        return Ok(exact.clone());
    }
    if plugins.len() == 1 {
        return Ok(plugins.into_iter().next().expect("len checked"));
    }
    let candidates = plugins.iter().map(|p| p.name.as_str()).collect::<Vec<_>>().join(", ");
    Err(anyhow!(
        "no installed environment plugin matches environment id '{plugin_id}'; installed environment plugins: [{candidates}]. Set the workflow/phase `environment:` (or a routing rule) to one of these ids, or install the plugin that provides '{plugin_id}'."
    ))
}

/// Build an [`ExecRequest`] from a handle + command, merging `extra_env` over
/// the command's own env and folding in `stdin` / `timeout`.
fn build_exec_request(
    handle: &EnvironmentHandle,
    mut command: HarnessCommand,
    extra_env: BTreeMap<String, String>,
    stdin: Option<String>,
    timeout: Option<Duration>,
) -> ExecRequest {
    // extra_env overrides the command's own env on key collision.
    command.env.extend(extra_env);
    ExecRequest { handle: handle.clone(), command, stdin, timeout_secs: timeout.map(|d| d.as_secs()) }
}

/// Derive the RPC-level timeout for an exec from the command's own
/// `timeout_secs`: the command's limit plus headroom, so the plugin can kill an
/// over-limit command and reply `timed_out` before the host abandons the RPC.
///
/// Returns `None` when the command carries NO explicit timeout — the protocol
/// treats that as unbounded (the environment may still impose its own policy),
/// so the host must NOT clamp such a command to a fixed RPC deadline that would
/// abort a legitimately long-running (but un-timed) command.
fn exec_rpc_timeout(command_timeout_secs: Option<u64>) -> Option<Duration> {
    command_timeout_secs.map(|secs| Duration::from_secs(secs).saturating_add(EXEC_RPC_TIMEOUT_HEADROOM))
}

// ---------------------------------------------------------------------------
// Resident-host machinery (cross-role shared registry — 0.7 Layer B).
//
// Warm `environment` hosts live in the process-global `ResidentHostRegistry`
// shared with `config_source` / `workflow_journal` / `subject_backend`, keyed by
// the plugin's binary path + mtime + spawn-context fingerprint. A plugin binary
// that also serves those full-env roles is therefore ONE shared process,
// spawned + handshaked once.
// ---------------------------------------------------------------------------

/// Reap every resident host in the shared registry. Wired into daemon graceful
/// shutdown so warm plugin processes terminate cleanly. Idempotent — a prior
/// config_source / journal teardown may already have drained the shared
/// registry.
pub async fn shutdown_resident_hosts() {
    global_resident_host_registry().shutdown_all().await;
}

/// A failure running an RPC against a resident host. Distinguishes a death-like
/// host failure (presume the process is dead → reap + re-spawn + retry once)
/// from any other error (a structured plugin RPC error, or a decode failure on a
/// live host) which propagates without burning a re-spawn.
enum ResidentCallError {
    Death(anyhow::Error),
    Other(anyhow::Error),
}

impl ResidentCallError {
    fn from_host_error(err: HostError) -> Self {
        match classify(&err) {
            RetryDecision::DeathLike => ResidentCallError::Death(anyhow!(err)),
            RetryDecision::StructuredError => ResidentCallError::Other(anyhow!(err)),
        }
    }
}

/// One buffered `environment/*` RPC against a resident host clone. `timeout`
/// bounds the RPC; `None` issues an unbounded request (used by exec for a
/// command that declared no timeout of its own — the environment enforces its
/// own policy) so a legitimately long, un-timed command is not aborted by a
/// fixed host deadline.
async fn environment_rpc(
    host: &PluginHost,
    method: &'static str,
    params: Value,
    timeout: Option<Duration>,
) -> std::result::Result<Value, ResidentCallError> {
    let result = match timeout {
        Some(timeout) => host.request_typed_with_timeout(method, Some(params), timeout).await,
        None => host.request_typed(method, Some(params)).await,
    };
    result.map_err(ResidentCallError::from_host_error)
}

/// `environment/exec_stream` against a resident host clone: subscribe to the
/// host's notifications BEFORE issuing the request (so no early
/// `environment/output` frame is lost), then drive the request while forwarding
/// each matching output delta to `on_output`.
async fn exec_stream_call<F>(
    host: &PluginHost,
    params: Value,
    handle_id: &str,
    timeout: Option<Duration>,
    on_output: &F,
) -> std::result::Result<Value, ResidentCallError>
where
    F: Fn(ExecStream, &str) + Send + Sync,
{
    use tokio::sync::broadcast::error::RecvError;

    let mut notifications = host.subscribe_notifications();
    // `None` timeout => an unbounded streamed exec (an un-timed command); the
    // environment enforces its own policy rather than the host clamping it.
    let request = async {
        match timeout {
            Some(timeout) => {
                host.request_typed_with_timeout(METHOD_ENVIRONMENT_EXEC_STREAM, Some(params), timeout).await
            }
            None => host.request_typed(METHOD_ENVIRONMENT_EXEC_STREAM, Some(params)).await,
        }
    };
    tokio::pin!(request);

    // Drive the request while draining output notifications. Polling is FAIR
    // (no `biased`): the resident host is shared, so a continuous notification
    // flood from another handle's concurrent stream must never starve THIS
    // request's future (which would also stall its internal timeout). Output
    // completeness does not depend on winning this race — the reply is the
    // plugin's LAST frame, so by the time the request resolves the reader has
    // already broadcast every earlier `environment/output` frame; the post-loop
    // `try_recv` drain below forwards any that were buffered but not yet seen.
    let response = loop {
        tokio::select! {
            note = notifications.recv() => {
                match note {
                    Ok(note) => forward_output(&note, handle_id, on_output),
                    // A lagged subscriber lost deltas; the buffered ExecResponse
                    // still carries the full aggregated output, so keep going.
                    Err(RecvError::Lagged(_)) => {}
                    // The host's reader closed (process died); stop draining and
                    // let the request resolve into the death-like path.
                    Err(RecvError::Closed) => break request.await,
                }
            }
            result = &mut request => break result,
        }
    };

    // The reply frame is the last frame the plugin sends, so any remaining
    // output frames for THIS handle were broadcast before it — drain the ones
    // already buffered (non-blocking) so a trailing delta emitted just before the
    // reply is not lost to the callback. Bound the pass to the message count
    // buffered AT RESPONSE TIME: on a shared resident host another handle's
    // concurrent stream could keep feeding new frames, so an unbounded
    // `while try_recv().is_ok()` could spin indefinitely and delay return even
    // though this RPC already completed.
    let buffered = notifications.len();
    for _ in 0..buffered {
        match notifications.try_recv() {
            Ok(note) => forward_output(&note, handle_id, on_output),
            Err(_) => break,
        }
    }

    response.map_err(ResidentCallError::from_host_error)
}

/// Wire payload of an `environment/output` notification.
///
/// The protocol carries the [`ExecNotification::Output`] fields FLAT (see
/// `ExecNotification::payload`, which emits `{handle_id, stream, text}` with no
/// `kind` tag), so we decode the flat shape directly rather than the tagged
/// [`ExecNotification`] enum.
#[derive(serde::Deserialize)]
struct OutputPayload {
    handle_id: String,
    stream: ExecStream,
    text: String,
}

/// Decode an `environment/output` notification and, when it targets `handle_id`,
/// forward its delta to `on_output`. Non-output methods and other handles are
/// ignored; a malformed payload is dropped (the aggregated ExecResponse remains
/// the source of truth).
fn forward_output<F>(note: &animus_plugin_protocol::RpcNotification, handle_id: &str, on_output: &F)
where
    F: Fn(ExecStream, &str),
{
    if note.method != NOTIFICATION_ENVIRONMENT_OUTPUT {
        return;
    }
    let Some(params) = note.params.clone() else {
        return;
    };
    if let Ok(OutputPayload { handle_id: note_handle, stream, text }) = serde_json::from_value::<OutputPayload>(params)
    {
        if note_handle == handle_id {
            on_output(stream, &text);
        }
    }
}

/// Lease the shared resident host for `plugin`, spawning + handshaking it once
/// via the [`ResidentHostRegistry`] if it is not already live. A handshake
/// failure inside the spawn closure tears the half-started child down (it is
/// never inserted) so a retry re-spawns cleanly.
async fn acquire_resident_lease(
    registry: &ResidentHostRegistry,
    plugin: &DiscoveredPlugin,
    mtime: u128,
    context: &str,
) -> Result<ResidentHostLease> {
    registry
        .get_or_spawn(&plugin.path, mtime, context, || async {
            let host = spawn_environment_host(plugin).await?;
            if let Err(err) = host.handshake().await {
                let _ = host.clone().shutdown().await;
                return Err(err).with_context(|| format!("handshake with environment plugin {}", plugin.name));
            }
            Ok(host)
        })
        .await
}

/// Spawn-context fingerprint for an `environment` host: full parent env
/// forwarded, no working dir. `notification_buffer` is the plugin manifest's
/// `notification_buffer_size` hint — environment is the streaming role
/// (`exec_stream` fans `environment/output` notifications through the host's
/// broadcast channel), so a plugin that declares a larger buffer must get a host
/// sized for it (and thus its OWN fingerprint). A plugin that declares no hint
/// (`None`) keeps the IDENTICAL context as the `config_source` / `workflow_journal`
/// roles (which never stream and always pass `None`), so a multi-role binary that
/// does not tune its buffer still collapses to one shared process.
fn environment_spawn_context(notification_buffer: Option<usize>) -> String {
    let forwarded_env: Vec<String> = std::env::vars().map(|(name, _)| name).collect();
    spawn_context_fingerprint(&forwarded_env, None, notification_buffer)
}

async fn spawn_environment_host(plugin: &DiscoveredPlugin) -> Result<PluginHost> {
    let forwarded_env: Vec<String> = std::env::vars().map(|(name, _)| name).collect();
    // Honor the manifest's `notification_buffer_size` so bursty `exec_stream`
    // output is not dropped by an under-sized broadcast channel. This MUST match
    // the hint folded into `environment_spawn_context` above or the fingerprint
    // would key a host spawned with a different capacity.
    let options =
        PluginSpawnOptions::for_manifest(plugin.name.clone(), &plugin.manifest.env_required, forwarded_env, None)
            .with_notification_buffer_hint(plugin.manifest.notification_buffer_size);
    PluginHost::spawn_with_options(&plugin.path, &[], options)
        .await
        .with_context(|| format!("spawning environment plugin {}", plugin.name))
}

/// Bridge an async future into a sync call. Works whether or not a tokio runtime
/// is already running (daemon = inside a runtime; CLI = none). Mirrors
/// `journal_client::run_blocking`.
fn run_blocking<F: Future>(fut: F) -> Result<F::Output> {
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => Ok(tokio::task::block_in_place(|| handle.block_on(fut))),
        Err(_) => {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .context("building tokio runtime for environment plugin call")?;
            Ok(rt.block_on(fut))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use animus_environment_protocol::ExecNotification;

    use animus_plugin_protocol::{InitializeResult, PluginCapabilities, PluginInfo, RpcRequest, RpcResponse};
    use tokio::io::{duplex, AsyncBufReadExt, AsyncWriteExt, BufReader};

    /// Serialize tests that drive the process-global resident-host registry: they
    /// reset it with `shutdown_all()` and count spawns, so they must not run
    /// concurrently against the shared slot. Async-aware so the guard can be held
    /// across the tests' `.await`s.
    fn registry_lock() -> &'static tokio::sync::Mutex<()> {
        use std::sync::OnceLock;
        static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
    }

    /// The process-global registry, drained to empty so a test starts from a
    /// known-cold state (spawn counts are then unambiguous).
    async fn fresh_registry() -> Arc<ResidentHostRegistry> {
        let registry = global_resident_host_registry();
        registry.shutdown_all().await;
        registry
    }

    /// A fake `environment` plugin over an in-memory duplex: handshakes
    /// (advertising plugin_kind = environment), then answers prepare / exec /
    /// exec_stream / teardown. For `exec_stream` it emits two
    /// `environment/output` notifications (stdout + stderr) before the reply.
    /// `spawn_count` increments once per construction so a test can assert
    /// host-sharing.
    async fn fake_environment_host(spawn_count: Arc<AtomicUsize>) -> PluginHost {
        spawn_count.fetch_add(1, Ordering::SeqCst);
        let (host_reader, mut plugin_writer) = duplex(64 * 1024);
        let (plugin_reader, host_writer) = duplex(64 * 1024);
        tokio::spawn(async move {
            let mut reader = BufReader::new(plugin_reader);
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).await.expect("read line") == 0 {
                    break;
                }
                if line.trim().is_empty() {
                    continue;
                }
                let request: RpcRequest = serde_json::from_str(line.trim()).expect("parse request");
                match request.method.as_str() {
                    "initialize" => {
                        let result = InitializeResult {
                            protocol_version: "1.0.0".to_string(),
                            plugin_info: PluginInfo {
                                name: "fake-env".to_string(),
                                version: "0.1.0".to_string(),
                                plugin_kind: PLUGIN_KIND_ENVIRONMENT.to_string(),
                                plugin_kinds: vec![PLUGIN_KIND_ENVIRONMENT.to_string()],
                                description: None,
                            },
                            capabilities: PluginCapabilities::default(),
                            kind_capabilities: std::collections::HashMap::new(),
                        };
                        write_response(&mut plugin_writer, RpcResponse::ok(request.id, serde_json::json!(result)))
                            .await;
                    }
                    "initialized" => continue,
                    METHOD_ENVIRONMENT_PREPARE => {
                        let resp = PrepareResponse {
                            handle: EnvironmentHandle {
                                id: "env-1".to_string(),
                                workspace_root: "/work".to_string(),
                                metadata: Value::Null,
                            },
                        };
                        write_response(
                            &mut plugin_writer,
                            RpcResponse::ok(request.id, serde_json::to_value(resp).unwrap()),
                        )
                        .await;
                    }
                    METHOD_ENVIRONMENT_EXEC => {
                        let req: ExecRequest =
                            serde_json::from_value(request.params.clone().unwrap_or(Value::Null)).expect("exec params");
                        // Echo the program name in stdout so the test can assert
                        // the command round-tripped.
                        let resp = ExecResponse {
                            exit_code: Some(0),
                            stdout: format!("ran {}", req.command.program),
                            stderr: String::new(),
                            timed_out: false,
                        };
                        write_response(
                            &mut plugin_writer,
                            RpcResponse::ok(request.id, serde_json::to_value(resp).unwrap()),
                        )
                        .await;
                    }
                    METHOD_ENVIRONMENT_EXEC_STREAM => {
                        let req: ExecRequest =
                            serde_json::from_value(request.params.clone().unwrap_or(Value::Null)).expect("exec params");
                        let handle_id = req.handle.id.clone();
                        // Emit two output notifications, then the aggregated reply.
                        for (stream, text) in [(ExecStream::Stdout, "chunk-out"), (ExecStream::Stderr, "chunk-err")] {
                            let note = ExecNotification::Output {
                                handle_id: handle_id.clone(),
                                stream,
                                text: text.to_string(),
                            };
                            let frame = serde_json::json!({
                                "jsonrpc": "2.0",
                                "method": note.method(),
                                "params": note.payload(),
                            });
                            write_line(&mut plugin_writer, &frame).await;
                        }
                        let resp = ExecResponse {
                            exit_code: Some(7),
                            stdout: "chunk-out".to_string(),
                            stderr: "chunk-err".to_string(),
                            timed_out: false,
                        };
                        write_response(
                            &mut plugin_writer,
                            RpcResponse::ok(request.id, serde_json::to_value(resp).unwrap()),
                        )
                        .await;
                    }
                    METHOD_ENVIRONMENT_TEARDOWN => {
                        write_response(
                            &mut plugin_writer,
                            RpcResponse::ok(request.id, serde_json::to_value(TeardownResponse::default()).unwrap()),
                        )
                        .await;
                    }
                    other => {
                        let err = animus_plugin_protocol::RpcError {
                            code: animus_plugin_protocol::error_codes::METHOD_NOT_SUPPORTED,
                            message: format!("unsupported method {other}"),
                            data: None,
                        };
                        write_response(&mut plugin_writer, RpcResponse::err(request.id, err)).await;
                    }
                }
            }
        });
        PluginHost::from_streams("fake-env", host_reader, host_writer)
    }

    async fn write_response(writer: &mut tokio::io::DuplexStream, response: RpcResponse) {
        write_line(writer, &serde_json::to_value(&response).expect("encode response")).await;
    }

    async fn write_line(writer: &mut tokio::io::DuplexStream, value: &Value) {
        let mut encoded = serde_json::to_string(value).expect("encode");
        encoded.push('\n');
        writer.write_all(encoded.as_bytes()).await.expect("write");
    }

    /// Drive a fake host through the resident registry the same way the client
    /// does, so a test exercises `get_or_spawn` + handshake without a real
    /// binary on disk.
    async fn lease_fake(
        registry: &ResidentHostRegistry,
        path: &Path,
        spawn_count: Arc<AtomicUsize>,
    ) -> ResidentHostLease {
        let mtime = binary_mtime_nanos(path);
        let ctx = environment_spawn_context(None);
        registry
            .get_or_spawn(path, mtime, &ctx, || async {
                let host = fake_environment_host(spawn_count).await;
                host.handshake().await?;
                Ok(host)
            })
            .await
            .expect("lease fake host")
    }

    fn sample_handle() -> EnvironmentHandle {
        EnvironmentHandle { id: "env-1".to_string(), workspace_root: "/work".to_string(), metadata: Value::Null }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn prepare_exec_teardown_round_trip() {
        let _guard = registry_lock().lock().await;
        let registry = fresh_registry().await;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fake-env");
        std::fs::write(&path, b"binary").unwrap();
        let spawns = Arc::new(AtomicUsize::new(0));

        // prepare
        let handle = {
            let lease = lease_fake(&registry, &path, spawns.clone()).await;
            let value = environment_rpc(
                lease.host(),
                METHOD_ENVIRONMENT_PREPARE,
                serde_json::to_value(PrepareRequest {
                    spec: EnvironmentSpec {
                        kind: "worktree".to_string(),
                        repos: Vec::new(),
                        image: None,
                        resources: None,
                        env: BTreeMap::new(),
                        metadata: Value::Null,
                    },
                })
                .unwrap(),
                Some(ENVIRONMENT_RPC_TIMEOUT),
            )
            .await
            .map_err(|e| match e {
                ResidentCallError::Death(e) | ResidentCallError::Other(e) => e,
            })
            .expect("prepare ok");
            let resp: PrepareResponse = serde_json::from_value(value).unwrap();
            resp.handle
        };
        assert_eq!(handle.id, "env-1");
        assert_eq!(handle.workspace_root, "/work");

        // exec
        {
            let lease = lease_fake(&registry, &path, spawns.clone()).await;
            let req = build_exec_request(
                &handle,
                HarnessCommand {
                    program: "echo".to_string(),
                    args: vec!["hi".to_string()],
                    env: BTreeMap::new(),
                    cwd: None,
                },
                BTreeMap::new(),
                None,
                None,
            );
            let value = environment_rpc(
                lease.host(),
                METHOD_ENVIRONMENT_EXEC,
                serde_json::to_value(&req).unwrap(),
                Some(ENVIRONMENT_RPC_TIMEOUT),
            )
            .await
            .map_err(|e| match e {
                ResidentCallError::Death(e) | ResidentCallError::Other(e) => e,
            })
            .expect("exec ok");
            let resp: ExecResponse = serde_json::from_value(value).unwrap();
            assert_eq!(resp.exit_code, Some(0));
            assert_eq!(resp.stdout, "ran echo");
        }

        // teardown
        {
            let lease = lease_fake(&registry, &path, spawns.clone()).await;
            let value = environment_rpc(
                lease.host(),
                METHOD_ENVIRONMENT_TEARDOWN,
                serde_json::to_value(TeardownRequest { handle: handle.clone() }).unwrap(),
                Some(ENVIRONMENT_RPC_TIMEOUT),
            )
            .await
            .map_err(|e| match e {
                ResidentCallError::Death(e) | ResidentCallError::Other(e) => e,
            })
            .expect("teardown ok");
            let _resp: TeardownResponse = serde_json::from_value(value).unwrap();
        }

        // All three RPCs resolved the SAME binary through the shared registry →
        // exactly one spawned process.
        assert_eq!(spawns.load(Ordering::SeqCst), 1, "one shared environment process across prepare/exec/teardown");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn exec_stream_forwards_output_and_returns_response() {
        let _guard = registry_lock().lock().await;
        let registry = fresh_registry().await;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fake-env");
        std::fs::write(&path, b"binary").unwrap();
        let spawns = Arc::new(AtomicUsize::new(0));

        let lease = lease_fake(&registry, &path, spawns.clone()).await;
        let collected: Arc<Mutex<Vec<(ExecStream, String)>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = collected.clone();
        let params = serde_json::to_value(build_exec_request(
            &sample_handle(),
            HarnessCommand { program: "run".to_string(), args: Vec::new(), env: BTreeMap::new(), cwd: None },
            BTreeMap::new(),
            None,
            None,
        ))
        .unwrap();

        let value =
            exec_stream_call(lease.host(), params, "env-1", Some(ENVIRONMENT_RPC_TIMEOUT), &move |stream, text| {
                sink.lock().unwrap_or_else(|p| p.into_inner()).push((stream, text.to_string()));
            })
            .await
            .map_err(|e| match e {
                ResidentCallError::Death(e) | ResidentCallError::Other(e) => e,
            })
            .expect("exec_stream ok");

        let resp: ExecResponse = serde_json::from_value(value).unwrap();
        assert_eq!(resp.exit_code, Some(7));

        let got = collected.lock().unwrap_or_else(|p| p.into_inner()).clone();
        assert_eq!(
            got,
            vec![(ExecStream::Stdout, "chunk-out".to_string()), (ExecStream::Stderr, "chunk-err".to_string())],
            "both output deltas forwarded to the callback in order"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn exec_stream_ignores_other_handles() {
        let _guard = registry_lock().lock().await;
        let registry = fresh_registry().await;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fake-env");
        std::fs::write(&path, b"binary").unwrap();
        let spawns = Arc::new(AtomicUsize::new(0));

        let lease = lease_fake(&registry, &path, spawns.clone()).await;
        let collected: Arc<Mutex<Vec<(ExecStream, String)>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = collected.clone();
        // The fake emits notifications for handle "env-1"; filter on a different
        // handle id so none should reach the callback.
        let params = serde_json::to_value(build_exec_request(
            &sample_handle(),
            HarnessCommand { program: "run".to_string(), args: Vec::new(), env: BTreeMap::new(), cwd: None },
            BTreeMap::new(),
            None,
            None,
        ))
        .unwrap();

        let value = exec_stream_call(
            lease.host(),
            params,
            "different-handle",
            Some(ENVIRONMENT_RPC_TIMEOUT),
            &move |stream, text| {
                sink.lock().unwrap_or_else(|p| p.into_inner()).push((stream, text.to_string()));
            },
        )
        .await
        .map_err(|e| match e {
            ResidentCallError::Death(e) | ResidentCallError::Other(e) => e,
        })
        .expect("exec_stream ok");

        let resp: ExecResponse = serde_json::from_value(value).unwrap();
        assert_eq!(resp.exit_code, Some(7), "response still returned even when no deltas match");
        assert!(
            collected.lock().unwrap_or_else(|p| p.into_inner()).is_empty(),
            "no deltas forwarded for a non-matching handle id"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn two_clients_share_one_host_process() {
        // Host-sharing: two independent resolves of the SAME env binary (as two
        // EnvironmentClients would) collapse to one spawned + handshaked process.
        let _guard = registry_lock().lock().await;
        let registry = fresh_registry().await;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fake-env");
        std::fs::write(&path, b"binary").unwrap();
        let spawns = Arc::new(AtomicUsize::new(0));

        let lease_a = lease_fake(&registry, &path, spawns.clone()).await;
        let lease_b = lease_fake(&registry, &path, spawns.clone()).await;

        // Both resolves collapsed to one spawned + handshaked process: a single
        // spawn is the observable proof of host-sharing (the live-host count and
        // raw host pointer are crate-private to orchestrator-plugin-host).
        assert_eq!(
            spawns.load(Ordering::SeqCst),
            1,
            "the environment binary spawned exactly once, shared by both clients"
        );
        drop((lease_a, lease_b));
    }

    fn discovered(name: &str) -> DiscoveredPlugin {
        DiscoveredPlugin {
            name: name.to_string(),
            path: PathBuf::from(format!("/plugins/{name}")),
            manifest: animus_plugin_protocol::PluginManifest {
                name: name.to_string(),
                version: "0.1.0".to_string(),
                plugin_kind: PLUGIN_KIND_ENVIRONMENT.to_string(),
                plugin_kinds: Vec::new(),
                description: String::new(),
                protocol_version: "1.0.0".to_string(),
                capabilities: Vec::new(),
                env_required: Vec::new(),
                notification_buffer_size: None,
                supports_mcp: None,
            },
            source: orchestrator_plugin_host::DiscoverySource::PluginPath,
        }
    }

    #[test]
    fn select_prefers_exact_name_match() {
        let plugins = vec![discovered("worktree"), discovered("container")];
        let got = select_environment_plugin(plugins, "container").expect("exact match");
        assert_eq!(got.name, "container");
    }

    #[test]
    fn select_falls_back_to_sole_environment_plugin() {
        let plugins = vec![discovered("animus-environment-worktree")];
        // Requested id differs from the installed name, but there is only one.
        let got = select_environment_plugin(plugins, "worktree").expect("sole fallback");
        assert_eq!(got.name, "animus-environment-worktree");
    }

    #[test]
    fn select_errors_when_ambiguous_and_no_match() {
        let plugins = vec![discovered("worktree"), discovered("container")];
        let err = select_environment_plugin(plugins, "railway").expect_err("ambiguous, no match");
        let msg = format!("{err}");
        assert!(msg.contains("railway"), "error names the requested id: {msg}");
        assert!(msg.contains("worktree") && msg.contains("container"), "error lists candidates: {msg}");
    }

    #[test]
    fn build_exec_request_merges_extra_env_over_command_env() {
        let mut cmd_env = BTreeMap::new();
        cmd_env.insert("A".to_string(), "cmd".to_string());
        cmd_env.insert("B".to_string(), "cmd".to_string());
        let command = HarnessCommand { program: "p".to_string(), args: Vec::new(), env: cmd_env, cwd: None };
        let mut extra = BTreeMap::new();
        extra.insert("B".to_string(), "extra".to_string()); // overrides
        extra.insert("C".to_string(), "extra".to_string()); // adds

        let req =
            build_exec_request(&sample_handle(), command, extra, Some("in".to_string()), Some(Duration::from_secs(5)));
        assert_eq!(req.command.env.get("A").map(String::as_str), Some("cmd"));
        assert_eq!(req.command.env.get("B").map(String::as_str), Some("extra"), "extra_env overrides on collision");
        assert_eq!(req.command.env.get("C").map(String::as_str), Some("extra"));
        assert_eq!(req.stdin.as_deref(), Some("in"));
        assert_eq!(req.timeout_secs, Some(5));
    }

    #[test]
    #[allow(clippy::duration_suboptimal_units)]
    fn exec_rpc_timeout_adds_headroom_over_command_limit() {
        assert_eq!(exec_rpc_timeout(Some(120)), Some(Duration::from_secs(120) + EXEC_RPC_TIMEOUT_HEADROOM));
        // No command timeout => unbounded RPC (the environment enforces its own
        // policy); the host must not clamp it to a fixed deadline.
        assert_eq!(exec_rpc_timeout(None), None);
    }
}
