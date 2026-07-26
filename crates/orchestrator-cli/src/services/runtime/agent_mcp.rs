//! Per-agent MCP server wiring for the ad-hoc `animus chat` and
//! `animus agent run` paths.
//!
//! Workflow runs already give each phase the MCP servers its agent
//! profile / skill declares (the daemon-side `inject_*` machinery in
//! `animus-runtime-shared`). The ad-hoc paths historically wired NO MCP
//! servers at all — an `animus chat` or `animus agent run` agent could not
//! see the Animus tools, let alone a profile's trading or marketing
//! servers.
//!
//! [`assemble_agent_mcp_contract`] closes that gap. It resolves the
//! SELECTED set of server names for one ad-hoc run (profile ∪ skill ∪ CLI
//! additions − the built-in `animus` when disabled), maps each name to its
//! `McpServerDefinition` (or the built-in `animus` stdio surface), and
//! builds the `runtime_contract` value the provider plugin consumes via
//! `SessionRequest.extras.runtime_contract`.
//!
//! The injection itself REUSES the workflow runner's machinery — fed the
//! filtered, per-agent name set rather than the whole project map — so a
//! trading agent gets the trading servers and a marketing agent gets the
//! marketing servers, never a blanket set. OAuth-protected servers (every
//! flow) are rewritten to the local `animus-mcp-proxy` by that same reused
//! machinery, so no resolved secret rides the contract.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use animus_actor::Actor;
use anyhow::{anyhow, Context, Result};
use serde_json::Value;
use tracing::warn;

use animus_runtime_shared::config_context::RuntimeConfigContext;
use animus_runtime_shared::runtime_contract::{
    inject_default_stdio_mcp_for_agent, inject_named_mcp_servers, set_mcp_tool_policy,
};
use orchestrator_config::agent_runtime_config::AgentToolPolicy;

/// The built-in MCP server name. Resolves to the `animus mcp serve` stdio
/// command rather than a project `mcp_servers` entry.
const BUILTIN_ANIMUS_SERVER: &str = "animus";

/// Phase id used only for the diagnostic context the reused
/// `inject_named_mcp_servers` helper threads into its (already pre-empted)
/// unknown-server error. Ad-hoc runs have no phase, so this is a label.
const ADHOC_PHASE_LABEL: &str = "chat";

/// Resolve the `animus` CLI command used to launch the built-in
/// `animus mcp serve` stdio MCP server. The running executable IS the
/// `animus` binary, so `current_exe()` is the authoritative source; falls
/// back to `ANIMUS_HOST_CLI_PATH` when the executable path cannot be read.
fn animus_cli_command() -> Option<String> {
    if let Ok(path) = std::env::current_exe() {
        return Some(path.to_string_lossy().into_owned());
    }
    std::env::var("ANIMUS_HOST_CLI_PATH").ok().filter(|value| !value.trim().is_empty())
}

/// Resolve the per-agent MCP server set for an ad-hoc run and build the
/// `runtime_contract` value the provider receives via
/// `extras.runtime_contract`.
///
/// The resolved server set is:
///
/// 1. `profile_servers` — the agent profile's `mcp_servers` (empty when no
///    `--agent` is selected), UNION
/// 2. `skill_servers` — the loaded skill's `mcp_servers` (empty when no
///    `--skill` is given), UNION
/// 3. `extra_servers` — repeatable `--mcp-server <name>` additions, MINUS
/// 4. the built-in `animus` server when `disable_animus` is set
///    (`--no-animus-mcp`).
///
/// When NO profile/skill is selected (`scope_selected == false`) the
/// baseline set is just `animus` (plus any `--mcp-server` additions) so a
/// plain `animus chat` still has the Animus tools. When a profile/skill IS
/// selected, its declared `mcp_servers` are authoritative — an intentionally
/// empty profile yields an empty set, not the `animus` fallback.
///
/// Each resolved name maps to a server definition:
///
/// * `animus` → the built-in stdio surface via `inject_default_stdio_mcp_with_config`
///   (`["--project-root", <root>, "mcp", "serve"]`).
/// * any other name → the project's `mcp_servers` definition (workflow YAML
///   `mcp_servers` first, then project `.animus` config), injected via the
///   reused [`inject_named_mcp_servers`] machinery (which rewrites OAuth
///   servers — any flow — to the `animus-mcp-proxy` command).
/// * a name that is neither `animus` nor defined anywhere → a clear error.
///
/// Returns `Ok(None)` when the tool cannot speak MCP
/// (`cli/capabilities/supports_mcp` is false, or the tool is unknown) — the
/// caller injects nothing in that case. Otherwise returns the runtime
/// contract `Value`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn assemble_agent_mcp_contract(
    project_root: &Path,
    tool: &str,
    model: &str,
    profile_servers: &[String],
    skill_servers: &[String],
    extra_servers: &[String],
    tool_policy: &AgentToolPolicy,
    scope_selected: bool,
    disable_animus: bool,
    agent_id: Option<&str>,
) -> Result<Option<Value>> {
    // Default (unauthenticated) ad-hoc path: no transport-asserted actor.
    assemble_agent_mcp_contract_with_actor(
        project_root,
        tool,
        model,
        profile_servers,
        skill_servers,
        extra_servers,
        tool_policy,
        scope_selected,
        disable_animus,
        agent_id,
        None,
    )
}

/// Variant of [`assemble_agent_mcp_contract`] that binds the spawned built-in
/// `animus mcp serve` child to a transport-asserted [`Actor`] (relayed as
/// `--actor-json`). Used by `animus chat send --actor-json` so a chat turn's
/// per-user subject / queue / integration tools are scoped to the caller.
///
/// TRUST BOUNDARY: `actor` originates ONLY from the authenticated caller (the
/// transport asserts it via the flag). It is NEVER synthesized from local
/// context, workflow YAML, agent output, or subject content. `None` = global
/// scope.
#[allow(clippy::too_many_arguments)]
pub(crate) fn assemble_agent_mcp_contract_with_actor(
    project_root: &Path,
    tool: &str,
    model: &str,
    profile_servers: &[String],
    skill_servers: &[String],
    extra_servers: &[String],
    tool_policy: &AgentToolPolicy,
    scope_selected: bool,
    disable_animus: bool,
    agent_id: Option<&str>,
    actor: Option<&Actor>,
) -> Result<Option<Value>> {
    // Base contract carries the per-tool `cli` block (including
    // `cli/capabilities/supports_mcp`) plus an empty `mcp` block the
    // inject_* helpers fill in. An unknown/unsupported tool yields `None`.
    let Some(mut runtime_contract) = animus_runtime_shared::build_runtime_contract(tool, model, "") else {
        return Ok(None);
    };

    // Honor `supports_mcp`: a tool that cannot speak MCP gets nothing.
    let supports_mcp =
        runtime_contract.pointer("/cli/capabilities/supports_mcp").and_then(Value::as_bool).unwrap_or(false);
    if !supports_mcp {
        return Ok(None);
    }

    let resolved = resolve_server_names(profile_servers, skill_servers, extra_servers, scope_selected, disable_animus);

    // The built-in `animus` server resolves to the stdio surface, never a
    // project `mcp_servers` lookup. Split it out so the named-server
    // injection only sees real project/workflow definitions.
    let include_animus = resolved.contains(BUILTIN_ANIMUS_SERVER);
    let named_servers: Vec<String> =
        resolved.iter().filter(|name| name.as_str() != BUILTIN_ANIMUS_SERVER).cloned().collect();

    if include_animus {
        // The daemon-side `inject_default_stdio_mcp` resolves the `animus`
        // binary from an init-extension override that the ad-hoc CLI path
        // does not install. Resolve it from the running executable so the
        // built-in `animus mcp serve` stdio surface is wired here. When the
        // binary cannot be resolved the stdio injection is skipped rather
        // than recursively self-launching the wrong binary.
        let mut mcp_config = protocol::McpRuntimeConfig::default();
        if let Some(command) = animus_cli_command() {
            mcp_config.stdio_command = Some(command);
        }
        // Pin the spawned `animus mcp serve` to the selected agent profile
        // (`--agent-id`) so the blocking approval/question tools cannot be
        // routed through another profile's approval_policy via the payload.
        inject_default_stdio_mcp_for_agent(
            &mut runtime_contract,
            &project_root.to_string_lossy(),
            &mcp_config,
            agent_id,
            // Transport-asserted actor (from `animus chat send --actor-json`),
            // relayed to the spawned `animus mcp serve` child so its tools
            // scope per-user. `None` for unauthenticated ad-hoc invocations;
            // the workflow runner supplies the actor on the daemon-driven
            // phase path. Never synthesized here.
            actor,
        );

        // Mirror the shared IPC path: when a stdio MCP command is injected
        // (no HTTP endpoint), flip `mcp.enforce_only` and seed
        // `allowed_tool_prefixes`. Providers that consume the runtime
        // contract's `mcp` block skip native MCP setup unless these are set,
        // so without this the stdio server would be silently ignored by the
        // contract path (the `.mcp.json` path is unaffected).
        let stdio_injected = runtime_contract
            .pointer("/mcp/stdio/command")
            .and_then(Value::as_str)
            .is_some_and(|c| !c.trim().is_empty());
        if stdio_injected {
            let agent_id = runtime_contract
                .pointer("/mcp/agent_id")
                .and_then(Value::as_str)
                .filter(|v| !v.trim().is_empty())
                .unwrap_or(BUILTIN_ANIMUS_SERVER)
                .to_string();
            if let Some(mcp) = runtime_contract.get_mut("mcp").and_then(Value::as_object_mut) {
                mcp.insert("enforce_only".to_string(), Value::Bool(true));
                let prefixes = protocol::default_allowed_tool_prefixes(&agent_id);
                mcp.insert("allowed_tool_prefixes".to_string(), serde_json::json!(prefixes));
            }
        }
    }

    if !named_servers.is_empty() {
        let project_root_str = project_root.to_string_lossy().into_owned();
        let ctx = RuntimeConfigContext::load_for_actor(&project_root_str, actor);

        // Pre-validate each requested name against the SELECTED definition
        // sources so an unknown name yields the spec's clear error rather
        // than the skill-flavored message the reused helper would emit.
        for name in &named_servers {
            let in_workflow = ctx.workflow_config.config.mcp_servers.contains_key(name);
            let in_project = protocol::Config::load_or_default(&project_root_str)
                .map(|config| config.mcp_servers.contains_key(name))
                .unwrap_or(false);
            if !in_workflow && !in_project {
                return Err(anyhow!("unknown MCP server '{name}'; not defined in project mcp_servers"));
            }
        }

        // Reuse the workflow runner's name-keyed injection on the FILTERED
        // per-agent set. It looks each name up in workflow YAML then project
        // config and rewrites OAuth servers (any flow) to the
        // `animus-mcp-proxy` stdio bridge.
        inject_named_mcp_servers(&mut runtime_contract, &project_root_str, &ctx, ADHOC_PHASE_LABEL, &named_servers)?;
    }

    // Apply the selected profile/skill's allow/deny tool policy to
    // `/mcp/tool_policy` so an ad-hoc agent honors the SAME MCP tool
    // restrictions a workflow run would (e.g. a profile that denies
    // `animus.daemon.stop`). `set_mcp_tool_policy` no-ops on an empty policy.
    set_mcp_tool_policy(&mut runtime_contract, tool_policy);

    // Drop the `cli.launch` and `cli.session` blocks before handing the
    // contract to the provider. `build_runtime_contract` builds `cli.launch`
    // from the EMPTY placeholder prompt this assembler passes (the real
    // prompt is owned by the per-turn `SessionRequest`, not known here). A
    // provider that honors `runtime_contract.cli.launch` would otherwise
    // launch its CLI with that empty prompt and never receive the user's
    // message. The ad-hoc path only wants the per-agent `mcp` block; the
    // provider keeps its own launch driven by the request prompt/model.
    // `cli.name` + `cli.capabilities` are left intact as harmless metadata.
    if let Some(cli) = runtime_contract.get_mut("cli").and_then(Value::as_object_mut) {
        cli.remove("launch");
        cli.remove("session");
    }

    Ok(Some(runtime_contract))
}

/// Top-level `.mcp.json` key recording which `mcpServers` entries Animus
/// materialized. Lets a later ad-hoc run REPLACE its own prior generated set
/// (so per-agent scoping holds across runs) without ever touching
/// user-authored entries.
const ANIMUS_MANAGED_MARKER: &str = "_animusManagedServers";

/// Convert an assembled runtime contract's `mcp` block into the
/// claude-code `mcpServers` shape (`{ "<name>": { command|url, ... } }`).
///
/// Provider CLIs that auto-discover a cwd-local `.mcp.json` (claude-code)
/// register MCP servers from that file rather than the runtime contract, so
/// the per-agent set must ALSO be materialized there for tools to actually
/// reach the wrapped CLI. The stdio `animus` server becomes a
/// `command`/`args` entry; each `additional_servers` entry is copied
/// through, EXCEPT that any resolved `Authorization`/auth header is dropped:
/// `.mcp.json` lives in the run cwd (often inside the repo) and persisting a
/// live bearer token to disk would leak the secret. OAuth servers (every
/// flow — `authorization_code`, `manual_bearer`, `client_credentials`,
/// `refresh_token`) are unaffected — the contract assembler already rewrote
/// them to a headerless `animus-mcp-proxy` stdio entry that resolves the
/// live token itself at connect time, so the strip here is defense in depth.
fn contract_mcp_servers_for_mcp_json(runtime_contract: &Value) -> serde_json::Map<String, Value> {
    let mut servers = serde_json::Map::new();

    if let Some(stdio) = runtime_contract.pointer("/mcp/stdio").and_then(Value::as_object) {
        if let Some(command) = stdio.get("command").and_then(Value::as_str) {
            let mut entry = serde_json::Map::new();
            entry.insert("command".to_string(), Value::String(command.to_string()));
            if let Some(args) = stdio.get("args") {
                entry.insert("args".to_string(), args.clone());
            }
            let name = runtime_contract
                .pointer("/mcp/agent_id")
                .and_then(Value::as_str)
                .filter(|v| !v.trim().is_empty())
                .unwrap_or(BUILTIN_ANIMUS_SERVER);
            servers.insert(name.to_string(), Value::Object(entry));
        }
    }

    if let Some(additional) = runtime_contract.pointer("/mcp/additional_servers").and_then(Value::as_object) {
        for (name, entry) in additional {
            let (sanitized, stripped_secret) = strip_secret_material(entry.clone());
            // An entry that required a secret we refuse to persist would be
            // written to `.mcp.json` in an unusable (unauthenticated / env-
            // less) form, so skip materializing it entirely. It still rides
            // on the runtime contract for contract-consuming providers, and
            // for `.mcp.json`-consuming providers an omitted server fails
            // loudly rather than connecting without its credentials.
            if stripped_secret {
                warn!(
                    server = %name,
                    "Skipping .mcp.json materialization for an MCP server whose resolved secret cannot be persisted to disk"
                );
                continue;
            }
            servers.insert(name.clone(), normalize_http_entry(sanitized));
        }
    }

    servers
}

/// The resolved per-agent MCP server map in the plugin-protocol wire shape
/// the provider receives via `extras.mcp_servers` (forwarded verbatim by
/// `PluginSessionBackend::build_run_params` as `AgentRunRequest.mcp_servers`).
///
/// This is the SAME map [`materialize_mcp_json`] writes — built from the
/// same [`contract_mcp_servers_for_mcp_json`] resolution, including its
/// defense-in-depth secret-stripping (OAuth servers arrive already rewritten
/// to the `animus-mcp-proxy` stdio entry, so no resolved secret rides any
/// channel; a non-OAuth server whose entry would need a literal secret is
/// omitted here too) — so the two channels can never disagree about which
/// servers an agent sees. The only difference is the canonical wire field
/// for remote servers: the runtime contract carries `transport`, while the
/// wire shape keys remote entries by `type` (`"http"` | `"sse"`); stdio
/// entries are keyed by `command`/`args`/`env` with no transport marker.
pub(crate) fn contract_mcp_servers_for_wire(runtime_contract: &Value) -> serde_json::Map<String, Value> {
    let mut servers = contract_mcp_servers_for_mcp_json(runtime_contract);
    for entry in servers.values_mut() {
        canonicalize_wire_entry(entry);
    }
    servers
}

/// Convert one resolved server entry from the runtime-contract shape to the
/// canonical wire shape: remote entries (`url` present) get `type` set from
/// `transport` (defaulting to `"http"`); the `transport` key itself is
/// dropped everywhere (stdio entries are identified by `command`).
fn canonicalize_wire_entry(entry: &mut Value) {
    let Some(obj) = entry.as_object_mut() else {
        return;
    };
    let transport = obj.remove("transport").and_then(|value| value.as_str().map(ToOwned::to_owned));
    let is_remote = obj.get("url").and_then(Value::as_str).is_some_and(|url| !url.trim().is_empty());
    if is_remote {
        let kind = match transport.as_deref() {
            Some("sse") => "sse",
            _ => "http",
        };
        obj.insert("type".to_string(), Value::String(kind.to_string()));
    }
}

/// Drop a vacuous `command: ""` / `args: []` from an HTTP MCP entry. The
/// runtime-contract additional-server shape always carries `command`/`args`
/// (empty for HTTP servers), but a claude-style `.mcp.json` selects stdio
/// when a non-empty `command` is present — leaving an empty `command`
/// alongside a `url` is ambiguous and can make the client try (and fail) to
/// launch an empty stdio command instead of connecting to the URL.
fn normalize_http_entry(mut entry: Value) -> Value {
    let has_url = entry.get("url").and_then(Value::as_str).is_some_and(|u| !u.trim().is_empty());
    if !has_url {
        return entry;
    }
    if let Some(obj) = entry.as_object_mut() {
        let command_empty = obj.get("command").and_then(Value::as_str).map(|c| c.trim().is_empty()).unwrap_or(true);
        if command_empty {
            obj.remove("command");
            // An args array is only meaningful with a command.
            if obj.get("args").and_then(Value::as_array).is_some_and(|a| a.is_empty()) {
                obj.remove("args");
            }
        }
    }
    entry
}

/// Strip resolved secret material from an MCP server entry before it is
/// written to the cwd-local `.mcp.json`. That file lives in the run cwd
/// (often inside — and committable to — the user's repo), so any resolved
/// `Authorization: Bearer <token>` is dropped, and `env` values are kept
/// ONLY when they are still unresolved `${VAR}` placeholders (which the
/// provider CLI expands itself at launch — no secret lands on disk). Any
/// literal `env` value is dropped, since it may be a resolved credential.
/// OAuth flows are unaffected: every flow (`authorization_code`,
/// `manual_bearer`, `client_credentials`, `refresh_token`) is already
/// rewritten at contract assembly to a headerless `animus-mcp-proxy` stdio
/// entry that resolves the live token itself at connect time, so this strip
/// is defense in depth against stray resolved secrets.
///
/// Returns `(sanitized_entry, stripped_secret)`. `stripped_secret` is `true`
/// when a literal `env` value or an `Authorization` header was removed — the
/// caller should then SKIP materializing the (now-incomplete) entry to
/// `.mcp.json` rather than write a server that would launch without its
/// credentials.
fn strip_secret_material(mut entry: Value) -> (Value, bool) {
    let mut stripped_secret = false;
    if let Some(obj) = entry.as_object_mut() {
        if let Some(env) = obj.get_mut("env").and_then(Value::as_object_mut) {
            // Keep only `${VAR}`-style passthroughs; drop literal (possibly
            // resolved-secret) values so they never land on disk.
            let before = env.len();
            env.retain(|_, value| value.as_str().is_some_and(is_env_placeholder));
            if env.len() != before {
                stripped_secret = true;
            }
            if env.is_empty() {
                obj.remove("env");
            }
        }
        if let Some(headers) = obj.get_mut("headers").and_then(Value::as_object_mut) {
            let before = headers.len();
            headers.retain(|key, _| !key.eq_ignore_ascii_case("authorization"));
            if headers.len() != before {
                stripped_secret = true;
            }
            if headers.is_empty() {
                obj.remove("headers");
            }
        }
    }
    (entry, stripped_secret)
}

/// Whether an env value is an unresolved `${VAR}` / `${VAR:-default}`
/// placeholder (safe to persist; the provider CLI expands it) rather than a
/// literal value (which may be a resolved secret).
fn is_env_placeholder(value: &str) -> bool {
    let trimmed = value.trim();
    trimmed.starts_with("${") && trimmed.ends_with('}')
}

/// Merge the per-agent MCP server set into the cwd-local `.mcp.json` so a
/// provider CLI that auto-discovers it (claude-code) registers the servers.
///
/// User-authored entries are always preserved. Animus-managed entries are
/// REPLACED wholesale on each run (tracked via the [`ANIMUS_MANAGED_MARKER`]
/// key): a prior run's generated servers are removed before the current
/// run's resolved set is written, so switching from a trading profile to a
/// marketing profile in the same cwd does not leave the trading server
/// visible. A `.mcp.json` that is absent is created; one that is malformed
/// is left untouched (best-effort) so a hand-authored file is never
/// corrupted. Returns the set of server names written, for logging.
///
/// Concurrency note: `.mcp.json` is the cwd-shared file a provider CLI
/// auto-discovers, so two ad-hoc runs with different scopes in the SAME cwd
/// at the same time can race — the second run's write may land before the
/// first provider process reads the file. This mirrors the inherent
/// single-`.mcp.json`-per-directory model the provider CLI imposes (the
/// runtime contract each process carries is unaffected and remains correct).
///
/// TODO(codex-p2): for true per-run isolation under overlapping ad-hoc runs
/// in one cwd, materialize into a per-run config path and point the provider
/// at it once provider plugins accept an explicit MCP-config path.
pub(crate) fn materialize_mcp_json(cwd: &Path, runtime_contract: &Value) -> Result<Vec<String>> {
    let resolved = contract_mcp_servers_for_mcp_json(runtime_contract);
    let mcp_path = cwd.join(".mcp.json");

    let existing = std::fs::read_to_string(&mcp_path);
    // When this run resolves to NO servers and there is no existing file,
    // there is nothing to write or clean — don't create an empty `.mcp.json`.
    if resolved.is_empty() && existing.is_err() {
        return Ok(Vec::new());
    }

    let mut root: serde_json::Map<String, Value> = match &existing {
        Ok(content) => match serde_json::from_str::<Value>(content) {
            Ok(Value::Object(map)) => map,
            // A non-object or malformed `.mcp.json` is left untouched to
            // avoid clobbering a hand-authored file we can't safely merge.
            Ok(_) | Err(_) => return Ok(Vec::new()),
        },
        Err(_) => serde_json::Map::new(),
    };

    // Names Animus wrote on a previous run — remove them first so this run's
    // resolved set fully replaces the prior generated set without disturbing
    // user-authored entries. This runs even when the current set is EMPTY
    // (e.g. `--no-animus-mcp` with no other servers), so an opt-out actually
    // removes the servers a previous run materialized rather than leaving
    // them auto-discoverable.
    let prior_managed: Vec<String> = root
        .get(ANIMUS_MANAGED_MARKER)
        .and_then(Value::as_array)
        .map(|names| names.iter().filter_map(Value::as_str).map(ToOwned::to_owned).collect())
        .unwrap_or_default();

    let mut mcp_servers = root.get("mcpServers").and_then(Value::as_object).cloned().unwrap_or_default();
    for stale in &prior_managed {
        mcp_servers.remove(stale);
    }

    // A name that still exists in `mcpServers` after stale-removal is a
    // user-authored entry (it was never recorded as Animus-managed). We do
    // NOT overwrite it — destroying a user's MCP config just by starting a
    // chat in that cwd would be the more dangerous failure. The tradeoff: a
    // provider that auto-discovers `.mcp.json` then sees the user's entry for
    // that name rather than Animus's resolved project/workflow definition.
    // We surface this loudly so the user can rename one side; the entry still
    // rides on the runtime contract for contract-consuming providers.
    //
    // TODO(codex-p2): consider an isolated generated MCP config dir so a
    // colliding user entry never shadows the resolved per-agent server for
    // `.mcp.json`-consuming providers, without mutating the user's file.
    let mut written = Vec::with_capacity(resolved.len());
    let mut skipped_user_owned = Vec::new();
    for (name, entry) in resolved {
        if mcp_servers.contains_key(&name) {
            skipped_user_owned.push(name);
            continue;
        }
        written.push(name.clone());
        mcp_servers.insert(name, entry);
    }
    written.sort();
    if !skipped_user_owned.is_empty() {
        skipped_user_owned.sort();
        warn!(
            servers = ?skipped_user_owned,
            "Preserving user-authored .mcp.json entries that collide with resolved Animus server names; \
             the agent will use the existing entry for these names. Rename one side to wire the Animus-scoped server instead."
        );
    }

    // Nothing changed and there was nothing managed before — avoid rewriting
    // a purely user-authored file (no marker churn, no needless writes).
    if written.is_empty() && prior_managed.is_empty() {
        return Ok(Vec::new());
    }

    if mcp_servers.is_empty() {
        root.remove("mcpServers");
    } else {
        root.insert("mcpServers".to_string(), Value::Object(mcp_servers));
    }
    if written.is_empty() {
        root.remove(ANIMUS_MANAGED_MARKER);
    } else {
        root.insert(
            ANIMUS_MANAGED_MARKER.to_string(),
            Value::Array(written.iter().cloned().map(Value::String).collect()),
        );
    }

    let serialized = format!("{}\n", serde_json::to_string_pretty(&Value::Object(root))?);
    std::fs::write(&mcp_path, serialized)
        .with_context(|| format!("failed to write MCP config at {}", mcp_path.display()))?;
    Ok(written)
}

/// Materialize the FULL (un-stripped) `runtime_contract` MCP server set into a
/// per-run ISOLATED directory and return the path of the written `.mcp.json`.
///
/// This is the actor-scoped counterpart to [`materialize_mcp_json`]: where the
/// shared cwd file is written actor-STRIPPED (a concurrent run, or a crash
/// before cleanup, must never let another caller inherit the per-turn identity
/// from the persisted, auto-discovered cwd file), this file lives in a fresh,
/// run-private directory and keeps the actor-pinned `animus mcp serve
/// --actor-json <json>` command intact. A provider that locates MCP servers by
/// file auto-discovery can be pointed at THIS path (e.g. claude-code's
/// `--mcp-config`) so the actor reaches that channel too — without ever
/// touching the shared cwd `.mcp.json`.
///
/// The directory is assumed to be run-private and freshly created, so there is
/// no user-file merge and no managed-marker bookkeeping: the resolved set is
/// written wholesale. Returns the `.mcp.json` path, or `None` when the contract
/// resolves to no servers (nothing to point a provider at).
pub(crate) fn materialize_isolated_mcp_json(dir: &Path, runtime_contract: &Value) -> Result<Option<PathBuf>> {
    let resolved = contract_mcp_servers_for_mcp_json(runtime_contract);
    if resolved.is_empty() {
        return Ok(None);
    }

    let mut written: Vec<String> = resolved.keys().cloned().collect();
    written.sort();
    let mut root = serde_json::Map::new();
    root.insert("mcpServers".to_string(), Value::Object(resolved));
    root.insert(ANIMUS_MANAGED_MARKER.to_string(), Value::Array(written.into_iter().map(Value::String).collect()));

    let mcp_path = dir.join(".mcp.json");
    let serialized = format!("{}\n", serde_json::to_string_pretty(&Value::Object(root))?);
    std::fs::write(&mcp_path, serialized)
        .with_context(|| format!("failed to write isolated MCP config at {}", mcp_path.display()))?;
    Ok(Some(mcp_path))
}

/// Return a clone of `runtime_contract` with any `--actor-json <json>` pair
/// stripped from the built-in `animus` server's `/mcp/stdio/args`.
///
/// The transport-asserted actor is a PER-TURN authz identity. It rides the
/// ephemeral `extras.runtime_contract` (the path the turn's own provider
/// session consumes), but it must NEVER be persisted into the cwd-local,
/// auto-discovered `.mcp.json`: that file outlives the turn and is shared, so a
/// later or concurrent invocation from the same directory could silently
/// inherit a previous user's identity (including `admin` claims). Use this to
/// sanitize the contract before [`materialize_mcp_json`].
pub(crate) fn strip_actor_from_contract(runtime_contract: &Value) -> Value {
    let mut sanitized = runtime_contract.clone();
    if let Some(args) = sanitized.pointer_mut("/mcp/stdio/args").and_then(Value::as_array_mut) {
        if let Some(pos) = args.iter().position(|a| a.as_str() == Some("--actor-json")) {
            // Remove the flag and its JSON value (when present).
            let remove_to = (pos + 2).min(args.len());
            args.drain(pos..remove_to);
        }
        args.retain(|arg| arg.as_str() != Some("--require-actor"));
    }
    sanitized
}

/// The MCP server names an `--agent` profile and `--skill` declare,
/// resolved from project config. Either component is empty when its flag is
/// absent.
#[derive(Debug)]
pub(crate) struct ResolvedAgentScope {
    /// The agent profile's `mcp_servers` (empty when `--agent` is absent).
    pub(crate) profile_servers: Vec<String>,
    /// The loaded skill's `mcp_servers` (empty when `--skill` is absent).
    pub(crate) skill_servers: Vec<String>,
    /// The merged allow/deny tool policy from the selected profile + skill.
    /// Empty when neither restricts tools. Applied to `/mcp/tool_policy` so
    /// an ad-hoc agent honors the same MCP tool restrictions a workflow run
    /// would (e.g. a profile that denies `animus.daemon.stop`).
    pub(crate) tool_policy: orchestrator_config::agent_runtime_config::AgentToolPolicy,
    /// The FULL skill application for the selected `--skill` (prompt
    /// fragments, extra_args, env, codex_config_overrides, ...). `None` when
    /// no `--skill` is given. The MCP servers and tool policy above are
    /// extracted from this same application; callers consume the remaining
    /// fields to apply the skill on the ad-hoc paths without resolving twice.
    pub(crate) skill_application: Option<orchestrator_config::skill_definition::SkillApplicationResult>,
}

/// Resolve a caller-supplied profile id to the exact configured map key and
/// its compiled profile. Persisting the map key (rather than the caller's
/// casing) gives conversations a stable canonical identity.
pub(crate) fn resolve_canonical_agent_profile(
    project_root: &Path,
    agent_id: &str,
    actor: Option<&Actor>,
) -> Result<(String, orchestrator_config::agent_runtime_config::AgentProfile)> {
    let config = orchestrator_config::agent_runtime_config::load_agent_runtime_config_with_metadata_for_actor(
        project_root,
        actor,
    )?
    .config;
    config
        .agents
        .iter()
        .find(|(configured_id, _)| configured_id.eq_ignore_ascii_case(agent_id))
        .map(|(configured_id, profile)| (configured_id.clone(), profile.clone()))
        .ok_or_else(|| anyhow!("unknown agent profile '{agent_id}'; not visible in this project and actor scope"))
}

/// Resolve the MCP server names that an `--agent` profile and `--skill`
/// declare, reading the agent runtime config and skill sources for the
/// project.
///
/// `tool` is the provider tool the run targets; it selects the skill's
/// tool-adapter so adapter-declared MCP servers
/// (`adapters.<tool>.mcp_servers`) are included alongside the skill's
/// top-level `mcp_servers`.
///
/// An unknown `--agent` profile and an unknown `--skill` are both hard
/// errors so a typo never silently drops the servers the caller expected
/// (which would leave the agent on the bare `animus` baseline, masking the
/// mistake).
pub(crate) fn resolve_agent_scope(
    project_root: &Path,
    tool: &str,
    agent: Option<&str>,
    skill: Option<&str>,
    actor: Option<&Actor>,
) -> Result<ResolvedAgentScope> {
    let mut tool_policy = orchestrator_config::agent_runtime_config::AgentToolPolicy::default();
    let profile_servers = match agent {
        Some(agent_id) => {
            // Mirror `inject_workflow_mcp_servers_with_project_root`'s
            // precedence exactly: the workflow YAML `agent_profiles` entry's
            // `mcp_servers` win when explicitly declared (even when declared
            // empty); when the YAML profile OMITS them the agent runtime
            // config profile's `mcp_servers` are used as the fallback. A
            // profile present in NEITHER source errors.
            let workflow = orchestrator_core::load_workflow_config_or_default_for_actor(project_root, actor);
            let yaml_profile = workflow.config.agent_profiles.get(agent_id).cloned();
            let runtime_config = orchestrator_core::load_agent_runtime_config_or_default_for_actor(project_root, actor);
            let runtime_profile = runtime_config.agent_profile(agent_id).cloned();

            if yaml_profile.is_none() && runtime_profile.is_none() {
                return Err(anyhow!(
                    "unknown agent profile '{agent_id}'; not defined in this project's workflow YAML agent_profiles or agent runtime config"
                ));
            }

            // Tool policy: the YAML profile's policy wins; fall back to the
            // runtime profile's policy when the YAML profile declares none.
            let yaml_policy = yaml_profile.as_ref().and_then(|p| p.tool_policy.as_ref());
            let policy_source = yaml_policy.or_else(|| {
                runtime_profile.as_ref().map(|p| &p.tool_policy).filter(|p| !p.allow.is_empty() || !p.deny.is_empty())
            });
            if let Some(policy) = policy_source {
                tool_policy.allow.extend(policy.allow.iter().cloned());
                tool_policy.deny.extend(policy.deny.iter().cloned());
            }

            // Servers: YAML profile's win when declared, with the runtime
            // profile as the omitted-field fallback (matching the workflow
            // injection path).
            let yaml_servers = yaml_profile.and_then(|p| p.mcp_servers);
            match yaml_servers {
                Some(servers) => servers,
                None => runtime_profile.map(|p| p.mcp_servers).unwrap_or_default(),
            }
        }
        None => Vec::new(),
    };

    let mut skill_application = None;
    let skill_servers = match skill {
        Some(skill_name) => {
            let resolved = orchestrator_config::skill_resolution::resolve_skills_for_project(
                &[skill_name.to_string()],
                project_root,
            )?;
            match resolved.into_iter().next() {
                // Apply the skill for the target tool so adapter-declared
                // MCP servers (`adapters.<tool>.mcp_servers`) and the skill's
                // tool_policy are merged with the skill's top-level values.
                Some(skill) => {
                    let applied = orchestrator_config::skill_definition::apply_skill_for_tool(&skill.definition, tool);
                    if let Some(policy) = applied.tool_policy.clone() {
                        tool_policy.allow.extend(policy.allow);
                        tool_policy.deny.extend(policy.deny);
                    }
                    let servers = applied.mcp_servers.clone();
                    skill_application = Some(applied);
                    servers
                }
                None => Vec::new(),
            }
        }
        None => Vec::new(),
    };

    // De-duplicate the merged allow/deny lists so a server listed by both the
    // profile and the skill is not repeated.
    tool_policy.allow.sort();
    tool_policy.allow.dedup();
    tool_policy.deny.sort();
    tool_policy.deny.dedup();

    Ok(ResolvedAgentScope { profile_servers, skill_servers, tool_policy, skill_application })
}

/// Build the de-duplicated, ordered-by-name set of MCP server names for an
/// ad-hoc run: profile ∪ skill ∪ extras.
///
/// The built-in `animus` baseline is added ONLY for a plain ad-hoc run that
/// selected no scope (`scope_selected == false`) — so a bare `animus chat`
/// still has the Animus tools. When a `--agent`/`--skill` IS selected, its
/// declared `mcp_servers` are authoritative: an intentionally EMPTY profile
/// (e.g. the builtin `default` with `mcp_servers: []`) yields an empty set,
/// not the `animus` fallback. `--mcp-server` additions always apply, and the
/// `animus` server is dropped when `disable_animus` is set.
fn resolve_server_names(
    profile_servers: &[String],
    skill_servers: &[String],
    extra_servers: &[String],
    scope_selected: bool,
    disable_animus: bool,
) -> BTreeSet<String> {
    let mut set: BTreeSet<String> = BTreeSet::new();

    for name in profile_servers.iter().chain(skill_servers.iter()) {
        let trimmed = name.trim();
        if !trimmed.is_empty() {
            set.insert(trimmed.to_string());
        }
    }

    // Only a plain run (no profile/skill selected) gets the `animus`
    // baseline. A selected profile/skill's declared set — even if empty —
    // is authoritative.
    if !scope_selected {
        set.insert(BUILTIN_ANIMUS_SERVER.to_string());
    }

    for name in extra_servers {
        let trimmed = name.trim();
        if !trimmed.is_empty() {
            set.insert(trimmed.to_string());
        }
    }

    if disable_animus {
        set.remove(BUILTIN_ANIMUS_SERVER);
    }

    set
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn strip_actor_from_contract_removes_actor_pair_only() {
        let contract = serde_json::json!({
            "mcp": { "stdio": { "command": "animus", "args": [
                "--project-root", "/p", "mcp", "serve", "--agent-id", "swe", "--actor-json", r#"{"user_id":"alice"}"#
            ]}}
        });
        let stripped = strip_actor_from_contract(&contract);
        let args: Vec<&str> = stripped
            .pointer("/mcp/stdio/args")
            .and_then(Value::as_array)
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .collect();
        assert!(!args.contains(&"--actor-json"), "actor flag must be dropped: {args:?}");
        assert!(!args.contains(&"--require-actor"), "actor requirement must be dropped with the identity: {args:?}");
        assert!(!args.iter().any(|a| a.contains("alice")), "actor json value must be dropped: {args:?}");
        // The rest of the args (incl. --agent-id) survive intact.
        assert!(args.contains(&"--agent-id") && args.contains(&"swe"), "non-actor args must survive: {args:?}");
        assert!(args.contains(&"serve"), "serve verb must survive: {args:?}");

        // No actor present => unchanged.
        let no_actor = serde_json::json!({ "mcp": { "stdio": { "command": "animus", "args": ["mcp", "serve"] }}});
        assert_eq!(strip_actor_from_contract(&no_actor), no_actor);
    }

    #[test]
    fn plain_chat_defaults_to_animus_only() {
        let resolved = resolve_server_names(&[], &[], &[], false, false);
        assert_eq!(resolved.iter().cloned().collect::<Vec<_>>(), vec!["animus".to_string()]);
    }

    #[test]
    fn selected_profile_with_empty_servers_gets_no_animus_fallback() {
        // A `--agent` whose profile declares NO mcp_servers must receive an
        // EMPTY set, not the animus baseline — the empty scope is intentional.
        let resolved = resolve_server_names(&[], &[], &[], /* scope_selected */ true, false);
        assert!(resolved.is_empty(), "a selected empty-scope profile must get no servers; got {resolved:?}");
    }

    #[test]
    fn profile_servers_do_not_implicitly_add_animus() {
        // A profile that names only `trading` must NOT silently gain the
        // built-in animus server — it is scoped to exactly what it declared.
        let resolved = resolve_server_names(&names(&["trading"]), &[], &[], true, false);
        assert_eq!(resolved.iter().cloned().collect::<Vec<_>>(), vec!["trading".to_string()]);
    }

    #[test]
    fn profile_listing_animus_keeps_it() {
        let resolved = resolve_server_names(&names(&["animus", "trading"]), &[], &[], true, false);
        assert_eq!(resolved.iter().cloned().collect::<Vec<_>>(), vec!["animus".to_string(), "trading".to_string()]);
    }

    #[test]
    fn skill_servers_union_with_profile() {
        let resolved = resolve_server_names(&names(&["trading"]), &names(&["analytics"]), &[], true, false);
        assert_eq!(resolved.iter().cloned().collect::<Vec<_>>(), vec!["analytics".to_string(), "trading".to_string()]);
    }

    #[test]
    fn extra_servers_add_to_the_set() {
        let resolved = resolve_server_names(&names(&["trading"]), &[], &names(&["extra"]), true, false);
        assert_eq!(resolved.iter().cloned().collect::<Vec<_>>(), vec!["extra".to_string(), "trading".to_string()]);
    }

    #[test]
    fn no_animus_drops_the_builtin_from_plain_chat() {
        // Plain chat would default to `animus`; --no-animus-mcp removes it,
        // leaving only the explicit additions.
        let resolved = resolve_server_names(&[], &[], &names(&["extra"]), false, true);
        assert_eq!(resolved.iter().cloned().collect::<Vec<_>>(), vec!["extra".to_string()]);
    }

    #[test]
    fn no_animus_drops_the_builtin_even_when_profile_lists_it() {
        let resolved = resolve_server_names(&names(&["animus", "trading"]), &[], &[], true, true);
        assert_eq!(resolved.iter().cloned().collect::<Vec<_>>(), vec!["trading".to_string()]);
    }

    #[test]
    fn whitespace_only_names_are_ignored() {
        // Plain run (scope not selected): blank profile/skill names are
        // dropped, the baseline animus is added, the trimmed extra included.
        let resolved = resolve_server_names(&names(&["  "]), &names(&[""]), &names(&["  trading  "]), false, false);
        assert_eq!(resolved.iter().cloned().collect::<Vec<_>>(), vec!["animus".to_string(), "trading".to_string()]);
    }

    #[test]
    fn unknown_tool_falls_back_to_the_mcp_capable_provider_shape() {
        // MCP support for a provider tool is decided by the loaded plugin's
        // DECLARED capability (see the discovery-backed wire test in
        // `runtime_agent::provider_client`). When NO provider plugin backs the
        // tool (e.g. an unknown name that cannot be dispatched anyway), the
        // built-in provider fallback still assembles an MCP-capable contract —
        // here, with no scope selected, the baseline `animus` stdio server.
        let tmp = tempfile::tempdir().unwrap();
        let contract = assemble_agent_mcp_contract(
            tmp.path(),
            "definitely-not-a-real-tool",
            "some-model",
            &[],
            &[],
            &[],
            &AgentToolPolicy::default(),
            false,
            false,
            None,
        )
        .unwrap()
        .expect("the built-in provider fallback assembles an MCP-capable contract");
        assert_eq!(contract.pointer("/cli/capabilities/supports_mcp").and_then(Value::as_bool), Some(true));
        assert!(
            contract.pointer("/mcp/stdio/command").and_then(Value::as_str).is_some(),
            "the baseline animus stdio server must be injected; got {contract}"
        );
    }

    #[test]
    fn plain_chat_injects_only_the_animus_stdio_server() {
        let tmp = tempfile::tempdir().unwrap();
        let contract = assemble_agent_mcp_contract(
            tmp.path(),
            "claude",
            "claude-sonnet-4-6",
            &[],
            &[],
            &[],
            &AgentToolPolicy::default(),
            false,
            false,
            None,
        )
        .unwrap()
        .expect("claude supports MCP so a contract is built");

        // The built-in animus server is wired as a stdio command.
        let command = contract.pointer("/mcp/stdio/command").and_then(Value::as_str);
        assert!(command.is_some(), "plain chat must wire the animus stdio server; contract: {contract}");
        let args: Vec<&str> = contract
            .pointer("/mcp/stdio/args")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        assert!(args.contains(&"mcp"), "args: {args:?}");
        assert!(args.contains(&"serve"), "args: {args:?}");
        // No project/workflow servers were named, so none are injected.
        assert!(
            contract.pointer("/mcp/additional_servers").is_none(),
            "plain chat must not inject any additional (project) servers"
        );
    }

    #[test]
    fn selected_agent_pins_identity_on_the_injected_serve_command() {
        let tmp = tempfile::tempdir().unwrap();
        let contract = assemble_agent_mcp_contract(
            tmp.path(),
            "claude",
            "claude-sonnet-4-6",
            &names(&["animus"]),
            &[],
            &[],
            &AgentToolPolicy::default(),
            true,
            false,
            Some("swe"),
        )
        .unwrap()
        .expect("claude supports MCP");
        let args: Vec<&str> = contract
            .pointer("/mcp/stdio/args")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        let position = args.iter().position(|arg| *arg == "--agent-id");
        assert!(position.is_some(), "the injected serve command must pin --agent-id; args: {args:?}");
        assert_eq!(args.get(position.unwrap() + 1), Some(&"swe"), "args: {args:?}");
    }

    #[test]
    fn assembled_contract_strips_cli_launch_so_provider_keeps_its_own() {
        // The assembler passes an EMPTY placeholder prompt to
        // build_runtime_contract, so any `cli.launch` it built would launch
        // the provider CLI with no prompt. The launch/session blocks must be
        // stripped, leaving the mcp block plus harmless cli metadata.
        let tmp = tempfile::tempdir().unwrap();
        let contract = assemble_agent_mcp_contract(
            tmp.path(),
            "claude",
            "claude-sonnet-4-6",
            &[],
            &[],
            &[],
            &AgentToolPolicy::default(),
            false,
            false,
            None,
        )
        .unwrap()
        .expect("claude supports MCP");
        assert!(
            contract.pointer("/cli/launch").is_none(),
            "cli.launch must be stripped so the provider keeps its own prompt-driven launch; contract: {contract}"
        );
        assert!(contract.pointer("/cli/session").is_none(), "cli.session must be stripped; contract: {contract}");
        // The mcp block survives so the provider still wires the servers.
        assert!(
            contract.pointer("/mcp/stdio/command").is_some(),
            "the mcp block must survive the cli strip; contract: {contract}"
        );
    }

    #[test]
    fn no_animus_mcp_skips_the_stdio_server() {
        let tmp = tempfile::tempdir().unwrap();
        let contract = assemble_agent_mcp_contract(
            tmp.path(),
            "claude",
            "claude-sonnet-4-6",
            &[],
            &[],
            &[],
            &AgentToolPolicy::default(),
            false,
            true,
            None,
        )
        .unwrap()
        .expect("claude supports MCP");
        assert!(
            contract.pointer("/mcp/stdio/command").is_none(),
            "--no-animus-mcp must drop the built-in animus stdio server; contract: {contract}"
        );
    }

    #[test]
    fn unknown_named_server_errors_clearly() {
        let tmp = tempfile::tempdir().unwrap();
        let err = assemble_agent_mcp_contract(
            tmp.path(),
            "claude",
            "claude-sonnet-4-6",
            &names(&["trading"]),
            &[],
            &[],
            &AgentToolPolicy::default(),
            true, // scope_selected
            false,
            None,
        )
        .expect_err("an undefined server name must error");
        let message = err.to_string();
        assert!(
            message.contains("unknown MCP server 'trading'"),
            "error should name the unknown server clearly; got: {message}"
        );
    }

    #[test]
    fn selected_profile_with_no_servers_assembles_empty_mcp_block() {
        // `--agent <profile-with-empty-mcp_servers>` must NOT fall back to
        // the animus baseline: an empty scope is authoritative.
        let tmp = tempfile::tempdir().unwrap();
        let contract = assemble_agent_mcp_contract(
            tmp.path(),
            "claude",
            "claude-sonnet-4-6",
            &[], // profile declares no servers
            &[], // no skill servers
            &[], // no --mcp-server
            &AgentToolPolicy::default(),
            true, // scope WAS selected (a --agent was passed)
            false,
            None,
        )
        .unwrap()
        .expect("claude supports MCP");
        assert!(
            contract.pointer("/mcp/stdio/command").is_none(),
            "a selected empty-scope profile must not wire the animus baseline; contract: {contract}"
        );
        assert!(
            contract.pointer("/mcp/additional_servers").is_none(),
            "a selected empty-scope profile must wire no additional servers; contract: {contract}"
        );
    }

    #[test]
    fn http_server_entry_drops_empty_command_for_mcp_json() {
        // An HTTP-only project server has command:"" in the contract; the
        // materialized .mcp.json entry must drop the empty command so a
        // client does not try to launch an empty stdio process.
        let tmp = tempfile::tempdir().unwrap();
        let root = project_with_http_servers(&tmp, &["trading"]);
        let contract = assemble_agent_mcp_contract(
            &root,
            "claude",
            "claude-sonnet-4-6",
            &names(&["trading"]),
            &[],
            &[],
            &AgentToolPolicy::default(),
            true,
            false,
            None,
        )
        .unwrap()
        .expect("claude supports MCP");
        materialize_mcp_json(&root, &contract).unwrap();
        let on_disk: Value = serde_json::from_str(&std::fs::read_to_string(root.join(".mcp.json")).unwrap()).unwrap();
        let entry = on_disk.pointer("/mcpServers/trading").expect("trading entry present");
        assert!(
            entry.get("command").is_none(),
            "an HTTP server's empty command must be dropped from .mcp.json; entry: {entry}"
        );
        assert_eq!(
            entry.pointer("/url").and_then(Value::as_str),
            Some("https://example.com/mcp/trading"),
            "the HTTP url must be preserved; entry: {entry}"
        );
        assert_eq!(entry.pointer("/transport").and_then(Value::as_str), Some("http"));
    }

    /// Write a project `.animus/config.json` whose `mcp_servers` map defines
    /// the named HTTP servers (no command needed for HTTP transport), and
    /// return the canonicalized project root the helper will see.
    fn project_with_http_servers(tmp: &tempfile::TempDir, server_names: &[&str]) -> std::path::PathBuf {
        let root = tmp.path().to_path_buf();
        let mut config = protocol::Config::load_or_default(&root.to_string_lossy())
            .expect("default config should load for a fresh temp project");
        for name in server_names {
            config.mcp_servers.insert(
                name.to_string(),
                protocol::ProjectMcpServerEntry {
                    command: String::new(),
                    args: Vec::new(),
                    env: std::collections::BTreeMap::new(),
                    assign_to: Vec::new(),
                    transport: Some("http".to_string()),
                    url: Some(format!("https://example.com/mcp/{name}")),
                    oauth: None,
                },
            );
        }
        config.save(&root.to_string_lossy()).expect("config save should succeed");
        // Match the canonicalization `Config::config_path` performs so the
        // returned root resolves to the same path the helper reads.
        root.canonicalize().unwrap_or(root)
    }

    fn additional_server_names(contract: &Value) -> Vec<String> {
        contract
            .pointer("/mcp/additional_servers")
            .and_then(Value::as_object)
            .map(|servers| servers.keys().cloned().collect())
            .unwrap_or_default()
    }

    #[test]
    fn trading_profile_gets_only_trading_not_every_project_server() {
        // The project defines THREE servers, but a profile that names only
        // `trading` must receive ONLY trading — proving the per-agent scope
        // is the SELECTED set, not the whole project map.
        let tmp = tempfile::tempdir().unwrap();
        let root = project_with_http_servers(&tmp, &["trading", "hubspot", "analytics"]);

        let contract = assemble_agent_mcp_contract(
            &root,
            "claude",
            "claude-sonnet-4-6",
            &names(&["trading"]),
            &[],
            &[],
            &AgentToolPolicy::default(),
            true,
            false,
            None,
        )
        .unwrap()
        .expect("claude supports MCP");

        let injected = additional_server_names(&contract);
        assert_eq!(injected, vec!["trading".to_string()], "trading profile must get ONLY trading; got {injected:?}");
        assert_eq!(
            contract.pointer("/mcp/additional_servers/trading/url").and_then(Value::as_str),
            Some("https://example.com/mcp/trading")
        );
    }

    #[test]
    fn marketing_profile_gets_its_own_servers_proving_per_agent_scoping() {
        // A different profile in the SAME project gets a DIFFERENT slice —
        // hubspot + analytics — never the trading server.
        let tmp = tempfile::tempdir().unwrap();
        let root = project_with_http_servers(&tmp, &["trading", "hubspot", "analytics"]);

        let contract = assemble_agent_mcp_contract(
            &root,
            "claude",
            "claude-sonnet-4-6",
            &names(&["hubspot", "analytics"]),
            &[],
            &[],
            &AgentToolPolicy::default(),
            true, // scope_selected
            false,
            None,
        )
        .unwrap()
        .expect("claude supports MCP");

        let mut injected = additional_server_names(&contract);
        injected.sort();
        assert_eq!(
            injected,
            vec!["analytics".to_string(), "hubspot".to_string()],
            "marketing profile must get hubspot+analytics, never trading; got {injected:?}"
        );
    }

    #[test]
    fn extra_mcp_server_flag_adds_to_the_resolved_profile_set() {
        let tmp = tempfile::tempdir().unwrap();
        let root = project_with_http_servers(&tmp, &["trading", "extra"]);

        let contract = assemble_agent_mcp_contract(
            &root,
            "claude",
            "claude-sonnet-4-6",
            &names(&["trading"]),
            &[],
            &names(&["extra"]),
            &AgentToolPolicy::default(),
            true, // scope_selected
            false,
            None,
        )
        .unwrap()
        .expect("claude supports MCP");

        let mut injected = additional_server_names(&contract);
        injected.sort();
        assert_eq!(injected, vec!["extra".to_string(), "trading".to_string()], "--mcp-server must ADD to the set");
    }

    #[test]
    fn skill_servers_union_into_the_injected_set() {
        let tmp = tempfile::tempdir().unwrap();
        let root = project_with_http_servers(&tmp, &["trading", "analytics"]);

        // Pass skill servers directly (skill resolution is tested at the
        // resolve_server_names layer); this asserts the union reaches the
        // injected contract.
        let contract = assemble_agent_mcp_contract(
            &root,
            "claude",
            "claude-sonnet-4-6",
            &names(&["trading"]),
            &names(&["analytics"]),
            &[],
            &AgentToolPolicy::default(),
            true, // scope_selected
            false,
            None,
        )
        .unwrap()
        .expect("claude supports MCP");

        let mut injected = additional_server_names(&contract);
        injected.sort();
        assert_eq!(injected, vec!["analytics".to_string(), "trading".to_string()]);
    }

    #[test]
    fn materialize_mcp_json_writes_animus_server_in_claude_shape() {
        let tmp = tempfile::tempdir().unwrap();
        let contract = assemble_agent_mcp_contract(
            tmp.path(),
            "claude",
            "claude-sonnet-4-6",
            &[],
            &[],
            &[],
            &AgentToolPolicy::default(),
            false,
            false,
            None,
        )
        .unwrap()
        .expect("claude supports MCP");
        let written = materialize_mcp_json(tmp.path(), &contract).unwrap();
        assert_eq!(written, vec!["animus".to_string()]);

        let on_disk: Value =
            serde_json::from_str(&std::fs::read_to_string(tmp.path().join(".mcp.json")).unwrap()).unwrap();
        assert!(
            on_disk.pointer("/mcpServers/animus/command").and_then(Value::as_str).is_some(),
            "animus server must be written in claude mcpServers shape; file: {on_disk}"
        );
        let args: Vec<&str> = on_disk
            .pointer("/mcpServers/animus/args")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        assert!(args.contains(&"mcp") && args.contains(&"serve"), "args: {args:?}");
    }

    #[test]
    fn materialize_isolated_mcp_json_keeps_actor_and_skips_shared_cwd() {
        // The isolated file lives in a run-private dir and retains the
        // actor-pinned `--actor-json` command, while the shared cwd file (if
        // any) is unaffected by this call.
        let isolated = tempfile::tempdir().unwrap();
        let contract = serde_json::json!({
            "mcp": {
                "agent_id": "animus",
                "stdio": {
                    "command": "animus",
                    "args": [
                        "--project-root", "/p", "mcp", "serve",
                        "--actor-json", r#"{"user_id":"alice","claims":["admin"]}"#
                    ]
                }
            }
        });

        let path = materialize_isolated_mcp_json(isolated.path(), &contract).unwrap().expect("servers resolved");
        assert_eq!(path, isolated.path().join(".mcp.json"), "isolated config lands in the run-private dir");

        let on_disk: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let args: Vec<&str> = on_disk
            .pointer("/mcpServers/animus/args")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        assert!(args.contains(&"--actor-json"), "isolated config must retain the actor flag: {args:?}");
        assert!(args.iter().any(|a| a.contains("alice")), "isolated config must retain the actor identity: {args:?}");
        // The marker is recorded so a consumer knows the entry is Animus-managed.
        assert!(
            on_disk.pointer(&format!("/{ANIMUS_MANAGED_MARKER}")).is_some(),
            "managed marker must be recorded: {on_disk}"
        );
    }

    #[test]
    fn materialize_isolated_mcp_json_returns_none_without_servers() {
        let isolated = tempfile::tempdir().unwrap();
        let contract = serde_json::json!({ "mcp": {} });
        assert!(
            materialize_isolated_mcp_json(isolated.path(), &contract).unwrap().is_none(),
            "no servers => nothing to point a provider at, and no file is written"
        );
        assert!(!isolated.path().join(".mcp.json").exists(), "no .mcp.json should be created when empty");
    }

    #[test]
    fn materialize_mcp_json_preserves_user_authored_entries() {
        let tmp = tempfile::tempdir().unwrap();
        // A user-authored .mcp.json with a custom server.
        std::fs::write(
            tmp.path().join(".mcp.json"),
            r#"{"mcpServers":{"my-custom":{"command":"foo","args":["bar"]}}}"#,
        )
        .unwrap();

        let contract = assemble_agent_mcp_contract(
            tmp.path(),
            "claude",
            "claude-sonnet-4-6",
            &[],
            &[],
            &[],
            &AgentToolPolicy::default(),
            false,
            false,
            None,
        )
        .unwrap()
        .expect("claude supports MCP");
        materialize_mcp_json(tmp.path(), &contract).unwrap();

        let on_disk: Value =
            serde_json::from_str(&std::fs::read_to_string(tmp.path().join(".mcp.json")).unwrap()).unwrap();
        // Both the user entry and the upserted animus entry are present.
        assert_eq!(
            on_disk.pointer("/mcpServers/my-custom/command").and_then(Value::as_str),
            Some("foo"),
            "user-authored entry must be preserved; file: {on_disk}"
        );
        assert!(
            on_disk.pointer("/mcpServers/animus/command").and_then(Value::as_str).is_some(),
            "animus entry must be upserted; file: {on_disk}"
        );
    }

    #[test]
    fn materialize_mcp_json_proxies_manual_bearer_server_without_the_token() {
        // A manual-bearer HTTP server is rewritten to the local
        // animus-mcp-proxy stdio bridge: the server IS materialized to the
        // cwd-local .mcp.json (which may sit inside the repo), but only as
        // the proxy entry — the live token must never reach disk.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        let bearer_env = "ANIMUS_TEST_ADHOC_BEARER";
        std::env::set_var(bearer_env, "tok-secret-123");
        let mut config = protocol::Config::load_or_default(&root.to_string_lossy()).unwrap();
        config.mcp_servers.insert(
            "trading".to_string(),
            protocol::ProjectMcpServerEntry {
                command: String::new(),
                args: Vec::new(),
                env: std::collections::BTreeMap::new(),
                assign_to: Vec::new(),
                transport: Some("http".to_string()),
                url: Some("https://trading.example.com/mcp".to_string()),
                oauth: Some(serde_json::json!({ "flow": "manual_bearer", "bearer_env": bearer_env })),
            },
        );
        config.save(&root.to_string_lossy()).unwrap();
        let root = root.canonicalize().unwrap_or(root);

        let contract = assemble_agent_mcp_contract(
            &root,
            "claude",
            "claude-sonnet-4-6",
            &names(&["trading"]),
            &[],
            &[],
            &AgentToolPolicy::default(),
            true,
            false,
            None,
        )
        .unwrap()
        .expect("claude supports MCP");
        materialize_mcp_json(&root, &contract).unwrap();
        let on_disk = std::fs::read_to_string(root.join(".mcp.json")).unwrap();
        std::env::remove_var(bearer_env);
        assert!(
            !on_disk.contains("tok-secret-123"),
            "a resolved bearer token must never be persisted to .mcp.json; file: {on_disk}"
        );
        assert!(
            !on_disk.to_ascii_lowercase().contains("authorization"),
            "the Authorization header must never be persisted to .mcp.json; file: {on_disk}"
        );
        // The server is materialized as the proxy stdio entry, not omitted
        // and not written with the upstream URL it cannot authenticate to.
        let parsed: Value = serde_json::from_str(&on_disk).unwrap();
        let command = parsed
            .pointer("/mcpServers/trading/command")
            .and_then(Value::as_str)
            .expect("manual_bearer server must be materialized as the proxy entry");
        assert!(command.contains("animus-mcp-proxy"), "expected the proxy binary; got {command}");
        let args: Vec<&str> = parsed
            .pointer("/mcpServers/trading/args")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        assert!(args.contains(&"--server") && args.contains(&"trading"), "args: {args:?}");
        assert!(parsed.pointer("/mcpServers/trading/url").is_none(), "proxy entry carries no upstream url");
    }

    #[test]
    fn materialize_mcp_json_replaces_prior_animus_servers_but_keeps_user_entries() {
        // Run 1 materializes `trading`; run 2 (a marketing profile) must
        // REPLACE it with hubspot/analytics — the trading server must not
        // linger — while a user-authored entry survives both runs.
        let tmp = tempfile::tempdir().unwrap();
        let root = project_with_http_servers(&tmp, &["trading", "hubspot", "analytics"]);
        // Seed a user-authored entry.
        std::fs::write(root.join(".mcp.json"), r#"{"mcpServers":{"my-custom":{"command":"foo"}}}"#).unwrap();

        let run1 = assemble_agent_mcp_contract(
            &root,
            "claude",
            "claude-sonnet-4-6",
            &names(&["trading"]),
            &[],
            &[],
            &AgentToolPolicy::default(),
            true, // scope_selected
            true, // --no-animus-mcp so only `trading` is materialized
            None,
        )
        .unwrap()
        .expect("claude supports MCP");
        materialize_mcp_json(&root, &run1).unwrap();

        let run2 = assemble_agent_mcp_contract(
            &root,
            "claude",
            "claude-sonnet-4-6",
            &names(&["hubspot", "analytics"]),
            &[],
            &[],
            &AgentToolPolicy::default(),
            true, // scope_selected
            true,
            None,
        )
        .unwrap()
        .expect("claude supports MCP");
        materialize_mcp_json(&root, &run2).unwrap();

        let on_disk: Value = serde_json::from_str(&std::fs::read_to_string(root.join(".mcp.json")).unwrap()).unwrap();
        let servers = on_disk.pointer("/mcpServers").and_then(Value::as_object).unwrap();
        assert!(servers.contains_key("hubspot") && servers.contains_key("analytics"), "run 2 servers present");
        assert!(
            !servers.contains_key("trading"),
            "the prior run's trading server must be removed so per-agent scoping holds; servers: {servers:?}"
        );
        assert!(
            servers.contains_key("my-custom"),
            "user-authored entries must survive across runs; servers: {servers:?}"
        );
    }

    #[test]
    fn empty_resolved_set_removes_prior_animus_servers_but_keeps_user_entries() {
        // After a run materializes `animus`, a later run with an EMPTY set
        // (--no-animus-mcp, no other servers) must REMOVE the prior animus
        // entry so the opt-out is honored, while preserving user entries.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        // Run 1: plain chat → materializes `animus`.
        let run1 = assemble_agent_mcp_contract(
            &root,
            "claude",
            "claude-sonnet-4-6",
            &[],
            &[],
            &[],
            &AgentToolPolicy::default(),
            false,
            false,
            None,
        )
        .unwrap()
        .expect("claude supports MCP");
        materialize_mcp_json(&root, &run1).unwrap();
        // Add a user-authored entry after run 1.
        let mut on_disk: Value =
            serde_json::from_str(&std::fs::read_to_string(root.join(".mcp.json")).unwrap()).unwrap();
        on_disk["mcpServers"]["my-custom"] = serde_json::json!({ "command": "foo" });
        std::fs::write(root.join(".mcp.json"), serde_json::to_string_pretty(&on_disk).unwrap()).unwrap();

        // Run 2: --no-animus-mcp, no other servers → empty resolved set.
        let run2 = assemble_agent_mcp_contract(
            &root,
            "claude",
            "claude-sonnet-4-6",
            &[],
            &[],
            &[],
            &AgentToolPolicy::default(),
            false,
            true,
            None,
        )
        .unwrap()
        .expect("claude supports MCP");
        let written = materialize_mcp_json(&root, &run2).unwrap();
        assert!(written.is_empty(), "an empty resolved set writes no managed servers");

        let after: Value = serde_json::from_str(&std::fs::read_to_string(root.join(".mcp.json")).unwrap()).unwrap();
        assert!(
            after.pointer("/mcpServers/animus").is_none(),
            "the prior animus server must be removed when the new set is empty; file: {after}"
        );
        assert_eq!(
            after.pointer("/mcpServers/my-custom/command").and_then(Value::as_str),
            Some("foo"),
            "user-authored entries must survive the cleanup; file: {after}"
        );
        assert!(after.get(ANIMUS_MANAGED_MARKER).is_none(), "marker should be cleared when nothing is managed");
    }

    #[test]
    fn unknown_agent_profile_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let err = resolve_agent_scope(tmp.path(), "claude", Some("does-not-exist-profile"), None, None)
            .expect_err("an unknown --agent profile must error");
        assert!(
            err.to_string().contains("unknown agent profile 'does-not-exist-profile'"),
            "error should name the unknown profile; got: {err}"
        );
    }

    #[test]
    fn skill_adapter_mcp_servers_are_included_via_apply_skill_for_tool() {
        // resolve_agent_scope applies the skill for the target tool so
        // adapter-declared MCP servers are merged with top-level ones. This
        // asserts the underlying merge that resolve_agent_scope relies on:
        // a skill with `mcp_servers: [top]` and `adapters.claude.mcp_servers:
        // [adapter]` yields BOTH when applied for `claude`.
        let yaml = "name: with-adapter\n\
                    mcp_servers:\n  - top-server\n\
                    adapters:\n  claude:\n    mcp_servers:\n      - adapter-server\n";
        let skill =
            orchestrator_config::skill_definition::parse_skill_definition(yaml).expect("skill yaml should parse");
        let applied = orchestrator_config::skill_definition::apply_skill_for_tool(&skill, "claude");
        assert!(
            applied.mcp_servers.contains(&"top-server".to_string()),
            "top-level skill mcp_servers must be included; got {:?}",
            applied.mcp_servers
        );
        assert!(
            applied.mcp_servers.contains(&"adapter-server".to_string()),
            "adapter-declared mcp_servers for the target tool must be included; got {:?}",
            applied.mcp_servers
        );
    }

    #[test]
    fn known_agent_profile_resolves_its_servers() {
        // v0.6 kernel-purification: the kernel ships no baked agents. Seed a
        // project config defining a `default` profile (as packs / config_source
        // would in production) and install the config_source seam so the loader
        // resolves it, then assert the known profile resolves.
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join(".animus")).unwrap();
        std::fs::write(
            tmp.path().join(".animus").join("workflows.yaml"),
            "tools_allowlist:\n  - cargo\nagents:\n  default:\n    description: Default\n    system_prompt: Default agent\nphases:\n  work:\n    mode: agent\n    agent_id: default\n",
        )
        .unwrap();
        let _config_source_seam =
            orchestrator_config::workflow_config::config_source_client::install_yaml_config_source_base(tmp.path());
        let scope = resolve_agent_scope(tmp.path(), "claude", Some("default"), None, None)
            .expect("a known profile must resolve without error");
        // We don't assert specific servers (config-defined), only that the
        // known profile path does not error.
        let _ = scope.profile_servers;
    }

    #[test]
    fn materialize_mcp_json_skips_servers_with_literal_env_secrets() {
        // A stdio server whose env carries a literal (possibly resolved
        // secret) value cannot be written to .mcp.json without either leaking
        // the secret or launching the server without it — so the entry is
        // omitted entirely rather than materialized incomplete.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        let mut config = protocol::Config::load_or_default(&root.to_string_lossy()).unwrap();
        let mut env = std::collections::BTreeMap::new();
        env.insert("API_KEY".to_string(), "super-secret-value".to_string());
        config.mcp_servers.insert(
            "trading".to_string(),
            protocol::ProjectMcpServerEntry {
                command: "trading-mcp".to_string(),
                args: vec!["--serve".to_string()],
                env,
                assign_to: Vec::new(),
                transport: Some("stdio".to_string()),
                url: None,
                oauth: None,
            },
        );
        config.save(&root.to_string_lossy()).unwrap();
        let root = root.canonicalize().unwrap_or(root);

        let contract = assemble_agent_mcp_contract(
            &root,
            "claude",
            "claude-sonnet-4-6",
            &names(&["trading"]),
            &[],
            &[],
            &AgentToolPolicy::default(),
            true,
            false,
            None,
        )
        .unwrap()
        .expect("claude supports MCP");
        materialize_mcp_json(&root, &contract).unwrap();
        let on_disk = std::fs::read_to_string(root.join(".mcp.json")).unwrap_or_default();
        assert!(
            !on_disk.contains("super-secret-value") && !on_disk.contains("API_KEY"),
            "the resolved secret must never reach .mcp.json; file: {on_disk}"
        );
        // The whole entry is omitted (it would be unusable without its env).
        if !on_disk.is_empty() {
            let parsed: Value = serde_json::from_str(&on_disk).unwrap();
            assert!(
                parsed.pointer("/mcpServers/trading").is_none(),
                "a server needing a stripped secret must be omitted, not written incomplete; file: {on_disk}"
            );
        }
    }

    #[test]
    fn materialize_mcp_json_preserves_placeholder_only_env_servers() {
        // A server whose env is entirely `${VAR}` placeholders carries no
        // resolved secret, so it IS materialized with its env intact (the
        // provider CLI expands the placeholders itself at launch).
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        let mut config = protocol::Config::load_or_default(&root.to_string_lossy()).unwrap();
        let mut env = std::collections::BTreeMap::new();
        env.insert("PLACEHOLDER_KEY".to_string(), "${SOME_VAR}".to_string());
        config.mcp_servers.insert(
            "trading".to_string(),
            protocol::ProjectMcpServerEntry {
                command: "trading-mcp".to_string(),
                args: Vec::new(),
                env,
                assign_to: Vec::new(),
                transport: Some("stdio".to_string()),
                url: None,
                oauth: None,
            },
        );
        config.save(&root.to_string_lossy()).unwrap();
        let root = root.canonicalize().unwrap_or(root);

        let contract = assemble_agent_mcp_contract(
            &root,
            "claude",
            "claude-sonnet-4-6",
            &names(&["trading"]),
            &[],
            &[],
            &AgentToolPolicy::default(),
            true,
            false,
            None,
        )
        .unwrap()
        .expect("claude supports MCP");
        materialize_mcp_json(&root, &contract).unwrap();
        let on_disk: Value = serde_json::from_str(&std::fs::read_to_string(root.join(".mcp.json")).unwrap()).unwrap();
        assert_eq!(
            on_disk.pointer("/mcpServers/trading/env/PLACEHOLDER_KEY").and_then(Value::as_str),
            Some("${SOME_VAR}"),
            "a placeholder-only env server must be materialized with its env intact; file: {on_disk}"
        );
        assert_eq!(on_disk.pointer("/mcpServers/trading/command").and_then(Value::as_str), Some("trading-mcp"));
    }

    #[test]
    fn materialize_mcp_json_does_not_overwrite_user_authored_colliding_entry() {
        // A user-authored `.mcp.json` with an `animus` entry (no Animus
        // marker) must NOT be clobbered just by starting a chat.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        std::fs::write(
            root.join(".mcp.json"),
            r#"{"mcpServers":{"animus":{"command":"user-owned-animus","args":["custom"]}}}"#,
        )
        .unwrap();

        let contract = assemble_agent_mcp_contract(
            &root,
            "claude",
            "claude-sonnet-4-6",
            &[],
            &[],
            &[],
            &AgentToolPolicy::default(),
            false,
            false,
            None,
        )
        .unwrap()
        .expect("claude supports MCP");
        let written = materialize_mcp_json(&root, &contract).unwrap();
        assert!(written.is_empty(), "a colliding user entry must not be reported as written");

        let on_disk: Value = serde_json::from_str(&std::fs::read_to_string(root.join(".mcp.json")).unwrap()).unwrap();
        assert_eq!(
            on_disk.pointer("/mcpServers/animus/command").and_then(Value::as_str),
            Some("user-owned-animus"),
            "the user's animus entry must be preserved, not overwritten; file: {on_disk}"
        );
    }

    #[test]
    fn agent_tool_policy_is_injected_into_the_contract() {
        // A profile's deny list (e.g. denying animus.daemon.stop) must reach
        // /mcp/tool_policy so the ad-hoc agent honors the restriction.
        let tmp = tempfile::tempdir().unwrap();
        let policy = AgentToolPolicy {
            allow: vec!["animus.subject.*".to_string()],
            deny: vec!["animus.daemon.stop".to_string()],
        };
        let contract = assemble_agent_mcp_contract(
            tmp.path(),
            "claude",
            "claude-sonnet-4-6",
            &[],
            &[],
            &[],
            &policy,
            false,
            false,
            None,
        )
        .unwrap()
        .expect("claude supports MCP");

        let deny: Vec<&str> = contract
            .pointer("/mcp/tool_policy/deny")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        assert!(
            deny.contains(&"animus.daemon.stop"),
            "the profile deny list must reach /mcp/tool_policy/deny; contract: {contract}"
        );
        let allow: Vec<&str> = contract
            .pointer("/mcp/tool_policy/allow")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        assert!(allow.contains(&"animus.subject.*"), "the allow list must reach /mcp/tool_policy/allow");
    }

    #[test]
    fn empty_tool_policy_leaves_no_tool_policy_block() {
        let tmp = tempfile::tempdir().unwrap();
        let contract = assemble_agent_mcp_contract(
            tmp.path(),
            "claude",
            "claude-sonnet-4-6",
            &[],
            &[],
            &[],
            &AgentToolPolicy::default(),
            false,
            false,
            None,
        )
        .unwrap()
        .expect("claude supports MCP");
        assert!(
            contract.pointer("/mcp/tool_policy").is_none(),
            "an empty policy must not add a tool_policy block; contract: {contract}"
        );
    }

    #[test]
    fn plain_chat_contract_enables_mcp_enforcement_for_stdio() {
        // Providers that consume the runtime contract's mcp block skip native
        // MCP setup unless enforce_only is set when a stdio command is
        // injected. Assert the assembler flips it (mirrors the IPC path).
        let tmp = tempfile::tempdir().unwrap();
        let contract = assemble_agent_mcp_contract(
            tmp.path(),
            "claude",
            "claude-sonnet-4-6",
            &[],
            &[],
            &[],
            &AgentToolPolicy::default(),
            false,
            false,
            None,
        )
        .unwrap()
        .expect("claude supports MCP");
        assert_eq!(
            contract.pointer("/mcp/enforce_only").and_then(Value::as_bool),
            Some(true),
            "stdio injection must enable enforce_only; contract: {contract}"
        );
        let prefixes =
            contract.pointer("/mcp/allowed_tool_prefixes").and_then(Value::as_array).expect("prefixes present");
        assert!(!prefixes.is_empty(), "allowed_tool_prefixes must be seeded; contract: {contract}");
    }

    #[test]
    fn materialize_mcp_json_leaves_malformed_file_untouched() {
        let tmp = tempfile::tempdir().unwrap();
        let malformed = "{ this is not valid json ]";
        std::fs::write(tmp.path().join(".mcp.json"), malformed).unwrap();

        let contract = assemble_agent_mcp_contract(
            tmp.path(),
            "claude",
            "claude-sonnet-4-6",
            &[],
            &[],
            &[],
            &AgentToolPolicy::default(),
            false,
            false,
            None,
        )
        .unwrap()
        .expect("claude supports MCP");
        let written = materialize_mcp_json(tmp.path(), &contract).unwrap();
        assert!(written.is_empty(), "a malformed .mcp.json must be left untouched, not merged");
        assert_eq!(
            std::fs::read_to_string(tmp.path().join(".mcp.json")).unwrap(),
            malformed,
            "malformed file content must be preserved verbatim"
        );
    }

    #[test]
    fn oauth_authorization_code_server_is_rewritten_to_the_proxy() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        let mut config = protocol::Config::load_or_default(&root.to_string_lossy()).unwrap();
        config.mcp_servers.insert(
            "linear".to_string(),
            protocol::ProjectMcpServerEntry {
                command: String::new(),
                args: Vec::new(),
                env: std::collections::BTreeMap::new(),
                assign_to: Vec::new(),
                transport: Some("http".to_string()),
                url: Some("https://mcp.linear.app/mcp".to_string()),
                oauth: Some(serde_json::json!({ "flow": "authorization_code", "scopes": ["read"] })),
            },
        );
        config.save(&root.to_string_lossy()).unwrap();
        let root = root.canonicalize().unwrap_or(root);

        let contract = assemble_agent_mcp_contract(
            &root,
            "claude",
            "claude-sonnet-4-6",
            &names(&["linear"]),
            &[],
            &[],
            &AgentToolPolicy::default(),
            true,
            false,
            None,
        )
        .unwrap()
        .expect("claude supports MCP");

        let entry = contract.pointer("/mcp/additional_servers/linear").expect("linear server should be injected");
        // OAuth authorization_code servers are repointed at the local
        // animus-mcp-proxy stdio bridge rather than the upstream URL.
        assert_eq!(entry.pointer("/transport").and_then(Value::as_str), Some("stdio"));
        assert!(entry.get("url").is_none(), "proxy entry must not carry the upstream url");
        let command = entry.pointer("/command").and_then(Value::as_str).expect("proxy command");
        assert!(command.contains("animus-mcp-proxy"), "expected the proxy binary; got {command}");
        // The proxy entry rides the wire channel unchanged (stdio shape, no
        // transport/type marker) — proxying the broker flows must not alter
        // the authorization_code treatment.
        let wire = contract_mcp_servers_for_wire(&contract);
        let linear = wire.get("linear").expect("auth_code proxy entry rides the wire");
        assert!(
            linear.get("command").and_then(Value::as_str).is_some_and(|c| c.contains("animus-mcp-proxy")),
            "entry: {linear}"
        );
        assert!(linear.get("transport").is_none() && linear.get("type").is_none(), "entry: {linear}");
    }

    #[test]
    fn wire_map_carries_stdio_animus_entry_without_transport_marker() {
        let tmp = tempfile::tempdir().unwrap();
        let contract = assemble_agent_mcp_contract(
            tmp.path(),
            "claude",
            "claude-sonnet-4-6",
            &[],
            &[],
            &[],
            &AgentToolPolicy::default(),
            false,
            false,
            None,
        )
        .unwrap()
        .expect("claude supports MCP");
        let servers = contract_mcp_servers_for_wire(&contract);
        let animus = servers.get("animus").expect("plain chat resolves the animus stdio server");
        assert!(animus.get("command").and_then(Value::as_str).is_some(), "stdio entry keeps command; got {animus}");
        let args: Vec<&str> = animus
            .get("args")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        assert!(args.contains(&"mcp") && args.contains(&"serve"), "args: {args:?}");
        assert!(animus.get("transport").is_none(), "wire shape carries no transport key; got {animus}");
        assert!(animus.get("type").is_none(), "stdio entries are keyed by command, not type; got {animus}");
    }

    #[test]
    fn wire_map_keys_remote_servers_by_type_not_transport() {
        let tmp = tempfile::tempdir().unwrap();
        let root = project_with_http_servers(&tmp, &["trading"]);
        let contract = assemble_agent_mcp_contract(
            &root,
            "claude",
            "claude-sonnet-4-6",
            &names(&["trading"]),
            &[],
            &[],
            &AgentToolPolicy::default(),
            true,
            false,
            None,
        )
        .unwrap()
        .expect("claude supports MCP");
        let servers = contract_mcp_servers_for_wire(&contract);
        let trading = servers.get("trading").expect("trading server resolved");
        assert_eq!(trading.get("type").and_then(Value::as_str), Some("http"), "entry: {trading}");
        assert_eq!(trading.get("url").and_then(Value::as_str), Some("https://example.com/mcp/trading"));
        assert!(trading.get("transport").is_none(), "the contract's transport key must not leak; got {trading}");
        assert!(trading.get("command").is_none(), "a remote entry carries no command; got {trading}");
    }

    #[test]
    fn wire_map_preserves_sse_transport_as_type() {
        let mut entry = serde_json::json!({ "transport": "sse", "url": "https://example.com/mcp/events" });
        canonicalize_wire_entry(&mut entry);
        assert_eq!(entry.get("type").and_then(Value::as_str), Some("sse"), "entry: {entry}");
        assert!(entry.get("transport").is_none());
    }

    #[test]
    fn wire_map_matches_the_mcp_json_resolved_set() {
        // The wire channel must carry the SAME server set materialize_mcp_json
        // writes — including the proxy rewrite of secret-bearing entries — so
        // the two channels can never disagree.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        let bearer_env = "ANIMUS_TEST_WIRE_BEARER";
        std::env::set_var(bearer_env, "tok-wire-secret");
        let mut config = protocol::Config::load_or_default(&root.to_string_lossy()).unwrap();
        config.mcp_servers.insert(
            "trading".to_string(),
            protocol::ProjectMcpServerEntry {
                command: String::new(),
                args: Vec::new(),
                env: std::collections::BTreeMap::new(),
                assign_to: Vec::new(),
                transport: Some("http".to_string()),
                url: Some("https://trading.example.com/mcp".to_string()),
                oauth: Some(serde_json::json!({ "flow": "manual_bearer", "bearer_env": bearer_env })),
            },
        );
        config.mcp_servers.insert(
            "analytics".to_string(),
            protocol::ProjectMcpServerEntry {
                command: String::new(),
                args: Vec::new(),
                env: std::collections::BTreeMap::new(),
                assign_to: Vec::new(),
                transport: Some("http".to_string()),
                url: Some("https://analytics.example.com/mcp".to_string()),
                oauth: None,
            },
        );
        config.save(&root.to_string_lossy()).unwrap();
        let root = root.canonicalize().unwrap_or(root);

        let contract = assemble_agent_mcp_contract(
            &root,
            "claude",
            "claude-sonnet-4-6",
            &names(&["trading", "analytics"]),
            &[],
            &[],
            &AgentToolPolicy::default(),
            true,
            false,
            None,
        )
        .unwrap()
        .expect("claude supports MCP");
        std::env::remove_var(bearer_env);

        let wire = contract_mcp_servers_for_wire(&contract);
        let json_servers = contract_mcp_servers_for_mcp_json(&contract);
        let json_set: Vec<&String> = json_servers.keys().collect();
        let wire_set: Vec<&String> = wire.keys().collect();
        assert_eq!(wire_set, json_set, "wire and .mcp.json channels must resolve the same server set");
        assert!(wire.contains_key("analytics"), "the plain http server is carried; got {wire_set:?}");
        // The manual_bearer server is carried as the proxy stdio entry —
        // never omitted, never with the resolved token.
        let trading = wire.get("trading").expect("a secret-bearing server rides the wire as the proxy entry");
        let command = trading.get("command").and_then(Value::as_str).expect("proxy command");
        assert!(command.contains("animus-mcp-proxy"), "expected the proxy binary; got {command}");
        assert!(trading.get("url").is_none(), "proxy entry carries no upstream url; got {trading}");
        assert!(trading.get("headers").is_none(), "proxy entry carries no headers; got {trading}");
        let serialized = serde_json::to_string(&wire).unwrap();
        assert!(!serialized.contains("tok-wire-secret"), "no resolved secret may ride the wire map: {serialized}");
        let serialized_json = serde_json::to_string(&json_servers).unwrap();
        assert!(
            !serialized_json.contains("tok-wire-secret"),
            "no resolved secret may reach .mcp.json: {serialized_json}"
        );
    }

    #[test]
    fn client_credentials_server_is_proxied_on_both_channels() {
        // client_credentials needs a token-endpoint POST to resolve; the
        // proxy rewrite means contract assembly performs NO resolution (this
        // test would otherwise need a live token endpoint) and the client
        // secret env value can never appear in either channel.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        let secret_env = "ANIMUS_TEST_WIRE_CC_SECRET";
        std::env::set_var(secret_env, "cc-secret-789");
        let mut config = protocol::Config::load_or_default(&root.to_string_lossy()).unwrap();
        config.mcp_servers.insert(
            "billing".to_string(),
            protocol::ProjectMcpServerEntry {
                command: String::new(),
                args: Vec::new(),
                env: std::collections::BTreeMap::new(),
                assign_to: Vec::new(),
                transport: Some("http".to_string()),
                url: Some("https://billing.example.com/mcp".to_string()),
                oauth: Some(serde_json::json!({
                    "flow": "client_credentials",
                    "token_url": "https://auth.example.com/token",
                    "client_id_env": "ANIMUS_TEST_WIRE_CC_ID",
                    "client_secret_env": secret_env,
                })),
            },
        );
        config.save(&root.to_string_lossy()).unwrap();
        let root = root.canonicalize().unwrap_or(root);

        let contract = assemble_agent_mcp_contract(
            &root,
            "claude",
            "claude-sonnet-4-6",
            &names(&["billing"]),
            &[],
            &[],
            &AgentToolPolicy::default(),
            true,
            false,
            None,
        )
        .unwrap()
        .expect("claude supports MCP");
        std::env::remove_var(secret_env);

        let wire = contract_mcp_servers_for_wire(&contract);
        let billing = wire.get("billing").expect("client_credentials server rides the wire as the proxy entry");
        let command = billing.get("command").and_then(Value::as_str).expect("proxy command");
        assert!(command.contains("animus-mcp-proxy"), "expected the proxy binary; got {command}");
        let args: Vec<&str> = billing
            .get("args")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        assert!(args.contains(&"--server") && args.contains(&"billing"), "args: {args:?}");

        materialize_mcp_json(&root, &contract).unwrap();
        let on_disk = std::fs::read_to_string(root.join(".mcp.json")).unwrap();
        let parsed: Value = serde_json::from_str(&on_disk).unwrap();
        assert!(
            parsed
                .pointer("/mcpServers/billing/command")
                .and_then(Value::as_str)
                .is_some_and(|c| c.contains("animus-mcp-proxy")),
            "client_credentials server must be materialized as the proxy entry; file: {on_disk}"
        );
        for serialized in [serde_json::to_string(&wire).unwrap(), serde_json::to_string(&contract).unwrap(), on_disk] {
            assert!(
                !serialized.contains("cc-secret-789"),
                "the client secret must never appear in any channel: {serialized}"
            );
        }
    }
}
