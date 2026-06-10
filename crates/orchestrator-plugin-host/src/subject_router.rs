use std::collections::HashMap;
use std::time::Duration;

use animus_plugin_protocol::RpcError;
use anyhow::{anyhow, Result};
use serde_json::Value;

use crate::PluginHost;

/// Generous upper bound for a single subject-backend RPC routed through
/// [`SubjectRouter::route_call`]. Subject ops are CRUD against a local
/// store and should complete in milliseconds; the deadline exists so a
/// wedged plugin (alive but not responding) cannot pin a daemon dispatch
/// task forever on the otherwise-untimed request path. Expiry surfaces as
/// an `RpcError` with the protocol's `TIMEOUT` code.
const SUBJECT_ROUTE_TIMEOUT: Duration = Duration::from_mins(2);

/// Subject-kind registration parsed from a plugin's declared
/// `subject_kinds`. A pattern ending in `.*` matches any kind whose dotted
/// prefix matches everything before the trailing `*`.
#[derive(Debug, Clone)]
struct KindPattern {
    /// Raw pattern as declared by the plugin (e.g. `"task"`, `"task.tracked"`,
    /// or `"task.*"`).
    raw: String,
    /// Pattern prefix excluding any trailing `*` (e.g. `"task."` for the glob
    /// `"task.*"`, or the full string for exact matches).
    prefix: String,
    /// Whether the pattern is a glob (`true`) or an exact match (`false`).
    is_glob: bool,
}

impl KindPattern {
    fn parse(raw: &str) -> Self {
        if let Some(stem) = raw.strip_suffix(".*") {
            KindPattern { raw: raw.to_string(), prefix: format!("{stem}."), is_glob: true }
        } else {
            KindPattern { raw: raw.to_string(), prefix: raw.to_string(), is_glob: false }
        }
    }

    fn matches(&self, kind: &str) -> bool {
        if self.is_glob {
            kind.starts_with(&self.prefix) && kind.len() > self.prefix.len()
        } else {
            self.prefix == kind
        }
    }
}

pub struct SubjectRouter {
    /// Exact-kind registrations keyed by the declared kind string.
    exact_kinds: HashMap<String, String>,
    /// Glob registrations stored as (pattern, plugin_name) pairs.
    glob_kinds: Vec<(KindPattern, String)>,
    hosts: HashMap<String, PluginHost>,
    /// Daemon-side translator state: maps user-facing `installed_kind` to
    /// the plugin's hardcoded `native_kind`. Outbound `route_call` rewrites
    /// `<installed_kind>/<verb>` to `<native_kind>/<verb>` before forwarding
    /// to the plugin's stdio; inbound responses have their top-level `kind`
    /// field rewritten from native back to installed.
    ///
    /// Empty when no installed plugin was renamed at install time —
    /// translation is a no-op and the router behaves identically to its
    /// pre-v0.5.7 form.
    aliases: KindAliasMap,
}

/// Per-plugin install-time rename map. Each entry pairs the user-facing
/// `installed_kind` (the prefix the SubjectRouter dispatches against) with
/// the plugin's hardcoded `native_kind` (the prefix the plugin actually
/// implements on the wire).
///
/// Built by the install pipeline from the v0.5.7 `plugins.lock` schema;
/// passed into [`SubjectRouter::from_initialized_hosts_with_aliases`] so
/// the router can register the installed_kind variant and translate at
/// the wire boundary.
#[derive(Debug, Clone, Default)]
pub struct KindAliasMap {
    /// Map from `installed_kind` -> `native_kind`. Only populated for
    /// plugins where the two values differ; identity mappings are
    /// represented by absence.
    installed_to_native: HashMap<String, String>,
    /// Map from `native_kind` -> `installed_kind`, scoped per plugin name
    /// so two plugins claiming the same native kind (each with its own
    /// installed_kind) can both round-trip inbound responses correctly.
    /// Lookups join on the plugin name produced at routing time.
    by_plugin: HashMap<String, HashMap<String, String>>,
}

impl KindAliasMap {
    /// Register an `(installed_kind, native_kind)` pair for `plugin_name`.
    /// Identity pairs (installed == native) are dropped: the translator
    /// only needs to track real renames.
    pub fn insert(&mut self, plugin_name: &str, installed_kind: &str, native_kind: &str) {
        if installed_kind == native_kind {
            return;
        }
        self.installed_to_native.insert(installed_kind.to_string(), native_kind.to_string());
        self.by_plugin
            .entry(plugin_name.to_string())
            .or_default()
            .insert(native_kind.to_string(), installed_kind.to_string());
    }

    /// Resolve the plugin-native kind that `installed_kind` maps to, if any.
    pub fn native_for_installed(&self, installed_kind: &str) -> Option<&str> {
        self.installed_to_native.get(installed_kind).map(String::as_str)
    }

    /// Resolve the user-facing kind a plugin's native kind should be
    /// rewritten to before returning a response, if any.
    pub fn installed_for_plugin_native(&self, plugin_name: &str, native_kind: &str) -> Option<&str> {
        self.by_plugin.get(plugin_name).and_then(|m| m.get(native_kind)).map(String::as_str)
    }

    /// `true` when no renames are registered. Lets the router short-circuit
    /// the inbound walker for the common case where every install uses its
    /// native kind.
    pub fn is_empty(&self) -> bool {
        self.installed_to_native.is_empty()
    }
}

impl SubjectRouter {
    pub async fn from_initialized_hosts(hosts: HashMap<String, PluginHost>) -> Result<Self> {
        Self::from_initialized_hosts_with_aliases(hosts, KindAliasMap::default()).await
    }

    /// Build the router and apply install-time kind renames. When `aliases`
    /// contains a `(plugin_name, native_kind) -> installed_kind` entry, the
    /// router registers the `installed_kind` against that plugin instead of
    /// the manifest-declared `native_kind`. This is the load-bearing piece
    /// of the v0.5.7 daemon-side translator: plugins keep emitting
    /// `task/list`, the router exposes it as `archive/list`, and outbound /
    /// inbound translation in [`Self::route_call`] keeps the wire boundary
    /// consistent.
    pub async fn from_initialized_hosts_with_aliases(
        hosts: HashMap<String, PluginHost>,
        aliases: KindAliasMap,
    ) -> Result<Self> {
        match Self::register_kinds(&hosts, &aliases).await {
            Ok((exact_kinds, glob_kinds)) => Ok(Self { exact_kinds, glob_kinds, hosts, aliases }),
            Err(error) => {
                // We own the spawned hosts; dropping them without shutdown
                // would orphan every already-live plugin child the moment
                // one plugin fails its handshake or claims a duplicate kind.
                for (_, host) in hosts {
                    let _ = host.shutdown().await;
                }
                Err(error)
            }
        }
    }

    async fn register_kinds(
        hosts: &HashMap<String, PluginHost>,
        aliases: &KindAliasMap,
    ) -> Result<(HashMap<String, String>, Vec<(KindPattern, String)>)> {
        let mut exact_kinds: HashMap<String, String> = HashMap::new();
        let mut glob_kinds: Vec<(KindPattern, String)> = Vec::new();
        let names = hosts.keys().cloned().collect::<Vec<_>>();

        for name in names {
            let host = hosts.get(&name).ok_or_else(|| anyhow!("plugin host disappeared during routing setup"))?;
            let result = host.handshake().await?;
            for raw_kind in result.capabilities.subject_kinds {
                let pattern = KindPattern::parse(&raw_kind);
                // Apply install-time rename: register the installed_kind
                // instead of the native one for this plugin if an alias
                // was recorded at install time. Glob patterns are
                // currently passed through unrenamed — the v0.5.7
                // translator only covers exact kinds, matching the
                // mission's scope of `task -> task-2` style renames.
                let (registered_pattern, registered_raw) = if pattern.is_glob {
                    (pattern, raw_kind.clone())
                } else if let Some(installed) = aliases.installed_for_plugin_native(&name, &pattern.raw) {
                    let renamed = KindPattern::parse(installed);
                    let renamed_raw = installed.to_string();
                    (renamed, renamed_raw)
                } else {
                    (pattern, raw_kind.clone())
                };

                if registered_pattern.is_glob {
                    if let Some((existing_pattern, existing_name)) =
                        glob_kinds.iter().find(|(p, _)| p.prefix == registered_pattern.prefix && p.is_glob)
                    {
                        return Err(anyhow!(
                            "duplicate subject kind glob '{}' claimed by '{}' and '{}'",
                            existing_pattern.raw,
                            existing_name,
                            name
                        ));
                    }
                    glob_kinds.push((registered_pattern, name.clone()));
                } else if let Some(existing) = exact_kinds.get(&registered_raw) {
                    return Err(anyhow!(
                        "duplicate subject kind '{}' claimed by '{}' and '{}'",
                        registered_raw,
                        existing,
                        name
                    ));
                } else {
                    exact_kinds.insert(registered_raw, name.clone());
                }
            }
        }

        Ok((exact_kinds, glob_kinds))
    }

    /// Resolve the plugin name responsible for `kind`.
    ///
    /// Precedence rules:
    ///
    /// 1. Exact-match registration (e.g. `task.tracked` beats `task.*`).
    /// 2. Longest matching glob prefix wins (`task.tracked.*` beats `task.*`
    ///    when resolving `task.tracked.foo`).
    /// 3. If two globs of equal prefix length both match, the resolution is
    ///    ambiguous and `None` is returned. (Equal-prefix duplicates are
    ///    already rejected at registration time, so this is defensive.)
    pub fn plugin_for_kind(&self, kind: &str) -> Option<&str> {
        if let Some(name) = self.exact_kinds.get(kind) {
            return Some(name.as_str());
        }
        let mut best: Option<(usize, &str)> = None;
        let mut ambiguous = false;
        for (pattern, plugin) in &self.glob_kinds {
            if !pattern.matches(kind) {
                continue;
            }
            let len = pattern.prefix.len();
            match best {
                None => best = Some((len, plugin.as_str())),
                Some((cur_len, _cur_plugin)) => {
                    if len > cur_len {
                        best = Some((len, plugin.as_str()));
                        ambiguous = false;
                    } else if len == cur_len {
                        ambiguous = true;
                    }
                }
            }
        }
        if ambiguous {
            None
        } else {
            best.map(|(_, plugin)| plugin)
        }
    }

    pub fn is_subject_method(&self, method: &str) -> bool {
        method.split('/').next().is_some_and(|kind| self.plugin_for_kind(kind).is_some())
    }

    pub async fn route_call(&self, method: &str, params: Option<Value>) -> Result<Value, RpcError> {
        let installed_kind = method.split('/').next().unwrap_or_default();
        let Some(plugin_name) = self.plugin_for_kind(installed_kind) else {
            return Err(RpcError {
                code: animus_plugin_protocol::error_codes::METHOD_NOT_FOUND,
                message: format!("no subject backend registered for kind '{installed_kind}'"),
                data: None,
            });
        };
        let plugin_name = plugin_name.to_string();
        let Some(host) = self.hosts.get(&plugin_name) else {
            return Err(RpcError {
                code: animus_plugin_protocol::error_codes::INTERNAL_ERROR,
                message: format!("subject backend '{plugin_name}' is not available"),
                data: None,
            });
        };

        let native_kind_opt = self.aliases.native_for_installed(installed_kind);

        // Outbound translation:
        //  - rewrite `<installed_kind>/<verb>` to `<native_kind>/<verb>` so
        //    the plugin sees the prefix it actually implements.
        //  - rewrite any top-level `id` / `subject_id` field in `params`
        //    whose `<kind>:` prefix matches the installed_kind so the
        //    plugin's local store can resolve native IDs (subject IDs are
        //    encoded `<kind>:<local-id>` per
        //    `extract_kind_from_subject_id` in control/dispatch.rs).
        let translated_method = match native_kind_opt {
            Some(native_kind) => match method.split_once('/') {
                Some((_, rest)) => format!("{native_kind}/{rest}"),
                None => native_kind.to_string(),
            },
            None => method.to_string(),
        };
        let translated_params = match (native_kind_opt, params) {
            (Some(native_kind), Some(value)) => Some(rewrite_outbound_id_prefix(value, installed_kind, native_kind)),
            (_, other) => other,
        };

        let mut response =
            host.request_with_timeout(&translated_method, translated_params, SUBJECT_ROUTE_TIMEOUT).await?;

        // Inbound translation: rewrite the top-level `kind` field AND the
        // `<kind>:` prefix in `id` fields so callers continue to see the
        // installed_kind they sent. Walker scope is intentionally narrow —
        // only Subject.kind/.id, SubjectList.subjects[*].kind/.id, and
        // SubjectEvent.subject.kind/.id — to avoid taking on full schema
        // knowledge inside the host crate. Deep-nested `kind` fields
        // (inside `metadata`, `tags`, etc.) are out of scope for v0.5.7.
        // See `docs/architecture/plugin-kind-translator-v0.5.7.md`.
        if !self.aliases.is_empty() {
            rewrite_response_kind(&mut response, &plugin_name, &self.aliases);
        }

        Ok(response)
    }

    pub async fn resolve_subject(&self, subject_kind: &str, subject_id: &str) -> Result<Value, RpcError> {
        self.route_call(&format!("{subject_kind}/get"), Some(serde_json::json!({ "id": subject_id }))).await
    }
}

/// Rewrite the top-level `kind` field on known response shapes from the
/// plugin's `native_kind` back to the user-facing `installed_kind`.
///
/// Supported shapes (matches the v0.5 SubjectRouter response surface):
///
/// - `Subject` — `{ "kind": "<native>", ... }` (top-level object).
/// - `SubjectList` — `{ "subjects": [{ "kind": "<native>", ... }, ...] }`.
/// - `SubjectEvent` — `{ "subject": { "kind": "<native>", ... }, ... }`.
///
/// The walker is intentionally narrow: it only inspects the top-level
/// object plus the two named collections above. Deep-nested `kind` fields
/// (inside `metadata`, `tags`, freeform plugin-defined payloads) are left
/// alone — rewriting them would require host-side schema knowledge that
/// belongs in the protocol crate, not the router. See
/// `docs/architecture/plugin-kind-translator-v0.5.7.md` for the explicit
/// deferral.
fn rewrite_response_kind(value: &mut Value, plugin_name: &str, aliases: &KindAliasMap) {
    let Value::Object(map) = value else {
        return;
    };
    rewrite_kind_in_object(map, plugin_name, aliases);
    if let Some(Value::Object(subject)) = map.get_mut("subject") {
        rewrite_kind_in_object(subject, plugin_name, aliases);
    }
    if let Some(Value::Array(subjects)) = map.get_mut("subjects") {
        for entry in subjects {
            if let Value::Object(item) = entry {
                rewrite_kind_in_object(item, plugin_name, aliases);
            }
        }
    }
}

fn rewrite_kind_in_object(object: &mut serde_json::Map<String, Value>, plugin_name: &str, aliases: &KindAliasMap) {
    if let Some(Value::String(kind)) = object.get("kind") {
        if let Some(installed) = aliases.installed_for_plugin_native(plugin_name, kind) {
            object.insert("kind".to_string(), Value::String(installed.to_string()));
        }
    }
    for id_field in ["id", "subject_id"] {
        let Some(Value::String(id)) = object.get(id_field) else {
            continue;
        };
        let Some((native_prefix, rest)) = id.split_once(':') else {
            continue;
        };
        let Some(installed) = aliases.installed_for_plugin_native(plugin_name, native_prefix) else {
            continue;
        };
        let rewritten = format!("{installed}:{rest}");
        object.insert(id_field.to_string(), Value::String(rewritten));
    }
}

/// Translate outbound params before forwarding to the plugin's stdio.
/// Rewrites the following fields when their value matches `installed_kind`:
///
/// - top-level `kind` (string or array of strings) — used by
///   `subject/create` payloads and the CLI's `subject list` shape.
/// - top-level `id` / `subject_id` — `<installed_kind>:<local-id>` is
///   rewritten to `<native_kind>:<local-id>`.
/// - top-level `filter.kind` (array of strings) — used by the daemon's
///   `subject/list` dispatch.
/// - nested `subject.kind` + `subject.id` — used by event-shaped params.
///
/// Recurses into the top-level object only — same narrow scope as
/// [`rewrite_response_kind`] — and is a no-op when the supplied JSON
/// isn't an object.
fn rewrite_outbound_id_prefix(mut value: Value, installed_kind: &str, native_kind: &str) -> Value {
    if let Value::Object(map) = &mut value {
        rewrite_outbound_in_object(map, installed_kind, native_kind);
        if let Some(Value::Object(subject)) = map.get_mut("subject") {
            rewrite_outbound_in_object(subject, installed_kind, native_kind);
        }
        if let Some(Value::Object(filter)) = map.get_mut("filter") {
            rewrite_outbound_kind_field(filter, installed_kind, native_kind);
        }
    }
    value
}

fn rewrite_outbound_in_object(object: &mut serde_json::Map<String, Value>, installed_kind: &str, native_kind: &str) {
    rewrite_outbound_kind_field(object, installed_kind, native_kind);
    for id_field in ["id", "subject_id"] {
        let Some(Value::String(id)) = object.get(id_field) else {
            continue;
        };
        let Some((prefix, rest)) = id.split_once(':') else {
            continue;
        };
        if prefix != installed_kind {
            continue;
        }
        let rewritten = format!("{native_kind}:{rest}");
        object.insert(id_field.to_string(), Value::String(rewritten));
    }
}

/// Rewrite the `kind` field on a JSON object when its value matches
/// `installed_kind`. Supports both string (`"kind": "archive"`) and
/// array-of-strings (`"kind": ["archive", "other"]`) shapes — the CLI
/// emits the array form for `subject/list` and the daemon control
/// dispatch reads `filter.kind` as an array.
fn rewrite_outbound_kind_field(object: &mut serde_json::Map<String, Value>, installed_kind: &str, native_kind: &str) {
    match object.get("kind") {
        Some(Value::String(kind)) if kind == installed_kind => {
            object.insert("kind".to_string(), Value::String(native_kind.to_string()));
        }
        Some(Value::Array(items)) => {
            let rewritten: Vec<Value> = items
                .iter()
                .map(|item| match item {
                    Value::String(k) if k == installed_kind => Value::String(native_kind.to_string()),
                    other => other.clone(),
                })
                .collect();
            object.insert("kind".to_string(), Value::Array(rewritten));
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use animus_plugin_protocol::{InitializeResult, PluginCapabilities, PluginInfo, RpcRequest, RpcResponse};
    use tokio::io::{duplex, AsyncBufReadExt, AsyncWriteExt, BufReader};

    use super::*;

    async fn subject_host(name: &str, subject_kinds: Vec<&str>) -> PluginHost {
        let (host_reader, mut plugin_writer) = duplex(8192);
        let (plugin_reader, host_writer) = duplex(8192);
        let name_for_task = name.to_string();
        let kinds = subject_kinds.into_iter().map(ToOwned::to_owned).collect::<Vec<_>>();

        tokio::spawn(async move {
            let mut reader = BufReader::new(plugin_reader);
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).await.expect("read line") == 0 {
                    break;
                }
                let request: RpcRequest = serde_json::from_str(line.trim()).expect("parse request");
                let response = match request.method.as_str() {
                    "initialize" => RpcResponse::ok(
                        request.id,
                        serde_json::json!(InitializeResult {
                            protocol_version: "1.0.0".to_string(),
                            plugin_info: PluginInfo {
                                name: name_for_task.clone(),
                                version: "0.1.0".to_string(),
                                plugin_kind: "subject_backend".to_string(),
                                description: None,
                            },
                            capabilities: PluginCapabilities {
                                subject_kinds: kinds.clone(),
                                methods: kinds.iter().map(|kind| format!("{kind}/get")).collect(),
                                ..PluginCapabilities::default()
                            },
                        }),
                    ),
                    "initialized" => continue,
                    method => RpcResponse::ok(request.id, serde_json::json!({ "method": method })),
                };
                let mut encoded = serde_json::to_string(&response).expect("encode response");
                encoded.push('\n');
                plugin_writer.write_all(encoded.as_bytes()).await.expect("write response");
            }
        });

        PluginHost::from_streams(name, host_reader, host_writer)
    }

    #[tokio::test]
    async fn routes_by_subject_kind_prefix() {
        let mut hosts = HashMap::new();
        hosts.insert("tasks".to_string(), subject_host("tasks", vec!["task"]).await);
        let router = SubjectRouter::from_initialized_hosts(hosts).await.expect("router");

        let result = router.route_call("task/get", Some(serde_json::json!({ "id": "TASK-1" }))).await.expect("route");

        assert_eq!(result["method"], "task/get");
        assert_eq!(router.plugin_for_kind("task"), Some("tasks"));
    }

    #[tokio::test]
    async fn glob_kind_matches_dotted_subkinds() {
        let mut hosts = HashMap::new();
        hosts.insert("all-tasks".to_string(), subject_host("all-tasks", vec!["task.*"]).await);
        let router = SubjectRouter::from_initialized_hosts(hosts).await.expect("router");

        // Glob matches both kinds.
        assert_eq!(router.plugin_for_kind("task.tracked"), Some("all-tasks"));
        assert_eq!(router.plugin_for_kind("task.untracked"), Some("all-tasks"));
        // The glob does not match the bare prefix itself.
        assert_eq!(router.plugin_for_kind("task"), None);
        // And the route_call path also accepts the dotted method.
        let result = router.route_call("task.tracked/list", Some(serde_json::json!({}))).await.expect("route");
        assert_eq!(result["method"], "task.tracked/list");
    }

    #[tokio::test]
    async fn exact_match_beats_glob() {
        let mut hosts = HashMap::new();
        hosts.insert("any-task".to_string(), subject_host("any-task", vec!["task.*"]).await);
        hosts.insert("tracked".to_string(), subject_host("tracked", vec!["task.tracked"]).await);
        let router = SubjectRouter::from_initialized_hosts(hosts).await.expect("router");

        assert_eq!(router.plugin_for_kind("task.tracked"), Some("tracked"));
        assert_eq!(router.plugin_for_kind("task.untracked"), Some("any-task"));
    }

    #[tokio::test]
    async fn longest_glob_prefix_wins() {
        let mut hosts = HashMap::new();
        hosts.insert("any-task".to_string(), subject_host("any-task", vec!["task.*"]).await);
        hosts.insert("nested".to_string(), subject_host("nested", vec!["task.tracked.*"]).await);
        let router = SubjectRouter::from_initialized_hosts(hosts).await.expect("router");

        assert_eq!(router.plugin_for_kind("task.tracked.high"), Some("nested"));
        assert_eq!(router.plugin_for_kind("task.untracked.low"), Some("any-task"));
    }

    /// Spawns a fake subject backend that round-trips the inbound method
    /// and params back to the caller. Used by translator tests so the test
    /// can assert what the plugin actually saw (post outbound rewrite).
    async fn echo_subject_host(name: &str, subject_kinds: Vec<&str>) -> PluginHost {
        let (host_reader, mut plugin_writer) = duplex(8192);
        let (plugin_reader, host_writer) = duplex(8192);
        let name_for_task = name.to_string();
        let kinds = subject_kinds.into_iter().map(ToOwned::to_owned).collect::<Vec<_>>();

        tokio::spawn(async move {
            let mut reader = BufReader::new(plugin_reader);
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).await.expect("read line") == 0 {
                    break;
                }
                let request: RpcRequest = serde_json::from_str(line.trim()).expect("parse request");
                let response = match request.method.as_str() {
                    "initialize" => RpcResponse::ok(
                        request.id,
                        serde_json::json!(InitializeResult {
                            protocol_version: "1.0.0".to_string(),
                            plugin_info: PluginInfo {
                                name: name_for_task.clone(),
                                version: "0.1.0".to_string(),
                                plugin_kind: "subject_backend".to_string(),
                                description: None,
                            },
                            capabilities: PluginCapabilities {
                                subject_kinds: kinds.clone(),
                                methods: kinds.iter().map(|kind| format!("{kind}/get")).collect(),
                                ..PluginCapabilities::default()
                            },
                        }),
                    ),
                    "initialized" => continue,
                    method => {
                        // Echo what the plugin saw, plus a `subject` payload
                        // whose `kind` matches the native prefix it received.
                        // Inbound translation should rewrite that `kind` back
                        // to the installed_kind before returning to the caller.
                        // IDs are emitted in the canonical `<kind>:<local-id>`
                        // shape so tests can assert outbound + inbound ID
                        // translation alongside the `kind` field rewrite.
                        let prefix = method.split('/').next().unwrap_or_default().to_string();
                        let saw_params = request.params.clone().unwrap_or(serde_json::Value::Null);
                        RpcResponse::ok(
                            request.id,
                            serde_json::json!({
                                "plugin_saw_method": method,
                                "plugin_saw_params": saw_params,
                                "kind": prefix.clone(),
                                "subject": {
                                    "kind": prefix.clone(),
                                    "id": format!("{prefix}:LOCAL-1"),
                                },
                                "subjects": [
                                    { "kind": prefix.clone(), "id": format!("{prefix}:LOCAL-A") },
                                    {
                                        "kind": prefix.clone(),
                                        "id": format!("{prefix}:LOCAL-B"),
                                        "metadata": { "kind": "untouched" },
                                    }
                                ]
                            }),
                        )
                    }
                };
                let mut encoded = serde_json::to_string(&response).expect("encode response");
                encoded.push('\n');
                plugin_writer.write_all(encoded.as_bytes()).await.expect("write response");
            }
        });

        PluginHost::from_streams(name, host_reader, host_writer)
    }

    #[tokio::test]
    async fn outbound_method_rewrites_installed_kind_to_native() {
        let mut hosts = HashMap::new();
        hosts.insert("archive".to_string(), echo_subject_host("archive", vec!["task"]).await);
        let mut aliases = KindAliasMap::default();
        aliases.insert("archive", "archive", "task");
        let router = SubjectRouter::from_initialized_hosts_with_aliases(hosts, aliases).await.expect("router builds");

        assert_eq!(router.plugin_for_kind("archive"), Some("archive"));
        assert_eq!(router.plugin_for_kind("task"), None, "native kind must NOT be routable after rename");

        let result = router.route_call("archive/list", None).await.expect("route call");
        assert_eq!(result["plugin_saw_method"], "task/list", "plugin must receive native-kind method");
    }

    #[tokio::test]
    async fn outbound_method_is_unchanged_when_alias_is_identity() {
        let mut hosts = HashMap::new();
        hosts.insert("default".to_string(), echo_subject_host("default", vec!["task"]).await);
        let router = SubjectRouter::from_initialized_hosts_with_aliases(hosts, KindAliasMap::default())
            .await
            .expect("router builds");

        let result = router.route_call("task/list", None).await.expect("route call");
        assert_eq!(result["plugin_saw_method"], "task/list");
    }

    #[tokio::test]
    async fn inbound_response_rewrites_top_level_subject_and_subjects_kind() {
        let mut hosts = HashMap::new();
        hosts.insert("archive".to_string(), echo_subject_host("archive", vec!["task"]).await);
        let mut aliases = KindAliasMap::default();
        aliases.insert("archive", "archive", "task");
        let router = SubjectRouter::from_initialized_hosts_with_aliases(hosts, aliases).await.expect("router builds");

        let result = router.route_call("archive/list", None).await.expect("route call");
        assert_eq!(result["kind"], "archive", "top-level kind rewritten to installed");
        assert_eq!(result["subject"]["kind"], "archive", "Subject.kind rewritten");
        assert_eq!(result["subjects"][0]["kind"], "archive", "SubjectList.subjects[0].kind rewritten");
        assert_eq!(result["subjects"][1]["kind"], "archive", "SubjectList.subjects[1].kind rewritten");
        // IDs must travel through the translator alongside the `kind`
        // field so subsequent control-plane round-trips that extract the
        // kind from `<kind>:<local-id>` land back on the same plugin.
        assert_eq!(result["subject"]["id"], "archive:LOCAL-1");
        assert_eq!(result["subjects"][0]["id"], "archive:LOCAL-A");
        assert_eq!(result["subjects"][1]["id"], "archive:LOCAL-B");
        // Deep nesting under `metadata` is explicitly out of scope.
        assert_eq!(
            result["subjects"][1]["metadata"]["kind"], "untouched",
            "deep-nested kind fields must be left alone in v0.5.7"
        );
    }

    #[tokio::test]
    async fn outbound_params_rewrite_id_prefix_to_native_kind() {
        let mut hosts = HashMap::new();
        hosts.insert("archive".to_string(), echo_subject_host("archive", vec!["task"]).await);
        let mut aliases = KindAliasMap::default();
        aliases.insert("archive", "archive", "task");
        let router = SubjectRouter::from_initialized_hosts_with_aliases(hosts, aliases).await.expect("router builds");

        let params = serde_json::json!({ "id": "archive:LOCAL-X" });
        let result = router.route_call("archive/get", Some(params)).await.expect("route call");
        assert_eq!(
            result["plugin_saw_params"]["id"], "task:LOCAL-X",
            "outbound id prefix must be translated to native_kind before forwarding"
        );
    }

    #[tokio::test]
    async fn outbound_params_rewrite_top_level_kind_string() {
        let mut hosts = HashMap::new();
        hosts.insert("archive".to_string(), echo_subject_host("archive", vec!["task"]).await);
        let mut aliases = KindAliasMap::default();
        aliases.insert("archive", "archive", "task");
        let router = SubjectRouter::from_initialized_hosts_with_aliases(hosts, aliases).await.expect("router builds");

        let params = serde_json::json!({ "kind": "archive", "title": "demo" });
        let result = router.route_call("archive/create", Some(params)).await.expect("route call");
        assert_eq!(
            result["plugin_saw_params"]["kind"], "task",
            "create's top-level kind must be translated to the native kind"
        );
    }

    #[tokio::test]
    async fn outbound_params_rewrite_filter_kind_array() {
        let mut hosts = HashMap::new();
        hosts.insert("archive".to_string(), echo_subject_host("archive", vec!["task"]).await);
        let mut aliases = KindAliasMap::default();
        aliases.insert("archive", "archive", "task");
        let router = SubjectRouter::from_initialized_hosts_with_aliases(hosts, aliases).await.expect("router builds");

        let params = serde_json::json!({ "filter": { "kind": ["archive", "other"] } });
        let result = router.route_call("archive/list", Some(params)).await.expect("route call");
        let kinds = &result["plugin_saw_params"]["filter"]["kind"];
        assert_eq!(kinds[0], "task", "matching installed_kind in array must be translated");
        assert_eq!(kinds[1], "other", "unrelated kinds in array must be preserved");
    }

    #[tokio::test]
    async fn outbound_params_leave_unrelated_id_prefixes_alone() {
        let mut hosts = HashMap::new();
        hosts.insert("archive".to_string(), echo_subject_host("archive", vec!["task"]).await);
        let mut aliases = KindAliasMap::default();
        aliases.insert("archive", "archive", "task");
        let router = SubjectRouter::from_initialized_hosts_with_aliases(hosts, aliases).await.expect("router builds");

        let params = serde_json::json!({ "id": "other:UNTOUCHED" });
        let result = router.route_call("archive/get", Some(params)).await.expect("route call");
        assert_eq!(
            result["plugin_saw_params"]["id"], "other:UNTOUCHED",
            "non-matching id prefixes must be forwarded verbatim"
        );
    }

    #[tokio::test]
    async fn duplicate_glob_kinds_are_rejected_at_registration() {
        let mut hosts = HashMap::new();
        hosts.insert("a".to_string(), subject_host("a", vec!["task.*"]).await);
        hosts.insert("b".to_string(), subject_host("b", vec!["task.*"]).await);

        let outcome = SubjectRouter::from_initialized_hosts(hosts).await;
        let err = match outcome {
            Err(e) => e,
            Ok(_) => panic!("router should reject duplicate glob kinds"),
        };
        assert!(format!("{err:?}").contains("duplicate subject kind glob"));
    }
}
