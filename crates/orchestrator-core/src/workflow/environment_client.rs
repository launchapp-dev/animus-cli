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
//! Mirrors [`super::journal_client`] and
//! `orchestrator_config::workflow_config::config_source_client`: the client
//! routes every RPC through the process-global [`ResidentHostRegistry`] — ONE
//! warm plugin host per binary, kept across calls, with a death-aware
//! reap+respawn+retry-once wrapper ([`with_resident_host`]) and a sync↔async
//! bridge ([`run_blocking`]).
//!
//! Environment plugins spawn with the same base context as `config_source` /
//! `workflow_journal` (full parent env forwarded, no working dir), so a single
//! plugin binary that ALSO serves one of those roles collapses to one shared
//! process (see [`orchestrator_plugin_host::resident_host_registry`] for the
//! spawn-context fingerprint semantics). Environment plugins run full-env because
//! they materialize workspaces (clone URLs, region/host config) the same way
//! config_source replaces the kernel's env-reading interpolator. Unlike those
//! non-streaming roles, environment honors the manifest's
//! `notification_buffer_size` (it is the streaming role — `exec_stream` fans
//! `environment/output` through the host broadcast channel), so a plugin that
//! declares a larger buffer gets its own correctly-sized host; a plugin that
//! declares none still shares the collapsed process.
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
    EnvironmentHandle, EnvironmentSpec, ExecRequest, ExecResponse, ExecStream, HarnessCommand, PrepareRequest,
    PrepareResponse, TeardownRequest, TeardownResponse, METHOD_ENVIRONMENT_EXEC, METHOD_ENVIRONMENT_EXEC_STREAM,
    METHOD_ENVIRONMENT_PREPARE, METHOD_ENVIRONMENT_TEARDOWN, NOTIFICATION_ENVIRONMENT_OUTPUT,
};
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

/// Host-side client bound to one installed `environment` plugin for one project
/// root. Cheap to construct (discovery only, no spawn); the warm plugin process
/// is spawned lazily on the first RPC and shared via the resident-host registry.
#[derive(Debug, Clone)]
pub struct EnvironmentClient {
    plugin: DiscoveredPlugin,
    project_root: PathBuf,
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
        Ok(Self { plugin, project_root: project_root.to_path_buf() })
    }

    /// The bound plugin's discovered name.
    pub fn plugin_name(&self) -> &str {
        &self.plugin.name
    }

    /// `environment/prepare`: materialize the execution context described by
    /// `spec` and return its [`EnvironmentHandle`].
    pub fn prepare(&self, spec: EnvironmentSpec) -> Result<EnvironmentHandle> {
        let request = PrepareRequest { spec };
        let value = self.call_blocking(METHOD_ENVIRONMENT_PREPARE, request, ENVIRONMENT_RPC_TIMEOUT)?;
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
    /// AT-MOST-ONCE: unlike the control RPCs, exec is NOT retried on a death-like
    /// host failure — a harness command may have side effects (file mutations,
    /// git, deploys) and could already have run before the plugin died, so a
    /// blind retry risks double-execution. A spawn happens once (the command has
    /// not run yet at spawn time); a failure after the request is sent surfaces
    /// as an error for the caller to handle.
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
        let value = run_blocking(with_resident_host_once(&self.plugin, &self.project_root, move |host| async move {
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

        let value = run_blocking(with_resident_host_once(&self.plugin, &self.project_root, move |host| async move {
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

    /// Serialize `params`, run a CONTROL RPC (prepare / teardown) against the
    /// resident host (spawning it once if absent), reap+respawn+retry-once on a
    /// death-like failure, and return the raw response `Value`.
    ///
    /// Retry is safe here because the control ops are effectively idempotent from
    /// the caller's view: a teardown re-issues a dispose-by-id (a no-op if
    /// already gone), and a prepare that died before returning its handle left no
    /// handle the caller can use, so a fresh prepare is the correct recovery.
    /// Exec is deliberately NOT routed through this path (see [`Self::exec`]).
    fn call_blocking<P>(&self, method: &'static str, params: P, timeout: Duration) -> Result<Value>
    where
        P: serde::Serialize + Send + 'static,
    {
        let params = serde_json::to_value(&params).with_context(|| format!("serializing params for {method}"))?;
        run_blocking(with_resident_host(&self.plugin, &self.project_root, move |host| {
            let params = params.clone();
            async move { environment_rpc(&host, method, params, Some(timeout)).await }
        }))?
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

/// Acquire the shared resident host for `plugin` and run `call` against a clone
/// of it, retrying once (reap + re-spawn) on a death-like failure. All other
/// errors propagate without a re-spawn.
///
/// The host lives in the process-global [`ResidentHostRegistry`] keyed by the
/// plugin's binary path + mtime + spawn-context, shared with the other
/// full-env resident roles. The lease is held across the RPC `.await` so LRU
/// pressure from another role can never evict the host mid-call.
async fn with_resident_host<T, F, Fut>(plugin: &DiscoveredPlugin, _project_root: &Path, mut call: F) -> Result<T>
where
    F: FnMut(PluginHost) -> Fut,
    Fut: Future<Output = std::result::Result<T, ResidentCallError>>,
{
    let registry = global_resident_host_registry();
    let mtime = binary_mtime_nanos(&plugin.path);
    let context = environment_spawn_context(plugin.manifest.notification_buffer_size);

    let lease = acquire_resident_lease(&registry, plugin, mtime, &context).await?;
    let generation = lease.generation();
    match call(lease.host().clone()).await {
        Ok(value) => Ok(value),
        Err(ResidentCallError::Other(err)) => Err(err),
        Err(ResidentCallError::Death(err)) => {
            // Reap ONLY the exact host that failed (a concurrent caller may have
            // already replaced it), then re-spawn once and retry.
            drop(lease);
            registry.invalidate_generation(&plugin.path, mtime, &context, generation).await;
            let lease = acquire_resident_lease(&registry, plugin, mtime, &context).await?;
            match call(lease.host().clone()).await {
                Ok(value) => Ok(value),
                Err(ResidentCallError::Other(retry_err)) => Err(retry_err),
                Err(ResidentCallError::Death(retry_err)) => Err(retry_err.context(format!(
                    "environment plugin {} still failing after one re-spawn (first error: {err})",
                    plugin.name
                ))),
            }
        }
    }
}

/// AT-MOST-ONCE variant of [`with_resident_host`]: run the side-effectful `call`
/// against the shared resident host EXACTLY once, but heal a stale/idle-dead
/// cached host FIRST so the single attempt lands on a live process. Used by
/// `exec` / `exec_stream`.
///
/// Liveness preflight (closes the idle-death window without risking a double
/// run): the resident registry hands back a cached host without checking its
/// process is still alive, so a host that exited while idle would fail the first
/// exec with `ConnectionLost` even though the command never ran. Before sending
/// the command we `$/ping` the leased host; a death-like ping failure means the
/// process is already gone, so we reap that generation and re-spawn a fresh host
/// — the command has NOT been sent, so this is safe. A structured ping error (or
/// a plugin that does not implement `$/ping`) counts as alive; only a death-like
/// ping outcome triggers the pre-send re-spawn.
///
/// After the ping-verified send, a death-like failure is NOT retried — the
/// command may already have run with side effects on the now-dead process, so a
/// blind re-issue risks double-execution. The dead generation is still reaped so
/// the NEXT exec spawns a fresh host rather than leasing the wedged one.
async fn with_resident_host_once<T, F, Fut>(plugin: &DiscoveredPlugin, _project_root: &Path, call: F) -> Result<T>
where
    F: FnOnce(PluginHost) -> Fut,
    Fut: Future<Output = std::result::Result<T, ResidentCallError>>,
{
    let registry = global_resident_host_registry();
    let mtime = binary_mtime_nanos(&plugin.path);
    let context = environment_spawn_context(plugin.manifest.notification_buffer_size);

    // Lease + liveness preflight. A freshly-spawned host (cache miss) was just
    // handshaked, so it is live and needs no ping; only a cache-reused host can
    // be idle-dead. We ping unconditionally for simplicity — it is a sub-ms
    // round-trip on a healthy host — and re-spawn once if the ping is death-like.
    let mut lease = acquire_resident_lease(&registry, plugin, mtime, &context).await?;
    if ping_is_dead(lease.host()).await {
        let dead_generation = lease.generation();
        drop(lease);
        registry.invalidate_generation(&plugin.path, mtime, &context, dead_generation).await;
        // Re-spawn a fresh host; if THIS one is also unusable we surface the
        // spawn/handshake error (still nothing executed).
        lease = acquire_resident_lease(&registry, plugin, mtime, &context).await?;
    }

    let generation = lease.generation();
    match call(lease.host().clone()).await {
        Ok(value) => Ok(value),
        Err(ResidentCallError::Other(err)) => Err(err),
        Err(ResidentCallError::Death(err)) => {
            // Do NOT re-run the command (it may already have executed with side
            // effects), but the host's process is presumed dead — reap this exact
            // generation so the NEXT exec spawns a fresh host instead of leasing a
            // wedged one and failing until a control call happens to invalidate it.
            drop(lease);
            registry.invalidate_generation(&plugin.path, mtime, &context, generation).await;
            Err(err)
        }
    }
}

/// `$/ping` the host and report whether its transport is dead. For a LIVENESS
/// probe, any plugin response — success OR any RPC error (including a structured
/// error, or a plugin that does not implement `$/ping`) — proves the process is
/// alive; only a CLOSED transport (connection lost / process exited) counts as
/// dead, so the exec preflight never re-spawns a live-but-quiet plugin. This
/// deliberately does NOT use `classify`, which maps unstructured `Rpc` errors to
/// `DeathLike` for its retry-decision purpose — the opposite interpretation of
/// what a liveness probe needs.
///
/// A `Timeout` is treated as ALIVE, not dead: the resident host is shared across
/// roles and callers, so a plugin busy serving another in-flight `exec` may not
/// answer `$/ping` within the short probe window even though its process is fine.
/// Reaping that generation here would `shutdown()` the process and abort the
/// concurrent command. A genuinely wedged host still fails the subsequent exec on
/// its own — same outcome, no collateral damage to a live sibling call. Only a
/// closed transport is an unambiguous death signal (the idle-exit case this
/// preflight exists to catch surfaces as `ConnectionLost`).
async fn ping_is_dead(host: &PluginHost) -> bool {
    const PING_TIMEOUT: Duration = Duration::from_secs(2);
    match host.request_typed_with_timeout("$/ping", None, PING_TIMEOUT).await {
        Ok(_) => false,
        // Any RPC error frame came back over a live transport: the plugin is up.
        Err(HostError::Rpc(_)) => false,
        // A protocol/capability rejection is still a response from a live process.
        Err(HostError::IncompatibleProtocol(_)) | Err(HostError::CapabilityNotSupported(_)) => false,
        // Ambiguous (busy vs wedged) — err toward alive so a busy shared host is
        // never killed out from under a concurrent in-flight command.
        Err(HostError::Timeout(_)) => false,
        // Closed transport: the process is gone / unreachable — unambiguously dead.
        Err(HostError::ConnectionLost) | Err(HostError::ProcessExited(_)) => true,
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

    /// A host whose plugin task answers `initialize` then exits, so its reader
    /// closes and any subsequent request (e.g. `$/ping`) sees `ConnectionLost`.
    async fn dead_after_handshake_host() -> PluginHost {
        let (host_reader, mut plugin_writer) = duplex(8192);
        let (plugin_reader, host_writer) = duplex(8192);
        tokio::spawn(async move {
            let mut reader = BufReader::new(plugin_reader);
            let mut line = String::new();
            if reader.read_line(&mut line).await.expect("read line") > 0 {
                let request: RpcRequest = serde_json::from_str(line.trim()).expect("parse request");
                let result = InitializeResult {
                    protocol_version: "1.0.0".to_string(),
                    plugin_info: PluginInfo {
                        name: "dead".to_string(),
                        version: "0.1.0".to_string(),
                        plugin_kind: PLUGIN_KIND_ENVIRONMENT.to_string(),
                        plugin_kinds: vec![PLUGIN_KIND_ENVIRONMENT.to_string()],
                        description: None,
                    },
                    capabilities: PluginCapabilities::default(),
                    kind_capabilities: std::collections::HashMap::new(),
                };
                write_response(&mut plugin_writer, RpcResponse::ok(request.id, serde_json::json!(result))).await;
            }
            // Read the follow-up `$/ping` request (so the handshake response is
            // fully consumed by the host before we drop the transport), then
            // return WITHOUT responding: the task ends, both duplex ends drop,
            // and the pending `$/ping` awaiter observes ConnectionLost.
            let mut ping = String::new();
            let _ = reader.read_line(&mut ping).await;
            // Task returns here: both duplex ends drop, the host's reader closes.
        });
        PluginHost::from_streams("dead", host_reader, host_writer)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn ping_treats_method_not_supported_as_alive() {
        // The fake answers `$/ping` with METHOD_NOT_SUPPORTED (a structured
        // error): the process is up, so the exec preflight must NOT re-spawn it.
        let spawns = Arc::new(AtomicUsize::new(0));
        let host = fake_environment_host(spawns).await;
        host.handshake().await.expect("handshake");
        assert!(!ping_is_dead(&host).await, "a live plugin that lacks $/ping is alive, not dead");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn ping_detects_a_dead_host() {
        // The plugin exits right after the handshake, so `$/ping` sees the
        // transport close (ConnectionLost) — death-like, so the preflight
        // re-spawns before sending a side-effectful exec.
        let host = dead_after_handshake_host().await;
        host.handshake().await.expect("handshake");
        assert!(ping_is_dead(&host).await, "a plugin whose transport closed is dead");
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
