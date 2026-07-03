//! v0.7 TASK-166 Phase 2: route `animus agent run` execution through a
//! resolved `environment` plugin.
//!
//! Phase 1 delegated only worktree *materialization* (see
//! `orchestrator_core::subject_adapter::adapter`); this module is the *exec*
//! seam. When the run resolves to a NON-LOCAL environment (a container / remote
//! sandbox plugin), the harness command that would have been spawned on the
//! host is instead prepared + executed inside that environment via
//! [`EnvironmentClient`] (`prepare` → `exec_stream` → `teardown`), with the
//! streamed stdout/stderr surfaced through the SAME [`SessionEvent`] channel
//! the local provider-plugin path uses — so persistence, rendering, and
//! output-capture behavior are identical downstream of the spawn point.
//!
//! ## Gate (default = byte-for-byte the current path)
//!
//! [`resolve_exec_environment`] decides whether a run is env-routed:
//!
//! - `ANIMUS_ENVIRONMENT_EXEC` unset → consult the compiled config's
//!   `environment_routing:` table via
//!   [`orchestrator_config::workflow_config::resolve_environment`] (subject
//!   kind `None` — an ad-hoc agent run has no subject; harness = the run's
//!   tool).
//! - `ANIMUS_ENVIRONMENT_EXEC` set to a falsy token (`0` / `false` / `no` /
//!   `off` / empty) → hard kill-switch: local execution even when the config
//!   routes.
//! - `ANIMUS_ENVIRONMENT_EXEC` set to any other value → that value is the
//!   environment plugin id (dev/test override, mirrors Phase 1's
//!   `ANIMUS_ENVIRONMENT_DELEGATE` posture).
//!
//! A resolution of `None`, or of a LOCAL environment id (`local` /
//! `worktree`), leaves the existing host path untouched: the provider plugin
//! spawns the provider CLI on the host exactly as before.
//!
//! ## Failure posture
//!
//! Once a non-local environment IS resolved, the run never silently falls back
//! to local execution — the user asked for isolation, so a missing plugin, a
//! failed `prepare`, or a failed exec fails the run with an actionable error.
//! The single transparent fallback is protocol-level: a plugin that does not
//! implement `environment/exec_stream` (METHOD_NOT_SUPPORTED) is retried with
//! the buffered `environment/exec`, which is safe because METHOD_NOT_SUPPORTED
//! means the command never started.
//!
//! ## Scope note
//!
//! Workflow-phase execution lives in the out-of-tree `workflow_runner` plugin
//! (its `phase_executor` / `workflow_execute` machinery is plugin-private —
//! see `animus-runtime-shared`), so routing PHASE harness execs through the
//! environment plugin is a runner-side change layered on the same
//! [`EnvironmentClient`] contract this module exercises kernel-side.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use animus_environment_protocol::{
    EnvironmentHandle, EnvironmentSpec, ExecResponse, ExecStream, HarnessCommand, RepoRef,
};
use animus_plugin_protocol::error_codes::METHOD_NOT_SUPPORTED;
use animus_session_backend::session::{SessionEvent, SessionRequest, SessionRun};
use anyhow::{anyhow, Result};
use orchestrator_core::EnvironmentClient;
use orchestrator_plugin_host::HostError;
use serde_json::Value;
use tokio::sync::mpsc;

/// Environment ids that mean "run on the host": the resolver treats them as
/// no-routing so the existing local spawn path stays byte-for-byte unchanged.
/// `local` is the documented `environment_routing.default` sentinel;
/// `worktree` is the local git-worktree environment (Phase 1's
/// materialization-only delegate target).
const LOCAL_ENVIRONMENT_IDS: &[&str] = &["local", "worktree"];

/// Default hard cap for an env-routed run when the caller sets no
/// `--timeout-secs` (and no skill timeout applies). Matches the local plugin
/// path's `DEFAULT_PLUGIN_RUN_TIMEOUT_SECS` in
/// `orchestrator-plugin-host::session::plugin_backend` (30 min) so routed and
/// local runs share the same default bound.
const DEFAULT_ENVIRONMENT_RUN_TIMEOUT_SECS: u64 = 1_800;

/// Whether `environment_id` names a LOCAL environment (host execution).
fn is_local_environment(environment_id: &str) -> bool {
    let normalized = environment_id.trim().to_ascii_lowercase();
    LOCAL_ENVIRONMENT_IDS.contains(&normalized.as_str())
}

/// A resolved non-local environment for a run: the plugin id plus any opaque
/// `spec` overrides the matching `environment_routing` rule declared (merged
/// into the compiled [`EnvironmentSpec`] by [`start_environment_session`]).
#[derive(Debug, Clone, PartialEq)]
pub(super) struct ResolvedEnvironment {
    pub(super) id: String,
    pub(super) spec_overrides: Option<BTreeMap<String, Value>>,
}

/// Resolve the NON-LOCAL environment (if any) that should execute this agent
/// run. Returns `None` for the default local path — see the module docs for
/// the full gate semantics.
pub(super) fn resolve_exec_environment(project_root: &Path, harness: &str) -> Option<ResolvedEnvironment> {
    let resolved = match std::env::var("ANIMUS_ENVIRONMENT_EXEC") {
        Ok(raw) => match raw.trim().to_ascii_lowercase().as_str() {
            "" | "0" | "false" | "no" | "off" => return None,
            _ => Some(ResolvedEnvironment { id: raw.trim().to_string(), spec_overrides: None }),
        },
        Err(_) => {
            let config = orchestrator_core::load_workflow_config_or_default(project_root).config;
            let routing = config.environment_routing.as_ref();
            orchestrator_config::workflow_config::resolve_environment(None, Some(harness), None, None, routing)
                .map(|id| ResolvedEnvironment { spec_overrides: routing_spec_overrides(routing, harness, &id), id })
        }
    };
    resolved.filter(|resolved| !is_local_environment(&resolved.id))
}

/// The `spec` overrides carried by the routing rule that selected
/// `resolved_id`, mirroring `resolve_environment`'s first-match-wins pass with
/// subject kind `None` (an ad-hoc agent run has no subject, so a rule with a
/// `match.kind` never matched). `None` when the id came from
/// `routing.default` (only rules carry a spec) or the rule declared none.
fn routing_spec_overrides(
    routing: Option<&orchestrator_config::workflow_config::EnvironmentRouting>,
    harness: &str,
    resolved_id: &str,
) -> Option<BTreeMap<String, Value>> {
    let first_match = routing?.rules.iter().find(|rule| {
        rule.match_on.kind.is_none()
            && rule.match_on.harness.as_deref().is_none_or(|rule_harness| rule_harness == harness)
    })?;
    if first_match.environment != resolved_id {
        return None;
    }
    first_match.spec.clone()
}

/// Start the run inside the resolved non-local environment: resolve the
/// plugin, build the harness command + spec, and hand the pipeline to a
/// background thread that streams [`SessionEvent`]s into the returned
/// [`SessionRun`] — the same handle shape `start_session` produces, so the
/// caller's event loop (persistence, rendering, exit-code handling) is
/// unchanged.
pub(super) fn start_environment_session(
    project_root: &Path,
    environment: &ResolvedEnvironment,
    request: &SessionRequest,
) -> Result<SessionRun> {
    let environment_id = environment.id.as_str();
    let client = EnvironmentClient::resolve(project_root, environment_id).map_err(|err| {
        anyhow!(
            "agent run is routed to environment '{environment_id}' but no usable environment plugin was resolved \
             (the run is NOT executed locally when an environment is requested): {err}"
        )
    })?;
    let (command, stdin) = harness_command_for_request(project_root, request)?;
    let mut spec = environment_spec_for_run(project_root, environment_id, request);
    if let Some(overrides) = environment.spec_overrides.clone() {
        apply_spec_overrides(&mut spec, overrides);
    }
    // Mirror the local path's default run cap: an un-timed env exec would
    // otherwise be unbounded (the client leaves the RPC deadline off), so a
    // hung provider command inside the environment would drain forever.
    let timeout = Some(Duration::from_secs(request.timeout_secs.unwrap_or(DEFAULT_ENVIRONMENT_RUN_TIMEOUT_SECS)));
    let backend_label = format!("environment:{}", client.plugin_name());
    Ok(spawn_environment_run(Arc::new(client), spec, command, stdin, timeout, backend_label))
}

/// Merge a routing rule's opaque `spec` overrides into the compiled
/// [`EnvironmentSpec`]: the wire-typed keys (`image`, `resources`, `env`) land
/// on their typed fields; everything else is carried opaquely on `metadata`,
/// matching the "carried opaquely" contract the config docs promise.
fn apply_spec_overrides(spec: &mut EnvironmentSpec, overrides: BTreeMap<String, Value>) {
    let mut metadata = serde_json::Map::new();
    for (key, value) in overrides {
        match key.as_str() {
            "image" => spec.image = value.as_str().map(ToOwned::to_owned),
            "resources" => spec.resources = Some(value),
            "env" => {
                if let Some(env) = value.as_object() {
                    for (env_key, env_value) in env {
                        if let Some(env_value) = env_value.as_str() {
                            spec.env.insert(env_key.clone(), env_value.to_string());
                        }
                    }
                }
            }
            other => {
                metadata.insert(other.to_string(), value);
            }
        }
    }
    if !metadata.is_empty() {
        spec.metadata = Value::Object(metadata);
    }
}

/// Build the [`HarnessCommand`] that mirrors what would have been spawned
/// locally. The request's assembled `runtime_contract.cli.launch` block wins
/// when present (a caller-supplied contract or a skill launch graft carries
/// the argv the provider would consume); otherwise the launch argv is built
/// from the same
/// [`orchestrator_core::runtime_contract::build_cli_launch_contract`] table
/// the contract path uses. Either way the argv is then normalized for the
/// env-exec transport:
///
/// - machine-output flags are stripped (see [`plain_text_launch_args`]): the
///   env path forwards raw stdout as text deltas rather than running a
///   provider-plugin stream parser, so the CLI must produce plain text, not
///   `stream-json`/`--json` frames;
/// - an explicit `permission_mode` is re-applied via the same
///   [`super::provider_client::apply_permission_mode_to_launch`] transform the
///   local transports/grafts use (idempotent when the graft already applied
///   it), so a mode like `plan` is never silently dropped in favor of the
///   built launch's skip-permissions default;
/// - kernel-mediated approvals (`extras.approvals`) FAIL CLOSED: their gate
///   rides transport-level wiring (claude's `--permission-prompt-tool` MCP
///   hook / prompt-injection on other tools) that does not reach inside an
///   environment, so an env-routed run with approvals requested errors rather
///   than silently running ungated.
///
/// `SessionRequest.env_vars` and any contract launch `env` ride
/// [`HarnessCommand::env`]; a caller-requested cwd below the project root maps
/// to the same relative path inside the environment's workspace (host-absolute
/// paths are meaningless inside a container / remote sandbox).
/// Returns the command plus the stdin payload for it: a contract launch with
/// `prompt_via_stdin: true` deliberately omits the prompt from the argv, so
/// the prompt is fed to the command's stdin instead (mirroring the local
/// transport's handling of that launch contract).
fn harness_command_for_request(
    project_root: &Path,
    request: &SessionRequest,
) -> Result<(HarnessCommand, Option<String>)> {
    if request.extras.pointer("/approvals").and_then(Value::as_bool).unwrap_or(false) {
        return Err(anyhow!(
            "kernel-mediated approvals are not supported for environment-routed runs yet: the approval gate is \
             wired at the provider transport layer, which an environment exec bypasses. Drop --approvals (or the \
             agent profile's approval_policy), or run this agent locally."
        ));
    }

    let system_prompt = request
        .extras
        .pointer("/system_prompt")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|prompt| !prompt.is_empty());
    let is_claude = request.tool.trim().eq_ignore_ascii_case("claude");
    let contract_launch = request.extras.pointer("/runtime_contract/cli/launch").cloned();
    // System prompt mapping (mirrors the local transports): claude carries it
    // as `--append-system-prompt` (injected onto the argv below); other tools'
    // transports prepend it to the prompt body, so merge it into the prompt fed
    // to the built launch / stdin. A non-claude CONTRACT launch embeds its
    // prompt opaquely in the argv, so the system prompt cannot be injected —
    // fail closed rather than silently dropping the agent's instructions.
    let effective_prompt = match system_prompt {
        Some(system) if !is_claude => format!("{system}\n\n{}", request.prompt),
        _ => request.prompt.clone(),
    };
    let prompt_via_stdin = contract_launch
        .as_ref()
        .and_then(|launch| launch.get("prompt_via_stdin"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if system_prompt.is_some() && contract_launch.is_some() && !is_claude && !prompt_via_stdin {
        return Err(anyhow!(
            "a system prompt cannot be applied to tool '{}' through its runtime contract launch inside an \
             environment (the contract embeds the prompt in its argv). Drop the system prompt / skill system \
             fragments, or run this agent locally.",
            request.tool
        ));
    }

    let launch = contract_launch.as_ref().and_then(launch_program_args).or_else(|| {
        orchestrator_core::runtime_contract::build_cli_launch_contract(
            &request.tool,
            &request.model,
            &effective_prompt,
            None,
            None,
        )
        .as_ref()
        .and_then(launch_program_args)
    });
    let Some((program, mut args)) = launch else {
        return Err(anyhow!(
            "cannot build a harness command for tool '{}' to run inside an environment plugin \
             (no runtime_contract cli.launch and no built-in launch table entry for this tool)",
            request.tool
        ));
    };
    if let (Some(system), true) = (system_prompt, is_claude) {
        args.push("--append-system-prompt".to_string());
        args.push(system.to_string());
    }
    let args = plain_text_launch_args(&request.tool, args);
    let args = apply_permission_mode(&request.tool, args, request.permission_mode.as_deref());
    let args = apply_reasoning_effort(
        &request.tool,
        args,
        request.extras.pointer("/reasoning_effort").and_then(Value::as_str),
    );
    // TODO(codex-p2): the env launch still bypasses the remaining
    // provider-transport request transforms the local path applies after launch
    // parsing — most notably `extras.mcp_servers` injection (the animus stdio
    // server / proxy binaries live on the HOST and are not reachable inside a
    // container/remote env without materializing them there) and gemini's
    // approval-mode mapping (which the contract-graft path also does not map
    // today). Folding those in needs an environment-aware MCP story; tracked as
    // a TASK-166 follow-up alongside the runner-side phase-exec integration.

    let mut env: BTreeMap<String, String> = BTreeMap::new();
    if let Some(launch_env) = contract_launch.as_ref().and_then(|launch| launch.get("env")).and_then(Value::as_object) {
        for (key, value) in launch_env {
            if let Some(value) = value.as_str() {
                env.insert(key.clone(), value.to_string());
            }
        }
    }
    // Explicit request env wins over contract launch env on collision.
    for (key, value) in &request.env_vars {
        env.insert(key.clone(), value.clone());
    }

    let stdin = prompt_via_stdin.then(|| effective_prompt.clone());

    Ok((HarnessCommand { program, args, env, cwd: relative_cwd_for_run(project_root, request) }, stdin))
}

/// Map the request's (host) cwd into the environment. `HarnessCommand::cwd` is
/// defined relative to the prepared `workspace_root`, so a caller cwd BELOW the
/// project root maps to the same relative subpath; a cwd AT the project root
/// (the default) maps to `None`, letting the environment run in its primary
/// repo directory. Assumes the single-repo spec this module prepares is
/// checked out at the workspace root (true for the reference worktree plugin);
/// a plugin with a different layout resolves the subpath against its own root.
// TODO(codex-p2): the environment protocol defines `HarnessCommand::cwd`
// relative to `workspace_root` but gives the kernel no way to address "relative
// to the PRIMARY REPO's checkout dir" — a plugin that materializes the primary
// repo under a subdirectory would resolve this subpath against the wrong base.
// Needs a protocol-level answer (e.g. a primary-repo-relative cwd mode on
// ExecRequest) — tracked as a TASK-166 follow-up in animus-protocol.
fn relative_cwd_for_run(project_root: &Path, request: &SessionRequest) -> Option<String> {
    let root = project_root.canonicalize().unwrap_or_else(|_| project_root.to_path_buf());
    let cwd = request.cwd.canonicalize().unwrap_or_else(|_| request.cwd.clone());
    let relative = cwd.strip_prefix(&root).ok()?;
    if relative.as_os_str().is_empty() {
        return None;
    }
    Some(relative.to_string_lossy().to_string())
}

/// Re-apply an explicit permission mode onto the env-exec argv via the same
/// [`super::provider_client::apply_permission_mode_to_launch`] transform the
/// local transports/grafts use. Idempotent when a grafted contract already
/// applied the mode; a no-op when no mode is set (the built launch then runs
/// with the same defaults the local path uses).
fn apply_permission_mode(tool: &str, args: Vec<String>, permission_mode: Option<&str>) -> Vec<String> {
    if permission_mode.map(str::trim).is_none_or(str::is_empty) {
        return args;
    }
    let mut contract = serde_json::json!({ "cli": { "launch": { "args": args } } });
    super::provider_client::apply_permission_mode_to_launch(&mut contract, tool, permission_mode);
    contract
        .pointer("/cli/launch/args")
        .and_then(Value::as_array)
        .map(|args| args.iter().filter_map(Value::as_str).map(ToOwned::to_owned).collect())
        .unwrap_or_default()
}

/// Re-apply an explicit reasoning-effort request onto the env-exec argv,
/// mirroring the contract-graft path: codex carries it as a
/// `model_reasoning_effort` config override; other tools' transports do not
/// map a launch flag for it today, so they pass through.
fn apply_reasoning_effort(tool: &str, args: Vec<String>, reasoning_effort: Option<&str>) -> Vec<String> {
    let Some(effort) = reasoning_effort.map(str::trim).filter(|effort| !effort.is_empty()) else {
        return args;
    };
    if !tool.trim().eq_ignore_ascii_case("codex") {
        return args;
    }
    let mut contract = serde_json::json!({ "cli": { "launch": { "args": args } } });
    animus_runtime_shared::inject_codex_config_overrides_list(
        &mut contract,
        "codex",
        &[format!("model_reasoning_effort={}", effort.to_ascii_lowercase())],
    );
    contract
        .pointer("/cli/launch/args")
        .and_then(Value::as_array)
        .map(|args| args.iter().filter_map(Value::as_str).map(ToOwned::to_owned).collect())
        .unwrap_or_default()
}

/// Strip the machine-output flags from a launch argv so the command emits
/// plain text. The local provider plugins request `stream-json` / `--json` and
/// parse the frames into normalized events; the env-exec path has no such
/// parser — stdout is surfaced directly as text deltas — so the
/// structured-output mode would leak raw JSON into rendered/persisted output.
/// Applied to BOTH the built-in launch table and contract launches (a skill
/// graft rebuilds the launch from the same machine-output table).
fn plain_text_launch_args(tool: &str, args: Vec<String>) -> Vec<String> {
    let normalized = tool.trim().to_ascii_lowercase();
    let (strip_flags, strip_pairs): (&[&str], &[&str]) = match normalized.as_str() {
        "claude" => (&["--verbose"], &["--output-format"]),
        "codex" => (&["--json"], &[]),
        "gemini" => (&[], &["--output-format"]),
        "opencode" | "oai-runner" => (&[], &["--format"]),
        _ => (&[], &[]),
    };
    let mut plain = Vec::with_capacity(args.len());
    let mut iter = args.into_iter().peekable();
    while let Some(arg) = iter.next() {
        if strip_pairs.contains(&arg.as_str()) {
            let _ = iter.next();
            continue;
        }
        if strip_flags.contains(&arg.as_str()) {
            continue;
        }
        plain.push(arg);
    }
    plain
}

/// Extract `(command, args)` from a `cli.launch` JSON block. Returns `None`
/// when the block carries no non-empty `command`.
fn launch_program_args(launch: &Value) -> Option<(String, Vec<String>)> {
    let program = launch.get("command").and_then(Value::as_str).map(str::trim).filter(|cmd| !cmd.is_empty())?;
    let args = launch
        .get("args")
        .and_then(Value::as_array)
        .map(|args| args.iter().filter_map(Value::as_str).map(ToOwned::to_owned).collect())
        .unwrap_or_default();
    Some((program.to_string(), args))
}

/// Build the [`EnvironmentSpec`] for an ad-hoc agent run: the project root is
/// the primary (and only) repo of the workspace; image/resources/metadata are
/// left to the plugin's defaults.
fn environment_spec_for_run(project_root: &Path, environment_id: &str, request: &SessionRequest) -> EnvironmentSpec {
    let mut env = BTreeMap::new();
    for (key, value) in &request.env_vars {
        env.insert(key.clone(), value.clone());
    }
    EnvironmentSpec {
        kind: environment_id.to_string(),
        repos: vec![RepoRef {
            url: project_root.to_string_lossy().to_string(),
            name: None,
            // Pin the caller's current checkout (branch, or exact commit when
            // detached): an env plugin that clones from the url would otherwise
            // materialize the repo's DEFAULT branch, running the agent against
            // different code than the local path's current worktree.
            git_ref: current_git_ref(project_root),
            primary: true,
        }],
        image: None,
        resources: None,
        env,
        metadata: Value::Null,
    }
}

/// The project root's current git ref: the checked-out branch name, or the
/// exact HEAD commit when detached. `None` when the root is not a git repo
/// (the plugin then uses its own default).
// TODO(codex-p2): a clone-based environment plugin materializes only COMMITTED
// state — staged/unstaged/untracked files in a dirty worktree are absent from
// the prepared environment, unlike the local path (which runs in the checkout
// itself). Carrying a dirty-state patch (or a bind-mount capability flag) is a
// protocol/plugin-contract question — tracked as a TASK-166 follow-up alongside
// the runner-side integration.
fn current_git_ref(project_root: &Path) -> Option<String> {
    let git = |args: &[&str]| -> Option<String> {
        let output = std::process::Command::new("git").arg("-C").arg(project_root).args(args).output().ok()?;
        if !output.status.success() {
            return None;
        }
        let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
        (!value.is_empty()).then_some(value)
    };
    match git(&["rev-parse", "--abbrev-ref", "HEAD"]) {
        Some(branch) if branch != "HEAD" => Some(branch),
        Some(_) => git(&["rev-parse", "HEAD"]),
        None => None,
    }
}

/// The prepare/exec/teardown surface the pipeline drives. Production wraps
/// [`EnvironmentClient`]; tests inject a fake so the pipeline's event
/// sequencing and teardown guarantees are exercised without a plugin binary.
pub(super) trait EnvironmentExecBackend: Send + Sync + 'static {
    fn prepare(&self, spec: EnvironmentSpec) -> Result<EnvironmentHandle>;
    fn exec_stream(
        &self,
        handle: &EnvironmentHandle,
        command: HarnessCommand,
        stdin: Option<String>,
        timeout: Option<Duration>,
        on_output: &(dyn Fn(ExecStream, &str) + Send + Sync),
    ) -> Result<ExecResponse>;
    fn exec(
        &self,
        handle: &EnvironmentHandle,
        command: HarnessCommand,
        stdin: Option<String>,
        timeout: Option<Duration>,
    ) -> Result<ExecResponse>;
    fn teardown(&self, handle: &EnvironmentHandle) -> Result<()>;
}

impl EnvironmentExecBackend for EnvironmentClient {
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
        EnvironmentClient::exec_stream(self, handle, command, BTreeMap::new(), stdin, timeout, on_output)
    }

    fn exec(
        &self,
        handle: &EnvironmentHandle,
        command: HarnessCommand,
        stdin: Option<String>,
        timeout: Option<Duration>,
    ) -> Result<ExecResponse> {
        EnvironmentClient::exec(self, handle, command, BTreeMap::new(), stdin, timeout)
    }

    fn teardown(&self, handle: &EnvironmentHandle) -> Result<()> {
        EnvironmentClient::teardown(self, handle)
    }
}

/// Spawn the environment pipeline on a dedicated OS thread (the
/// [`EnvironmentClient`] surface is blocking) and return a [`SessionRun`]
/// whose event stream mirrors the local provider path. Must be called from
/// within a tokio runtime (the forwarder task bridges the pipeline's
/// unbounded sends onto the bounded `SessionRun` channel).
pub(super) fn spawn_environment_run(
    backend: Arc<dyn EnvironmentExecBackend>,
    spec: EnvironmentSpec,
    command: HarnessCommand,
    stdin: Option<String>,
    timeout: Option<Duration>,
    backend_label: String,
) -> SessionRun {
    let (events_tx, events_rx) = mpsc::channel::<SessionEvent>(256);
    // The pipeline thread sends through an unbounded channel: the exec_stream
    // output callback fires inside the client's internal runtime, where a
    // bounded blocking send is not allowed. The forwarder task applies the
    // bounded channel's backpressure on the async side.
    let (pipeline_tx, mut pipeline_rx) = mpsc::unbounded_channel::<SessionEvent>();
    tokio::spawn(async move {
        while let Some(event) = pipeline_rx.recv().await {
            if events_tx.send(event).await.is_err() {
                break;
            }
        }
    });

    let selected_backend = backend_label.clone();
    std::thread::spawn(move || {
        run_environment_pipeline(backend.as_ref(), spec, command, stdin, timeout, &backend_label, &pipeline_tx);
    });

    SessionRun { session_id: None, events: events_rx, selected_backend, fallback_reason: None, pid: None }
}

/// The blocking prepare → exec_stream → teardown pipeline. Emits the same
/// event grammar the provider path does: `Started`, stdout deltas as
/// `TextDelta`, stderr deltas as recoverable `Error` frames (which
/// `to_agent_event` maps to stderr output chunks), then a terminal `Finished`
/// (or an unrecoverable `Error`). Teardown always runs once a handle exists —
/// success or failure — and is idempotent plugin-side.
fn run_environment_pipeline(
    backend: &dyn EnvironmentExecBackend,
    spec: EnvironmentSpec,
    command: HarnessCommand,
    stdin: Option<String>,
    timeout: Option<Duration>,
    backend_label: &str,
    events: &mpsc::UnboundedSender<SessionEvent>,
) {
    let send = |event: SessionEvent| {
        let _ = events.send(event);
    };
    send(SessionEvent::Started { backend: backend_label.to_string(), session_id: None, pid: None });

    let handle = match backend.prepare(spec) {
        Ok(handle) => handle,
        Err(err) => {
            send(SessionEvent::Error {
                message: format!("environment prepare failed for {backend_label}: {err:#}"),
                recoverable: false,
            });
            return;
        }
    };

    let stream_events = events.clone();
    let on_output = move |stream: ExecStream, text: &str| {
        let event = match stream {
            ExecStream::Stdout => SessionEvent::TextDelta { text: text.to_string() },
            ExecStream::Stderr => SessionEvent::Error { message: text.to_string(), recoverable: true },
        };
        let _ = stream_events.send(event);
    };

    let result = match backend.exec_stream(&handle, command.clone(), stdin.clone(), timeout, &on_output) {
        // A plugin without exec_stream support answers METHOD_NOT_SUPPORTED
        // before the command ever starts, so retrying with the buffered exec
        // is safe (no double-execution risk). The aggregated output is then
        // emitted once, post-hoc — deltas were never streamed.
        Err(err) if is_method_not_supported(&err) => {
            backend.exec(&handle, command, stdin, timeout).inspect(|response| {
                if !response.stdout.is_empty() {
                    send(SessionEvent::FinalText { text: response.stdout.clone() });
                }
                if !response.stderr.is_empty() {
                    send(SessionEvent::Error { message: response.stderr.clone(), recoverable: true });
                }
            })
        }
        result => result,
    };

    // Teardown regardless of exec outcome; a teardown failure is surfaced as a
    // recoverable frame (the run's verdict is the exec result, not cleanup).
    if let Err(err) = backend.teardown(&handle) {
        send(SessionEvent::Error {
            message: format!("environment teardown failed for {backend_label} (handle {}): {err:#}", handle.id),
            recoverable: true,
        });
    }

    match result {
        Ok(response) if response.timed_out => send(SessionEvent::Error {
            message: format!(
                "environment exec timed out in {backend_label}{}",
                timeout.map(|t| format!(" after {}s", t.as_secs())).unwrap_or_default()
            ),
            recoverable: false,
        }),
        // A finished command with NO exit code was signal-killed / OOM-killed:
        // downstream status treats `exit_code: None` as success
        // (`unwrap_or_default`), so surface it as a terminal error instead.
        Ok(ExecResponse { exit_code: None, .. }) => send(SessionEvent::Error {
            message: format!(
                "environment exec in {backend_label} ended without an exit code (terminated by a signal?)"
            ),
            recoverable: false,
        }),
        Ok(response) => send(SessionEvent::Finished { exit_code: response.exit_code }),
        Err(err) => send(SessionEvent::Error {
            message: format!("environment exec failed in {backend_label}: {err:#}"),
            recoverable: false,
        }),
    }
}

/// Whether an exec error is the plugin's structured METHOD_NOT_SUPPORTED
/// rejection of `environment/exec_stream` — the only error class where a
/// buffered-exec retry is guaranteed side-effect free.
fn is_method_not_supported(err: &anyhow::Error) -> bool {
    err.chain().any(|cause| {
        matches!(
            cause.downcast_ref::<HostError>(),
            Some(HostError::Rpc(rpc)) if rpc.code == METHOD_NOT_SUPPORTED
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Mutex;

    use crate::shared::test_env_lock;
    use animus_plugin_protocol::RpcError;
    use protocol::test_utils::EnvVarGuard;

    fn sample_request(tool: &str) -> SessionRequest {
        SessionRequest {
            tool: tool.to_string(),
            model: "some-model".to_string(),
            prompt: "do the thing".to_string(),
            cwd: std::path::PathBuf::from("."),
            project_root: None,
            mcp_endpoint: None,
            mcp_servers: None,
            permission_mode: None,
            timeout_secs: None,
            env_vars: Vec::new(),
            extras: serde_json::json!({}),
            actor: None,
        }
    }

    // -----------------------------------------------------------------
    // Gate: resolve_exec_environment
    // -----------------------------------------------------------------

    /// THE Phase-2 regression pin: with no `ANIMUS_ENVIRONMENT_EXEC` and no
    /// `environment_routing:` in config, the gate resolves to `None`, so
    /// `handle_agent_run` takes the exact pre-existing `start_session` local
    /// path — the default is byte-for-byte unchanged.
    #[test]
    fn default_resolves_to_no_environment_so_the_local_path_is_unchanged() {
        let _lock = test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let _gate = EnvVarGuard::set("ANIMUS_ENVIRONMENT_EXEC", None);
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(resolve_exec_environment(tmp.path(), "claude"), None);
    }

    #[test]
    fn env_var_falsy_tokens_disable_routing() {
        let _lock = test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        for token in ["", "0", "false", "no", "off", "False", "OFF", "No"] {
            let _gate = EnvVarGuard::set("ANIMUS_ENVIRONMENT_EXEC", Some(token));
            assert_eq!(resolve_exec_environment(tmp.path(), "claude"), None, "token {token:?} must disable");
        }
    }

    #[test]
    fn env_var_explicit_id_routes_and_local_ids_stay_local() {
        let _lock = test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        {
            let _gate = EnvVarGuard::set("ANIMUS_ENVIRONMENT_EXEC", Some("container"));
            assert_eq!(resolve_exec_environment(tmp.path(), "claude").map(|e| e.id).as_deref(), Some("container"));
        }
        for local in ["local", "worktree", "Worktree", " local "] {
            let _gate = EnvVarGuard::set("ANIMUS_ENVIRONMENT_EXEC", Some(local));
            assert_eq!(resolve_exec_environment(tmp.path(), "claude"), None, "id {local:?} is local");
        }
    }

    #[test]
    fn config_routing_selects_the_environment_for_the_matching_harness() {
        let _lock = test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let _gate = EnvVarGuard::set("ANIMUS_ENVIRONMENT_EXEC", None);
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        std::fs::create_dir_all(root.join(".animus")).unwrap();
        std::fs::write(
            root.join(".animus").join("workflows.yaml"),
            r#"
environment_routing:
  rules:
    - match:
        harness: codex
      environment: sandbox-env
"#,
        )
        .unwrap();
        let _config_source_seam =
            orchestrator_config::workflow_config::config_source_client::install_yaml_config_source_base(&root);

        assert_eq!(resolve_exec_environment(&root, "codex").map(|e| e.id).as_deref(), Some("sandbox-env"));
        // Non-matching harness, no default -> local path.
        assert_eq!(resolve_exec_environment(&root, "claude"), None);
    }

    #[test]
    fn config_routing_local_default_keeps_the_local_path() {
        let _lock = test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let _gate = EnvVarGuard::set("ANIMUS_ENVIRONMENT_EXEC", None);
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        std::fs::create_dir_all(root.join(".animus")).unwrap();
        std::fs::write(
            root.join(".animus").join("workflows.yaml"),
            r#"
environment_routing:
  default: local
"#,
        )
        .unwrap();
        let _config_source_seam =
            orchestrator_config::workflow_config::config_source_client::install_yaml_config_source_base(&root);

        assert_eq!(resolve_exec_environment(&root, "claude"), None, "the documented `default: local` stays local");
    }

    // -----------------------------------------------------------------
    // Harness command construction
    // -----------------------------------------------------------------

    #[test]
    fn harness_command_builds_a_plain_text_launch_for_a_known_tool() {
        let request = sample_request("claude");
        let (command, _stdin) = harness_command_for_request(Path::new("."), &request).expect("known tool builds");
        assert_eq!(command.program, "claude");
        assert!(command.args.contains(&"--print".to_string()), "launch args mirror the local argv: {:?}", command.args);
        assert_eq!(command.args.last().map(String::as_str), Some("do the thing"), "prompt rides the argv");
        // The env-exec path has no provider stream parser, so the machine-output
        // mode the local plugins request must be stripped from the built launch.
        assert!(
            !command.args.iter().any(|arg| arg == "--output-format" || arg == "stream-json" || arg == "--verbose"),
            "machine-output flags stripped for env exec: {:?}",
            command.args
        );
        assert!(command.cwd.is_none(), "cwd at the project root defaults to the environment's primary repo dir");
    }

    #[test]
    fn plain_text_launch_strips_json_mode_per_tool() {
        let codex = plain_text_launch_args(
            "codex",
            vec!["exec".to_string(), "--json".to_string(), "--full-auto".to_string(), "prompt".to_string()],
        );
        assert_eq!(codex, vec!["exec".to_string(), "--full-auto".to_string(), "prompt".to_string()]);

        let gemini = plain_text_launch_args(
            "gemini",
            vec!["--output-format".to_string(), "json".to_string(), "-p".to_string(), "prompt".to_string()],
        );
        assert_eq!(gemini, vec!["-p".to_string(), "prompt".to_string()]);

        // Unknown tools pass through untouched.
        let other = plain_text_launch_args("other", vec!["--json".to_string()]);
        assert_eq!(other, vec!["--json".to_string()]);
    }

    #[test]
    fn harness_command_maps_a_subdirectory_cwd_into_the_workspace() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        std::fs::create_dir_all(root.join("crates").join("web")).unwrap();

        let mut request = sample_request("claude");
        request.cwd = root.join("crates").join("web");
        let (command, _stdin) = harness_command_for_request(&root, &request).expect("builds");
        assert_eq!(
            command.cwd.as_deref(),
            Some("crates/web"),
            "a caller cwd below the project root maps to the same workspace-relative subpath"
        );

        request.cwd = root.clone();
        let (command, _stdin) = harness_command_for_request(&root, &request).expect("builds");
        assert!(command.cwd.is_none(), "a cwd at the project root maps to the environment default");
    }

    #[test]
    fn harness_command_prefers_the_assembled_contract_launch() {
        let mut request = sample_request("claude");
        request.extras = serde_json::json!({
            "runtime_contract": {
                "cli": {
                    "name": "claude",
                    "launch": {
                        "command": "custom-claude",
                        "args": ["--flag", "value"],
                        "env": { "FROM_LAUNCH": "launch", "SHARED": "launch" }
                    }
                }
            }
        });
        request.env_vars = vec![("SHARED".to_string(), "request".to_string())];
        let (command, _stdin) = harness_command_for_request(Path::new("."), &request).expect("contract launch builds");
        assert_eq!(command.program, "custom-claude");
        assert_eq!(command.args, vec!["--flag".to_string(), "value".to_string()]);
        assert_eq!(command.env.get("FROM_LAUNCH").map(String::as_str), Some("launch"));
        assert_eq!(command.env.get("SHARED").map(String::as_str), Some("request"), "request env wins on collision");
    }

    #[test]
    fn harness_command_strips_machine_output_from_a_contract_launch_too() {
        // A skill launch graft rebuilds cli.launch from the machine-output
        // table (stream-json for claude); the env path must strip it there as
        // well, not only on the built-in fallback.
        let mut request = sample_request("claude");
        request.extras = serde_json::json!({
            "runtime_contract": {
                "cli": {
                    "name": "claude",
                    "launch": {
                        "command": "claude",
                        "args": ["--print", "--verbose", "--output-format", "stream-json", "prompt"]
                    }
                }
            }
        });
        let (command, _stdin) = harness_command_for_request(Path::new("."), &request).expect("contract launch builds");
        assert_eq!(command.args, vec!["--print".to_string(), "prompt".to_string()]);
    }

    #[test]
    fn harness_command_feeds_the_prompt_via_stdin_when_the_contract_says_so() {
        let mut request = sample_request("claude");
        request.extras = serde_json::json!({
            "runtime_contract": {
                "cli": {
                    "name": "claude",
                    "launch": {
                        "command": "claude",
                        "args": ["--print"],
                        "prompt_via_stdin": true
                    }
                }
            }
        });
        let (command, stdin) = harness_command_for_request(Path::new("."), &request).expect("builds");
        assert_eq!(stdin.as_deref(), Some("do the thing"), "prompt rides stdin, mirroring the local transport");
        assert!(!command.args.iter().any(|arg| arg == "do the thing"), "prompt stays off the argv");

        // Default: prompt on the argv, no stdin.
        let (_, stdin) = harness_command_for_request(Path::new("."), &sample_request("claude")).expect("builds");
        assert!(stdin.is_none());
    }

    #[test]
    fn routing_rule_spec_overrides_reach_the_environment_spec() {
        let _lock = test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let _gate = EnvVarGuard::set("ANIMUS_ENVIRONMENT_EXEC", None);
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        std::fs::create_dir_all(root.join(".animus")).unwrap();
        std::fs::write(
            root.join(".animus").join("workflows.yaml"),
            r#"
environment_routing:
  rules:
    - match:
        harness: claude
      environment: container-env
      spec:
        image: acme/dev:latest
        env:
          IN_CONTAINER: "1"
        network: none
"#,
        )
        .unwrap();
        let _config_source_seam =
            orchestrator_config::workflow_config::config_source_client::install_yaml_config_source_base(&root);

        let resolved = resolve_exec_environment(&root, "claude").expect("rule routes");
        assert_eq!(resolved.id, "container-env");
        let overrides = resolved.spec_overrides.clone().expect("rule spec carried");

        let mut spec = environment_spec_for_run(&root, &resolved.id, &sample_request("claude"));
        apply_spec_overrides(&mut spec, overrides);
        assert_eq!(spec.image.as_deref(), Some("acme/dev:latest"), "spec.image reaches prepare");
        assert_eq!(spec.env.get("IN_CONTAINER").map(String::as_str), Some("1"), "spec.env reaches prepare");
        assert_eq!(
            spec.metadata.pointer("/network").and_then(Value::as_str),
            Some("none"),
            "unknown spec keys ride metadata opaquely"
        );
    }

    #[test]
    fn harness_command_applies_an_explicit_permission_mode() {
        let mut request = sample_request("claude");
        request.permission_mode = Some("plan".to_string());
        let (command, _stdin) = harness_command_for_request(Path::new("."), &request).expect("builds");
        assert!(
            !command.args.iter().any(|arg| arg == "--dangerously-skip-permissions"),
            "the explicit mode replaces the skip-permissions default: {:?}",
            command.args
        );
        let pos = command
            .args
            .iter()
            .position(|arg| arg == "--permission-mode")
            .expect("permission mode flag applied to the env launch");
        assert_eq!(command.args.get(pos + 1).map(String::as_str), Some("plan"));
    }

    #[test]
    fn environment_spec_pins_the_current_checkout_ref() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let git = |args: &[&str]| {
            assert!(std::process::Command::new("git")
                .arg("-C")
                .arg(&root)
                .args(args)
                .output()
                .expect("git runs")
                .status
                .success());
        };
        git(&["init", "--initial-branch=main"]);
        git(&["-c", "user.email=t@t", "-c", "user.name=t", "commit", "--allow-empty", "-m", "init"]);
        git(&["switch", "-c", "feature/env-exec"]);

        let spec = environment_spec_for_run(&root, "container", &sample_request("claude"));
        assert_eq!(
            spec.repos[0].git_ref.as_deref(),
            Some("feature/env-exec"),
            "the run's spec pins the caller's checked-out branch, not the default branch"
        );

        // Not a git repo -> no ref pinned (plugin default).
        let plain = tempfile::tempdir().unwrap();
        let spec = environment_spec_for_run(plain.path(), "container", &sample_request("claude"));
        assert!(spec.repos[0].git_ref.is_none());
    }

    #[test]
    fn harness_command_maps_the_system_prompt_per_tool() {
        // claude: rides its dedicated --append-system-prompt flag.
        let mut request = sample_request("claude");
        request.extras = serde_json::json!({ "system_prompt": "You are guided." });
        let (command, _stdin) = harness_command_for_request(Path::new("."), &request).expect("builds");
        let pos = command
            .args
            .iter()
            .position(|arg| arg == "--append-system-prompt")
            .expect("claude system prompt rides the flag");
        assert_eq!(command.args.get(pos + 1).map(String::as_str), Some("You are guided."));

        // Other tools: merged into the prompt body of the built launch.
        let mut request = sample_request("codex");
        request.extras = serde_json::json!({ "system_prompt": "You are guided." });
        let (command, _stdin) = harness_command_for_request(Path::new("."), &request).expect("builds");
        assert_eq!(
            command.args.last().map(String::as_str),
            Some("You are guided.\n\ndo the thing"),
            "system prompt prepended to the prompt argv"
        );

        // A non-claude contract launch embeds its prompt opaquely: fail closed
        // rather than silently dropping the instructions.
        let mut request = sample_request("codex");
        request.extras = serde_json::json!({
            "system_prompt": "You are guided.",
            "runtime_contract": { "cli": { "launch": { "command": "codex", "args": ["exec", "prompt"] } } }
        });
        let err = harness_command_for_request(Path::new("."), &request).expect_err("must fail closed");
        assert!(format!("{err}").contains("system prompt"), "error names the dropped input: {err}");
    }

    #[test]
    fn harness_command_applies_codex_reasoning_effort() {
        let mut request = sample_request("codex");
        request.extras = serde_json::json!({ "reasoning_effort": "High" });
        let (command, _stdin) = harness_command_for_request(Path::new("."), &request).expect("builds");
        let pos = command
            .args
            .iter()
            .position(|arg| arg == "model_reasoning_effort=high")
            .expect("codex reasoning effort override applied");
        assert_eq!(command.args.get(pos - 1).map(String::as_str), Some("-c"), "rides a -c config override");
    }

    #[test]
    fn harness_command_fails_closed_when_kernel_approvals_are_requested() {
        let mut request = sample_request("claude");
        request.extras = serde_json::json!({ "approvals": true });
        let err: anyhow::Error = harness_command_for_request(Path::new("."), &request)
            .expect_err("approvals must not silently fail open inside an environment");
        assert!(format!("{err}").contains("approvals"), "error explains the approvals gate: {err}");
    }

    #[test]
    fn harness_command_errors_for_an_unknown_tool_without_a_contract() {
        let request = sample_request("definitely-not-a-tool");
        let err: anyhow::Error =
            harness_command_for_request(Path::new("."), &request).expect_err("unknown tool cannot build");
        assert!(format!("{err}").contains("definitely-not-a-tool"), "error names the tool: {err}");
    }

    // -----------------------------------------------------------------
    // Pipeline event sequencing (fake backend)
    // -----------------------------------------------------------------

    struct FakeBackend {
        prepare_result: Mutex<Option<Result<EnvironmentHandle>>>,
        exec_stream_outcome: Mutex<Option<StreamOutcome>>,
        exec_result: Mutex<Option<Result<ExecResponse>>>,
        exec_calls: AtomicUsize,
        torn_down: AtomicBool,
        last_command: Mutex<Option<HarnessCommand>>,
        last_stdin: Mutex<Option<String>>,
    }

    enum StreamOutcome {
        Deltas(Vec<(ExecStream, String)>, Result<ExecResponse>),
        Fail(anyhow::Error),
    }

    impl FakeBackend {
        fn new() -> Self {
            Self {
                prepare_result: Mutex::new(Some(Ok(sample_handle()))),
                exec_stream_outcome: Mutex::new(None),
                exec_result: Mutex::new(None),
                exec_calls: AtomicUsize::new(0),
                torn_down: AtomicBool::new(false),
                last_command: Mutex::new(None),
                last_stdin: Mutex::new(None),
            }
        }
    }

    fn sample_handle() -> EnvironmentHandle {
        EnvironmentHandle { id: "env-1".to_string(), workspace_root: "/work".to_string(), metadata: Value::Null }
    }

    fn ok_response(exit_code: i32) -> ExecResponse {
        ExecResponse { exit_code: Some(exit_code), stdout: String::new(), stderr: String::new(), timed_out: false }
    }

    impl EnvironmentExecBackend for FakeBackend {
        fn prepare(&self, _spec: EnvironmentSpec) -> Result<EnvironmentHandle> {
            self.prepare_result.lock().unwrap().take().expect("prepare stubbed")
        }

        fn exec_stream(
            &self,
            _handle: &EnvironmentHandle,
            command: HarnessCommand,
            stdin: Option<String>,
            _timeout: Option<Duration>,
            on_output: &(dyn Fn(ExecStream, &str) + Send + Sync),
        ) -> Result<ExecResponse> {
            *self.last_command.lock().unwrap() = Some(command);
            *self.last_stdin.lock().unwrap() = stdin;
            match self.exec_stream_outcome.lock().unwrap().take().expect("exec_stream stubbed") {
                StreamOutcome::Deltas(deltas, result) => {
                    for (stream, text) in deltas {
                        on_output(stream, &text);
                    }
                    result
                }
                StreamOutcome::Fail(err) => Err(err),
            }
        }

        fn exec(
            &self,
            _handle: &EnvironmentHandle,
            _command: HarnessCommand,
            _stdin: Option<String>,
            _timeout: Option<Duration>,
        ) -> Result<ExecResponse> {
            self.exec_calls.fetch_add(1, Ordering::SeqCst);
            self.exec_result.lock().unwrap().take().expect("exec stubbed")
        }

        fn teardown(&self, _handle: &EnvironmentHandle) -> Result<()> {
            self.torn_down.store(true, Ordering::SeqCst);
            Ok(())
        }
    }

    async fn drain(mut run: SessionRun) -> Vec<SessionEvent> {
        let mut events = Vec::new();
        while let Some(event) = run.events.recv().await {
            let finished =
                matches!(event, SessionEvent::Finished { .. } | SessionEvent::Error { recoverable: false, .. });
            events.push(event);
            if finished {
                break;
            }
        }
        events
    }

    fn spec_for_test() -> EnvironmentSpec {
        EnvironmentSpec {
            kind: "container".to_string(),
            repos: Vec::new(),
            image: None,
            resources: None,
            env: BTreeMap::new(),
            metadata: Value::Null,
        }
    }

    fn command_for_test() -> HarnessCommand {
        HarnessCommand {
            program: "claude".to_string(),
            args: vec!["--print".to_string()],
            env: BTreeMap::new(),
            cwd: None,
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn env_routed_run_streams_deltas_through_the_session_channel_and_tears_down() {
        let backend = Arc::new(FakeBackend::new());
        *backend.exec_stream_outcome.lock().unwrap() = Some(StreamOutcome::Deltas(
            vec![(ExecStream::Stdout, "out-1".to_string()), (ExecStream::Stderr, "err-1".to_string())],
            Ok(ok_response(7)),
        ));

        let run = spawn_environment_run(
            backend.clone(),
            spec_for_test(),
            command_for_test(),
            None,
            None,
            "environment:container".to_string(),
        );
        assert_eq!(run.selected_backend, "environment:container");
        let events = drain(run).await;

        assert!(
            matches!(&events[0], SessionEvent::Started { backend, .. } if backend == "environment:container"),
            "first frame is Started: {events:?}"
        );
        assert!(
            events.iter().any(|e| matches!(e, SessionEvent::TextDelta { text } if text == "out-1")),
            "stdout delta rides TextDelta (same channel as the local path): {events:?}"
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, SessionEvent::Error { message, recoverable: true } if message == "err-1")),
            "stderr delta rides a recoverable Error frame (-> stderr chunk): {events:?}"
        );
        assert!(
            matches!(events.last(), Some(SessionEvent::Finished { exit_code: Some(7) })),
            "terminal frame carries the exec exit code: {events:?}"
        );
        assert!(backend.torn_down.load(Ordering::SeqCst), "teardown runs after a successful exec");
        let command = backend.last_command.lock().unwrap().clone().expect("command routed to the backend");
        assert_eq!(command.program, "claude", "the HarnessCommand reaches EnvironmentExecBackend::exec_stream");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn prepare_failure_fails_the_run_without_executing_locally() {
        let backend = Arc::new(FakeBackend::new());
        *backend.prepare_result.lock().unwrap() = Some(Err(anyhow!("no docker daemon")));

        let run = spawn_environment_run(
            backend.clone(),
            spec_for_test(),
            command_for_test(),
            None,
            None,
            "environment:container".to_string(),
        );
        let events = drain(run).await;
        assert!(
            matches!(
                events.last(),
                Some(SessionEvent::Error { message, recoverable: false }) if message.contains("prepare failed")
            ),
            "prepare failure is a terminal error, never a silent local run: {events:?}"
        );
        assert_eq!(backend.exec_calls.load(Ordering::SeqCst), 0, "nothing executed after a failed prepare");
        assert!(!backend.torn_down.load(Ordering::SeqCst), "no handle -> no teardown");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn exec_failure_still_tears_down_and_fails_the_run() {
        let backend = Arc::new(FakeBackend::new());
        *backend.exec_stream_outcome.lock().unwrap() = Some(StreamOutcome::Fail(anyhow!("plugin died")));

        let run = spawn_environment_run(
            backend.clone(),
            spec_for_test(),
            command_for_test(),
            None,
            None,
            "environment:container".to_string(),
        );
        let events = drain(run).await;
        assert!(
            matches!(
                events.last(),
                Some(SessionEvent::Error { message, recoverable: false }) if message.contains("exec failed")
            ),
            "exec failure is terminal: {events:?}"
        );
        assert!(backend.torn_down.load(Ordering::SeqCst), "teardown still runs after a failed exec");
        assert_eq!(backend.exec_calls.load(Ordering::SeqCst), 0, "a non-METHOD_NOT_SUPPORTED failure is not retried");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn method_not_supported_falls_back_to_buffered_exec() {
        let backend = Arc::new(FakeBackend::new());
        *backend.exec_stream_outcome.lock().unwrap() =
            Some(StreamOutcome::Fail(anyhow::Error::from(HostError::Rpc(RpcError {
                code: METHOD_NOT_SUPPORTED,
                message: "no exec_stream".to_string(),
                data: None,
            }))));
        *backend.exec_result.lock().unwrap() = Some(Ok(ExecResponse {
            exit_code: Some(0),
            stdout: "buffered-out".to_string(),
            stderr: "buffered-err".to_string(),
            timed_out: false,
        }));

        let run = spawn_environment_run(
            backend.clone(),
            spec_for_test(),
            command_for_test(),
            None,
            None,
            "environment:container".to_string(),
        );
        let events = drain(run).await;
        assert_eq!(backend.exec_calls.load(Ordering::SeqCst), 1, "buffered exec retried exactly once");
        assert!(
            events.iter().any(|e| matches!(e, SessionEvent::FinalText { text } if text == "buffered-out")),
            "aggregated stdout emitted once via FinalText: {events:?}"
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, SessionEvent::Error { message, recoverable: true } if message == "buffered-err")),
            "aggregated stderr emitted as a recoverable frame: {events:?}"
        );
        assert!(matches!(events.last(), Some(SessionEvent::Finished { exit_code: Some(0) })), "events: {events:?}");
        assert!(backend.torn_down.load(Ordering::SeqCst));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn missing_exit_code_is_a_terminal_error() {
        // exit_code: None + timed_out: false = the command was signal-killed;
        // Finished { exit_code: None } would read as SUCCESS downstream.
        let backend = Arc::new(FakeBackend::new());
        *backend.exec_stream_outcome.lock().unwrap() = Some(StreamOutcome::Deltas(
            Vec::new(),
            Ok(ExecResponse { exit_code: None, stdout: String::new(), stderr: String::new(), timed_out: false }),
        ));

        let run = spawn_environment_run(
            backend.clone(),
            spec_for_test(),
            command_for_test(),
            None,
            None,
            "environment:container".to_string(),
        );
        let events = drain(run).await;
        assert!(
            matches!(
                events.last(),
                Some(SessionEvent::Error { message, recoverable: false }) if message.contains("without an exit code")
            ),
            "a signal-killed command must not be reported as success: {events:?}"
        );
        assert!(backend.torn_down.load(Ordering::SeqCst));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn timed_out_exec_is_a_terminal_error() {
        let backend = Arc::new(FakeBackend::new());
        *backend.exec_stream_outcome.lock().unwrap() = Some(StreamOutcome::Deltas(
            Vec::new(),
            Ok(ExecResponse { exit_code: None, stdout: String::new(), stderr: String::new(), timed_out: true }),
        ));

        let run = spawn_environment_run(
            backend.clone(),
            spec_for_test(),
            command_for_test(),
            None,
            Some(Duration::from_secs(5)),
            "environment:container".to_string(),
        );
        let events = drain(run).await;
        assert!(
            matches!(
                events.last(),
                Some(SessionEvent::Error { message, recoverable: false }) if message.contains("timed out")
            ),
            "timeout is terminal: {events:?}"
        );
        assert!(backend.torn_down.load(Ordering::SeqCst));
    }
}
