//! Backend selection for chat conversation persistence.
//!
//! [`ConversationStoreClient`] is the seam that lets chat history be served
//! either by the in-tree filesystem store (the default — chat works with zero
//! plugins installed) or by an installed `conversation_store` plugin (e.g. a
//! Postgres backend with per-user ownership + sharing). This mirrors how
//! `config_source` and `subject_backend` choose a backend at runtime:
//!
//! * if a [`PLUGIN_KIND_CONVERSATION_STORE`] plugin is discovered, every data
//!   op routes to it over JSON-RPC. Each call spawns a plugin host, runs one
//!   RPC, and ALWAYS reaps the host with `host.shutdown().await` — never
//!   leaking a host per call (the v0.6.3 leak lesson);
//! * else the call falls through to [`FileConversationStore`].
//!
//! The `conversation_store` role is **optional**: it is NOT a required
//! preflight role, and the daemon never refuses to start without it.
//!
//! [`PLUGIN_KIND_CONVERSATION_STORE`]: animus_plugin_protocol::PLUGIN_KIND_CONVERSATION_STORE

use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use animus_actor::Actor;
use animus_plugin_protocol::conversation_store as proto;
use anyhow::{anyhow, Context, Result};
use orchestrator_core::{
    ChatOperationBegin, ChatOperationClaim, ChatOperationReceipt, ChatOperationRequest, ChatOperationStatus,
};
use orchestrator_plugin_host::{discover_by_kind, DiscoveredPlugin, PluginHost, PluginSpawnOptions};

use super::store::{
    ChatMessage, ConversationLock, ConversationMeta, ConversationStore, ConversationSummary, FileConversationStore,
};

const CONVERSATION_STORE_KIND: &str = "conversation_store";
const RPC_TIMEOUT: Duration = Duration::from_secs(30);
const BACKEND_PROBE_TIMEOUT: Duration = Duration::from_secs(5);
pub(crate) const SHARED_OPERATION_CAPABILITY: &str = proto::CAPABILITY_CONVERSATION_OPERATIONS_SHARED_V1;
pub(crate) const FENCED_APPEND_CAPABILITY: &str = proto::CAPABILITY_CONVERSATION_OPERATION_FENCED_APPEND_V1;
const REQUIRED_SHARED_OPERATION_METHODS: [&str; 7] = [
    proto::METHOD_CONVERSATION_OPERATION_BEGIN,
    proto::METHOD_CONVERSATION_OPERATION_LOAD,
    proto::METHOD_CONVERSATION_OPERATION_RENEW,
    proto::METHOD_CONVERSATION_OPERATION_BIND_EXECUTION,
    proto::METHOD_CONVERSATION_OPERATION_RELEASE,
    proto::METHOD_CONVERSATION_OPERATION_ACCEPT_USER,
    proto::METHOD_CONVERSATION_OPERATION_TERMINALIZE,
];

#[derive(Debug, serde::Serialize, PartialEq, Eq)]
pub(crate) struct ChatBackendReadiness {
    pub schema: &'static str,
    pub kind: &'static str,
    pub authority_mode: &'static str,
    pub required_capability: &'static str,
    pub required_capability_observed: bool,
    pub ready: bool,
    pub error_code: Option<&'static str>,
}

pub(crate) fn backend_readiness(project_root: &Path) -> ChatBackendReadiness {
    let discovered = match discover_by_kind(project_root.to_path_buf(), CONVERSATION_STORE_KIND) {
        Err(_) => BackendDiscovery::Failed,
        Ok(plugins) => match plugins.into_iter().next() {
            None => BackendDiscovery::File,
            Some(plugin) => {
                let shared = plugin_supports_shared_authority(&plugin);
                let probe_succeeded = shared && probe_plugin_backend(&plugin, project_root).is_ok();
                BackendDiscovery::Plugin { shared, probe_succeeded }
            }
        },
    };
    readiness_for(discovered)
}

#[derive(Debug, Clone, Copy)]
enum BackendDiscovery {
    Failed,
    File,
    Plugin { shared: bool, probe_succeeded: bool },
}

fn readiness_for(discovered: BackendDiscovery) -> ChatBackendReadiness {
    match discovered {
        BackendDiscovery::Failed => ChatBackendReadiness {
            schema: "animus.chat.backend_readiness.v1",
            kind: "unavailable",
            authority_mode: "unavailable",
            required_capability: SHARED_OPERATION_CAPABILITY,
            required_capability_observed: false,
            ready: false,
            error_code: Some("conversation_store_discovery_failed"),
        },
        BackendDiscovery::File => ChatBackendReadiness {
            schema: "animus.chat.backend_readiness.v1",
            kind: "file",
            authority_mode: "local_sqlite",
            required_capability: SHARED_OPERATION_CAPABILITY,
            required_capability_observed: false,
            ready: true,
            error_code: None,
        },
        BackendDiscovery::Plugin { shared: true, probe_succeeded: true } => ChatBackendReadiness {
            schema: "animus.chat.backend_readiness.v1",
            kind: "plugin",
            authority_mode: "shared_conversation_store_rpc",
            required_capability: SHARED_OPERATION_CAPABILITY,
            required_capability_observed: true,
            ready: true,
            error_code: None,
        },
        BackendDiscovery::Plugin { shared: true, probe_succeeded: false } => ChatBackendReadiness {
            schema: "animus.chat.backend_readiness.v1",
            kind: "plugin",
            authority_mode: "unavailable",
            required_capability: SHARED_OPERATION_CAPABILITY,
            required_capability_observed: true,
            ready: false,
            error_code: Some("conversation_store_probe_failed"),
        },
        BackendDiscovery::Plugin { shared: false, .. } => ChatBackendReadiness {
            schema: "animus.chat.backend_readiness.v1",
            kind: "plugin",
            authority_mode: "unavailable",
            required_capability: SHARED_OPERATION_CAPABILITY,
            required_capability_observed: false,
            ready: false,
            error_code: Some("shared_operation_authority_missing"),
        },
    }
}

/// Perform bounded, read-only RPCs through the selected plugin. Static
/// capability declarations alone cannot prove that the process starts, that
/// its authoritative database is reachable, or that initialize advertises the
/// complete shared-operation surface. Both reads use guaranteed-nonexistent
/// keys and never create production data.
fn probe_plugin_backend(plugin: &DiscoveredPlugin, project_root: &Path) -> Result<()> {
    let plugin = plugin.clone();
    let project_root = project_root.to_path_buf();
    run_blocking(async move {
        let options = PluginSpawnOptions::for_manifest(
            plugin.name.clone(),
            &plugin.manifest.env_required,
            std::iter::empty::<String>(),
            None,
        )
        .with_working_dir(project_root.clone());
        let host = PluginHost::spawn_with_options(&plugin.path, &[], options)
            .await
            .with_context(|| format!("spawning conversation_store plugin {} for readiness probe", plugin.name))?;
        let result = async {
            let actor = Actor {
                user_id: "__animus_readiness_probe__".to_string(),
                claims: Vec::new(),
                tenant_id: Some("__animus_readiness_probe__".to_string()),
            };
            let conversation_id = "__animus_readiness_probe_missing__";
            let request = BoundConversationIdRequest {
                scope: bound_scope(&project_root, &actor),
                id: conversation_id.to_string(),
                as_user: actor.user_id.clone(),
            };
            let params = bind_actor_to_params(&actor, &request)?;
            tokio::time::timeout(BACKEND_PROBE_TIMEOUT, async {
                let initialized = host.handshake().await?;
                require_complete_operation_surface(&initialized.capabilities.methods)?;
                let value = host
                    .request_typed_with_timeout(
                        proto::METHOD_CONVERSATION_LOAD_META,
                        Some(params),
                        BACKEND_PROBE_TIMEOUT,
                    )
                    .await?;
                let _: BoundConversationMetaResponse = serde_json::from_value(value)?;

                let scope = bound_scope(&project_root, &actor);
                let operation_read = proto::ConversationOperationLoadRequest {
                    key: proto::ConversationOperationKey {
                        scope: proto::ConversationScope {
                            tenant_id: Some(scope.tenant_id),
                            project_root: scope.project_root,
                            repo_scope: scope.repo_scope,
                        },
                        conversation_id: conversation_id.to_string(),
                        caller_key: "__animus_readiness_probe__".to_string(),
                        as_user: Some(actor.user_id.clone()),
                    },
                };
                let operation_params = bind_actor_to_params(&actor, &operation_read)?;
                match host
                    .request_typed_with_timeout(METHOD_OPERATION_LOAD, Some(operation_params), BACKEND_PROBE_TIMEOUT)
                    .await
                {
                    Ok(value) => {
                        let _: proto::ConversationOperationLoadResponse = serde_json::from_value(value)?;
                    }
                    // A backend may authorize the conversation before loading
                    // its operation row. The deliberately missing conversation
                    // can therefore return an application-level not-found. It
                    // still proves the operation route was dispatched; a
                    // method-not-found response does not contain our sentinel.
                    Err(error) if operation_probe_rejected_missing_key(&error, conversation_id) => {}
                    Err(error) => return Err(error).context("probing conversation/operation_load"),
                }
                Ok::<_, anyhow::Error>(())
            })
            .await
            .map_err(|_| anyhow!("conversation_store readiness probe timed out"))?
        }
        .await;
        let _ = host.shutdown().await;
        result
    })?
}

fn plugin_supports_shared_authority(plugin: &DiscoveredPlugin) -> bool {
    let capabilities = &plugin.manifest.capabilities;
    capabilities.iter().any(|value| value == SHARED_OPERATION_CAPABILITY)
        && capabilities.iter().any(|value| value == FENCED_APPEND_CAPABILITY)
        && REQUIRED_SHARED_OPERATION_METHODS.iter().all(|required| capabilities.iter().any(|value| value == required))
}

fn require_complete_operation_surface(methods: &[String]) -> Result<()> {
    let missing: Vec<&str> = REQUIRED_SHARED_OPERATION_METHODS
        .iter()
        .copied()
        .filter(|required| !methods.iter().any(|method| method == required))
        .collect();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(anyhow!("conversation_store initialize omitted required shared operation methods: {}", missing.join(", ")))
    }
}

fn operation_probe_rejected_missing_key(error: &impl std::fmt::Display, conversation_id: &str) -> bool {
    let message = error.to_string().to_ascii_lowercase();
    message.contains(&conversation_id.to_ascii_lowercase())
        && (message.contains("not found") || message.contains("not writable"))
        && !message.contains("method not found")
        && !message.contains("-32601")
}

fn require_remote_shared_authority(observed: bool) -> Result<()> {
    if observed {
        Ok(())
    } else {
        Err(crate::unavailable_error(
            "chat_backend_not_ready: plugin conversation_store lacks the complete shared-operation and fenced-append contract; keyed sends fail closed",
        ))
    }
}

/// Runtime-selected conversation store: the in-tree filesystem store, or a
/// discovered `conversation_store` plugin.
pub(crate) enum ConversationStoreClient {
    /// Default backend: the scoped-state filesystem store.
    File(FileConversationStore),
    /// An installed `conversation_store` plugin handles persistence. Boxed so
    /// the common `File` variant does not pay for the plugin client's larger
    /// footprint (a `DiscoveredPlugin` + `PathBuf`).
    Plugin(Box<PluginConversationStore>),
}

impl ConversationStoreClient {
    /// Resolve the conversation store for an explicitly local/system caller.
    /// A discovered plugin rejects this unscoped construction; authenticated
    /// plugin calls must use [`Self::for_project_as_actor`].
    pub(crate) fn for_project(project_root: &Path) -> Result<Self> {
        Self::for_project_as_actor(project_root, None, None)
    }

    /// Resolve a store for a transport-authenticated actor. `as_user` is only
    /// a compatibility assertion and may never establish caller authority.
    /// Plugin-backed stores require a non-empty actor user and tenant; local
    /// file storage remains available without an actor for explicit
    /// system/local use and is tenant-partitioned when a tenant is supplied.
    pub(crate) fn for_project_as_actor(
        project_root: &Path,
        actor: Option<Actor>,
        as_user: Option<&str>,
    ) -> Result<Self> {
        assert_actor_matches_as_user(actor.as_ref(), as_user)?;
        let plugins = discover_by_kind(project_root.to_path_buf(), CONVERSATION_STORE_KIND)
            .with_context(|| format!("discovering conversation_store plugins for {}", project_root.display()))?;
        match plugins.into_iter().next() {
            Some(plugin) => {
                let actor = require_plugin_actor(actor)?;
                let lock_store = file_store_for_actor(project_root, Some(&actor))?;
                Ok(Self::Plugin(Box::new(PluginConversationStore::new(
                    plugin,
                    project_root.to_path_buf(),
                    actor,
                    lock_store,
                ))))
            }
            None => Ok(Self::File(file_store_for_actor(project_root, actor.as_ref())?)),
        }
    }

    /// Search conversations for `query`, newest-first, up to `limit` matches.
    /// The File store reads in-process, so the generic per-conversation scan is
    /// cheap. The Plugin store spawns + reaps the host on EVERY rpc, so the
    /// scan's N+1 (`list` + `load_messages` per conversation) would spawn the
    /// plugin N+1 times — instead we run the whole scan over ONE spawned host
    /// (see [`PluginConversationStore::search_async`]).
    pub(crate) fn search(
        &self,
        query: &str,
        case_insensitive: bool,
        limit: usize,
        as_user: Option<&str>,
    ) -> Result<Vec<super::SearchMatch>> {
        match self {
            Self::File(_) => super::search_conversations(self, query, case_insensitive, limit, as_user),
            Self::Plugin(plugin) => {
                let plugin = (**plugin).clone();
                let query = query.to_string();
                let as_user = as_user.map(ToOwned::to_owned);
                run_blocking(
                    async move { plugin.search_async(&query, case_insensitive, limit, as_user.as_deref()).await },
                )?
            }
        }
    }

    /// Select durable keyed-send authority from the transcript backend.
    /// A remote transcript store may never silently use host-local SQLite.
    pub(crate) fn shared_operation_client(&self, require_shared: bool) -> Result<Option<SharedOperationClient>> {
        match self {
            Self::File(_) if require_shared => Err(crate::unavailable_error(
                "chat_backend_not_ready: --require-shared-authority forbids host-local SQLite operation authority",
            )),
            Self::File(_) => Ok(None),
            Self::Plugin(plugin) => {
                require_remote_shared_authority(plugin_supports_shared_authority(&plugin.plugin))?;
                Ok(Some(SharedOperationClient::new(
                    plugin.plugin.clone(),
                    plugin.project_root.clone(),
                    plugin.actor.clone(),
                )))
            }
        }
    }

    /// Build a `File`-backed client rooted at an explicit directory. Test-only
    /// escape hatch mirroring [`FileConversationStore::with_root_for_test`] so
    /// query-layer helpers that take a `ConversationStoreClient` can be
    /// exercised against a temp dir.
    #[cfg(test)]
    pub(crate) fn with_root_for_test(root: PathBuf) -> Self {
        Self::File(FileConversationStore::with_root_for_test(root))
    }
}

/// Validate the legacy `--as-user` assertion and return the authoritative user
/// from the transport actor. An assertion without an actor is impersonation,
/// not authentication, and therefore fails closed even for the local store.
pub(crate) fn assert_actor_matches_as_user<'a>(
    actor: Option<&'a Actor>,
    as_user: Option<&str>,
) -> Result<Option<&'a str>> {
    let Some(actor) = actor else {
        if as_user.is_some() {
            return Err(crate::invalid_input_error("--as-user requires --actor-json and may only assert its user_id"));
        }
        return Ok(None);
    };
    if actor.user_id.trim().is_empty() {
        return Err(crate::invalid_input_error("--actor-json user_id must be non-empty"));
    }
    if let Some(asserted) = as_user {
        if asserted != actor.user_id {
            return Err(crate::invalid_input_error("--as-user must equal actor_json.user_id"));
        }
    }
    Ok(Some(actor.user_id.as_str()))
}

fn require_plugin_actor(actor: Option<Actor>) -> Result<Actor> {
    let actor = actor.ok_or_else(|| {
        crate::invalid_input_error("plugin-backed chat requires --actor-json with user_id and tenant_id")
    })?;
    if actor.user_id.trim().is_empty() {
        return Err(crate::invalid_input_error("plugin-backed chat requires a non-empty actor user_id"));
    }
    if actor.tenant_id.as_deref().is_none_or(|value| value.trim().is_empty()) {
        return Err(crate::invalid_input_error("plugin-backed chat requires actor tenant_id"));
    }
    Ok(actor)
}

fn file_store_for_actor(project_root: &Path, actor: Option<&Actor>) -> Result<FileConversationStore> {
    match actor.and_then(|value| value.tenant_id.as_deref()).filter(|value| !value.trim().is_empty()) {
        Some(tenant_id) => FileConversationStore::for_project_tenant(project_root, tenant_id),
        None => FileConversationStore::for_project(project_root),
    }
}

fn bind_actor_to_params(actor: &Actor, request: &impl serde::Serialize) -> Result<serde_json::Value> {
    let mut params = serde_json::to_value(request).context("serializing conversation_store request")?;
    let object = params
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("conversation_store request must serialize as an object"))?;
    let tenant_id = actor
        .tenant_id
        .as_ref()
        .ok_or_else(|| crate::invalid_input_error("plugin-backed chat requires actor tenant_id"))?;
    object.insert("tenant_id".to_string(), serde_json::Value::String(tenant_id.clone()));
    object.insert("actor".to_string(), serde_json::to_value(actor).context("serializing conversation_store actor")?);
    Ok(params)
}

impl ConversationStore for ConversationStoreClient {
    fn try_lock_conversation(&self, id: &str) -> Result<Option<ConversationLock>> {
        match self {
            // The turn loop holds this guard across the whole read-meta →
            // append → run → save-meta critical section. Seq is assigned
            // client-side from `meta.message_count`, so even the plugin path
            // needs whole-turn serialization or two concurrent sends to one
            // conversation race the seq and last-writer-win the meta. We reuse
            // the host-local file lock for BOTH backends: it serializes turns
            // on this host (the common case). A multi-host DB-backed plugin
            // should additionally serialize via its own transaction/advisory
            // lock — which the contract enables (the data ops are individual
            // RPCs), but is the plugin's responsibility, not the kernel's.
            Self::File(store) => store.try_lock_conversation(id),
            Self::Plugin(plugin) => plugin.try_lock_conversation(id),
        }
    }

    fn create(&self, id: Option<String>) -> Result<ConversationMeta> {
        self.create_with_ownership(id, None, Default::default())
    }

    fn load_meta(&self, id: &str) -> Result<Option<ConversationMeta>> {
        match self {
            Self::File(store) => store.load_meta(id),
            Self::Plugin(plugin) => plugin.load_meta(id),
        }
    }

    fn save_meta(&self, meta: &ConversationMeta) -> Result<()> {
        match self {
            Self::File(store) => store.save_meta(meta),
            Self::Plugin(plugin) => plugin.save_meta(meta, None),
        }
    }

    fn save_meta_if_revision(&self, meta: &ConversationMeta, expected: Option<u64>) -> Result<()> {
        match self {
            Self::File(store) => store.save_meta_if_revision(meta, expected),
            Self::Plugin(plugin) => plugin.save_meta(meta, expected),
        }
    }

    fn append_message(&self, id: &str, message: &ChatMessage) -> Result<()> {
        match self {
            Self::File(store) => store.append_message(id, message),
            Self::Plugin(plugin) => plugin.append_message(id, message),
        }
    }

    fn append_assistant_message(
        &self,
        id: &str,
        message: &ChatMessage,
        operation_fence: Option<&proto::ConversationOperationAppendFence>,
    ) -> Result<()> {
        match self {
            Self::File(store) => store.append_message(id, message),
            Self::Plugin(plugin) => plugin.append_message_fenced(id, message, operation_fence),
        }
    }

    fn load_messages(&self, id: &str) -> Result<Vec<ChatMessage>> {
        match self {
            Self::File(store) => store.load_messages(id),
            Self::Plugin(plugin) => plugin.load_messages(id),
        }
    }

    fn list(&self) -> Result<Vec<ConversationSummary>> {
        self.list_for_user(None)
    }

    fn delete(&self, id: &str) -> Result<()> {
        match self {
            Self::File(store) => store.delete(id),
            Self::Plugin(plugin) => plugin.delete(id),
        }
    }
}

impl ConversationStoreClient {
    /// Create a conversation, stamping `owner` + `visibility` onto its meta.
    /// For the file store the fields are applied via a follow-up `save_meta`;
    /// for the plugin they ride the `conversation/create` request.
    pub(crate) fn create_with_ownership(
        &self,
        id: Option<String>,
        owner: Option<String>,
        visibility: super::store::Visibility,
    ) -> Result<ConversationMeta> {
        self.create_with_ownership_and_agent(id, owner, visibility, None)
    }

    /// Create a conversation and atomically stamp its canonical agent
    /// binding. The binding rides the create RPC so a plugin-backed store
    /// cannot briefly expose an unbound row between create and save_meta.
    pub(crate) fn create_with_ownership_and_agent(
        &self,
        id: Option<String>,
        owner: Option<String>,
        visibility: super::store::Visibility,
        agent_id: Option<String>,
    ) -> Result<ConversationMeta> {
        match self {
            Self::File(store) => {
                let mut meta = store.create(id)?;
                if owner.is_some() || visibility != super::store::Visibility::Private || agent_id.is_some() {
                    meta.owner = owner;
                    meta.visibility = visibility;
                    meta.agent_id = agent_id;
                    store.save_meta(&meta)?;
                }
                Ok(meta)
            }
            Self::Plugin(plugin) => plugin.create(id, owner, visibility, agent_id),
        }
    }

    /// List conversations, applying owner-aware filtering when `as_user` is
    /// `Some`: the file store has no auth context, so it returns everything
    /// and filtering happens here (owner == user OR visibility == Shared); the
    /// plugin receives `as_user` and applies the same rule server-side.
    pub(crate) fn list_for_user(&self, as_user: Option<&str>) -> Result<Vec<ConversationSummary>> {
        match self {
            Self::File(store) => {
                let all = store.list()?;
                Ok(filter_for_user(all, as_user))
            }
            // Defense in depth: the plugin receives `as_user` and is expected
            // to filter server-side, but the contract permits an auth-unaware
            // backend to ignore it and return everything. Re-apply the same
            // owner/visibility rule client-side so a private conversation owned
            // by another user can never leak through an over-broad listing.
            Self::Plugin(plugin) => Ok(filter_for_user(plugin.list(as_user)?, as_user)),
        }
    }
}

/// `true` when `as_user` may access `meta` under the owner/shared rule: no
/// acting user is the legacy/admin view (always allowed); otherwise the user
/// must own the conversation OR it must be [`super::store::Visibility::Shared`].
pub(crate) fn user_may_access(meta: &ConversationMeta, as_user: Option<&str>) -> bool {
    match as_user {
        None => true,
        Some(user) => {
            matches!(meta.visibility, super::store::Visibility::Shared) || meta.owner.as_deref() == Some(user)
        }
    }
}

/// Enforce [`user_may_access`] on a direct-ID read/write, returning a uniform
/// "not found" error (not "forbidden") so a probe cannot distinguish a private
/// conversation it may not see from one that does not exist. This is the
/// client-side backstop for `get` / `export` / `rename` / `delete`: the plugin
/// receives `as_user` and is expected to authorize server-side, but the
/// contract permits an auth-unaware backend to return rows by id regardless, so
/// the kernel re-checks before exposing or mutating the conversation.
pub(crate) fn ensure_user_may_access(meta: &ConversationMeta, id: &str, as_user: Option<&str>) -> Result<()> {
    if user_may_access(meta, as_user) {
        Ok(())
    } else {
        Err(anyhow::anyhow!("conversation '{id}' not found"))
    }
}

/// Owner-aware filter applied to file-store listings (the plugin applies the
/// equivalent rule server-side). With no `as_user`, everything passes
/// (legacy/admin view). With an `as_user`, a conversation passes when the user
/// owns it OR it is [`super::store::Visibility::Shared`].
fn filter_for_user(summaries: Vec<ConversationSummary>, as_user: Option<&str>) -> Vec<ConversationSummary> {
    match as_user {
        None => summaries,
        Some(user) => summaries
            .into_iter()
            .filter(|s| matches!(s.visibility, super::store::Visibility::Shared) || s.owner.as_deref() == Some(user))
            .collect(),
    }
}

/// JSON-RPC client over a discovered `conversation_store` plugin. Each method
/// spawns a host, runs one RPC, and reaps the host on every exit path.
#[derive(Clone)]
pub(crate) struct PluginConversationStore {
    plugin: DiscoveredPlugin,
    project_root: PathBuf,
    /// Authenticated caller supplied by the transport. This exact actor is
    /// injected at the top level of every conversation RPC, where the SDK
    /// turns it into the authoritative `CallContext`.
    actor: Actor,
    /// Host-local file store used SOLELY for its per-conversation cross-process
    /// lock, so the turn loop's whole-turn serialization holds for the plugin
    /// backend too (seq is assigned client-side). No chat data is read or
    /// written through it.
    lock_store: FileConversationStore,
}

// These bound wire structs keep the CLI compatible with the rc.11 dependency
// while emitting the contract committed in animus-protocol 6b88922. They can
// collapse back to protocol structs after that contract receives a release
// tag, without changing the JSON shape.
#[derive(Clone, serde::Serialize)]
struct BoundConversationScope {
    tenant_id: String,
    project_root: Option<String>,
    repo_scope: Option<String>,
}

fn bound_scope(project_root: &Path, actor: &Actor) -> BoundConversationScope {
    BoundConversationScope {
        tenant_id: actor.tenant_id.clone().expect("plugin actor tenant validated at construction"),
        project_root: Some(project_root.to_string_lossy().into_owned()),
        repo_scope: Some(protocol::repository_scope_for_path(project_root)),
    }
}

#[derive(serde::Serialize)]
struct BoundConversationCreateRequest {
    #[serde(flatten)]
    scope: BoundConversationScope,
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    agent_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    owner: Option<String>,
    visibility: super::store::Visibility,
}

#[derive(serde::Deserialize)]
struct BoundConversationMetaResponse {
    meta: Option<ConversationMeta>,
}

#[derive(serde::Deserialize)]
struct BoundConversationCreateResponse {
    meta: ConversationMeta,
}

#[derive(serde::Serialize)]
struct BoundConversationSaveMetaRequest<'a> {
    #[serde(flatten)]
    scope: BoundConversationScope,
    meta: &'a ConversationMeta,
    #[serde(skip_serializing_if = "Option::is_none")]
    expected_revision: Option<u64>,
    as_user: String,
}

#[derive(serde::Serialize)]
struct BoundConversationIdRequest {
    #[serde(flatten)]
    scope: BoundConversationScope,
    id: String,
    as_user: String,
}

#[derive(serde::Serialize)]
struct BoundConversationListRequest {
    #[serde(flatten)]
    scope: BoundConversationScope,
    as_user: String,
}

#[derive(serde::Deserialize)]
struct BoundConversationListResponse {
    conversations: Vec<ConversationSummary>,
}

const METHOD_OPERATION_BEGIN: &str = proto::METHOD_CONVERSATION_OPERATION_BEGIN;
const METHOD_OPERATION_LOAD: &str = proto::METHOD_CONVERSATION_OPERATION_LOAD;
const METHOD_OPERATION_RENEW: &str = proto::METHOD_CONVERSATION_OPERATION_RENEW;
const METHOD_OPERATION_BIND_EXECUTION: &str = proto::METHOD_CONVERSATION_OPERATION_BIND_EXECUTION;
const METHOD_OPERATION_RELEASE: &str = proto::METHOD_CONVERSATION_OPERATION_RELEASE;
const METHOD_OPERATION_ACCEPT_USER: &str = proto::METHOD_CONVERSATION_OPERATION_ACCEPT_USER;
const METHOD_OPERATION_TERMINALIZE: &str = proto::METHOD_CONVERSATION_OPERATION_TERMINALIZE;

fn operation_status_from_protocol(status: proto::ConversationOperationStatus) -> ChatOperationStatus {
    match status {
        proto::ConversationOperationStatus::Pending => ChatOperationStatus::Pending,
        proto::ConversationOperationStatus::UserAccepted => ChatOperationStatus::UserAccepted,
        proto::ConversationOperationStatus::Completed => ChatOperationStatus::Completed,
        proto::ConversationOperationStatus::AssistantFailed => ChatOperationStatus::AssistantFailed,
        proto::ConversationOperationStatus::AssistantInterrupted => ChatOperationStatus::AssistantInterrupted,
    }
}

fn operation_status_to_protocol(status: ChatOperationStatus) -> proto::ConversationOperationStatus {
    match status {
        ChatOperationStatus::Pending => proto::ConversationOperationStatus::Pending,
        ChatOperationStatus::UserAccepted => proto::ConversationOperationStatus::UserAccepted,
        ChatOperationStatus::Completed => proto::ConversationOperationStatus::Completed,
        ChatOperationStatus::AssistantFailed => proto::ConversationOperationStatus::AssistantFailed,
        ChatOperationStatus::AssistantInterrupted => proto::ConversationOperationStatus::AssistantInterrupted,
    }
}

fn operation_receipt(operation: proto::ConversationOperation) -> Result<ChatOperationReceipt> {
    let status = operation_status_from_protocol(operation.status);
    if !status.is_terminal() {
        return Err(anyhow!("shared chat operation receipt is not terminal"));
    }
    Ok(ChatOperationReceipt {
        operation_id: operation.operation_id,
        conversation_id: operation.conversation_id,
        user_message_id: operation.user_message_id,
        user_seq: operation.user_seq,
        assistant_message_id: operation.assistant_message_id,
        assistant_seq: operation.assistant_seq,
        status,
        error_code: operation.error_code,
        error_message: operation.error_message,
    })
}

/// Cloneable RPC authority used by the provider heartbeat thread. It carries
/// only plugin discovery metadata and the authenticated actor; lease secrets
/// live solely in `ChatOperationClaim` and never in readiness output.
#[derive(Clone)]
pub(crate) struct SharedOperationClient {
    transport: Arc<dyn SharedOperationTransport>,
    project_root: PathBuf,
    actor: Actor,
}

trait SharedOperationTransport: Send + Sync {
    fn call(&self, method: &str, params: serde_json::Value) -> Result<serde_json::Value>;
}

#[derive(Clone)]
struct PluginOperationTransport {
    plugin: DiscoveredPlugin,
    project_root: PathBuf,
}

impl SharedOperationClient {
    fn new(plugin: DiscoveredPlugin, project_root: PathBuf, actor: Actor) -> Self {
        Self {
            transport: Arc::new(PluginOperationTransport { plugin, project_root: project_root.clone() }),
            project_root,
            actor,
        }
    }

    fn scope(&self) -> BoundConversationScope {
        bound_scope(&self.project_root, &self.actor)
    }

    fn key(&self, request: &ChatOperationRequest) -> Result<proto::ConversationOperationKey> {
        let tenant = self.actor.tenant_id.as_deref().unwrap_or_default();
        if request.workspace_id != tenant || request.actor_id != self.actor.user_id {
            return Err(crate::invalid_input_error(
                "shared chat operation scope must match the authenticated conversation actor",
            ));
        }
        let scope = self.scope();
        Ok(proto::ConversationOperationKey {
            scope: proto::ConversationScope {
                tenant_id: Some(scope.tenant_id),
                project_root: scope.project_root,
                repo_scope: scope.repo_scope,
            },
            conversation_id: request.conversation_id.clone(),
            caller_key: request.caller_key.clone(),
            as_user: Some(self.actor.user_id.clone()),
        })
    }

    fn renew_request(&self, claim: &ChatOperationClaim) -> Result<proto::ConversationOperationRenewRequest> {
        Ok(proto::ConversationOperationRenewRequest {
            key: self.key(claim.request())?,
            operation_id: claim.operation_id.clone(),
            lease_token: claim.lease_token().to_string(),
        })
    }

    fn validate_response_key(
        &self,
        request: &ChatOperationRequest,
        operation: &proto::ConversationOperation,
    ) -> Result<()> {
        // Re-validate actor/tenant assertions as well as the durable key. A
        // compromised or buggy shared backend must never redirect a replay or
        // load across a conversation/key boundary.
        let _ = self.key(request)?;
        if operation.conversation_id != request.conversation_id || operation.caller_key != request.caller_key {
            return Err(anyhow!("shared operation authority returned a mismatched key"));
        }
        Ok(())
    }

    pub(crate) fn begin(&self, request: ChatOperationRequest) -> Result<ChatOperationBegin> {
        request.validate()?;
        let key = self.key(&request)?;
        let wire = proto::ConversationOperationBeginRequest {
            scope: key.scope,
            conversation_id: key.conversation_id,
            caller_key: key.caller_key,
            request_hash: request.request_hash.clone(),
            as_user: key.as_user,
        };
        let response: proto::ConversationOperationBeginResponse = self.call(METHOD_OPERATION_BEGIN, &wire)?;
        match response.outcome {
            proto::ConversationOperationBeginOutcome::InProgress => Ok(ChatOperationBegin::InProgress),
            proto::ConversationOperationBeginOutcome::Conflict => Ok(ChatOperationBegin::Conflict),
            proto::ConversationOperationBeginOutcome::Replay => {
                let operation =
                    response.operation.ok_or_else(|| anyhow!("shared operation replay omitted operation"))?;
                self.validate_response_key(&request, &operation)?;
                Ok(ChatOperationBegin::Replay(Box::new(operation_receipt(operation)?)))
            }
            proto::ConversationOperationBeginOutcome::Acquired => {
                let claim = response.claim.ok_or_else(|| anyhow!("shared operation acquire omitted claim"))?;
                self.validate_response_key(&request, &claim.operation)?;
                Ok(ChatOperationBegin::Acquired(Box::new(ChatOperationClaim::from_authority(
                    request,
                    claim.operation.operation_id,
                    claim.operation.user_message_id,
                    claim.operation.assistant_message_id,
                    operation_status_from_protocol(claim.operation.status),
                    claim.operation.user_seq,
                    claim.operation.execution_hash,
                    claim.lease_token,
                    claim.lease_expires_at,
                    claim.recovered,
                )?)))
            }
        }
    }

    pub(crate) fn renew(&self, claim: &ChatOperationClaim) -> Result<bool> {
        let response: proto::ConversationOperationMutationResponse =
            self.call(METHOD_OPERATION_RENEW, &self.renew_request(claim)?)?;
        Ok(response.changed)
    }

    pub(crate) fn bind_execution(
        &self,
        claim: &mut ChatOperationClaim,
        execution_hash: &str,
        allow_rebind: bool,
    ) -> Result<bool> {
        let lease = self.renew_request(claim)?;
        let request = proto::ConversationOperationBindExecutionRequest {
            key: lease.key,
            operation_id: lease.operation_id,
            lease_token: lease.lease_token,
            execution_hash: execution_hash.to_string(),
            allow_rebind,
        };
        let response: proto::ConversationOperationMutationResponse =
            self.call(METHOD_OPERATION_BIND_EXECUTION, &request)?;
        if response.changed {
            claim.execution_hash = Some(execution_hash.to_string());
        }
        Ok(response.changed)
    }

    pub(crate) fn release(&self, claim: &ChatOperationClaim) -> Result<bool> {
        let lease = self.renew_request(claim)?;
        let request = proto::ConversationOperationReleaseRequest {
            key: lease.key,
            operation_id: lease.operation_id,
            lease_token: lease.lease_token,
        };
        let response: proto::ConversationOperationMutationResponse = self.call(METHOD_OPERATION_RELEASE, &request)?;
        Ok(response.changed)
    }

    pub(crate) fn accept_user(&self, claim: &mut ChatOperationClaim, user_seq: u64) -> Result<bool> {
        let lease = self.renew_request(claim)?;
        let request = proto::ConversationOperationAcceptUserRequest {
            key: lease.key,
            operation_id: lease.operation_id,
            lease_token: lease.lease_token,
            user_seq,
        };
        let response: proto::ConversationOperationMutationResponse =
            self.call(METHOD_OPERATION_ACCEPT_USER, &request)?;
        if response.changed {
            claim.status = ChatOperationStatus::UserAccepted;
            claim.user_seq = Some(user_seq);
        }
        Ok(response.changed)
    }

    pub(crate) fn terminalize(
        &self,
        claim: &ChatOperationClaim,
        status: ChatOperationStatus,
        assistant_seq: Option<u64>,
        error_code: Option<&str>,
        error_message: Option<&str>,
    ) -> Result<bool> {
        let lease = self.renew_request(claim)?;
        let request = proto::ConversationOperationTerminalizeRequest {
            key: lease.key,
            operation_id: lease.operation_id,
            lease_token: lease.lease_token,
            status: operation_status_to_protocol(status),
            assistant_seq,
            error_code: error_code.map(ToOwned::to_owned),
            error_message: error_message.map(ToOwned::to_owned),
        };
        let response: proto::ConversationOperationMutationResponse =
            self.call(METHOD_OPERATION_TERMINALIZE, &request)?;
        Ok(response.changed)
    }

    pub(crate) fn receipt(&self, claim: &ChatOperationClaim) -> Result<ChatOperationReceipt> {
        let request = proto::ConversationOperationLoadRequest { key: self.key(claim.request())? };
        let response: proto::ConversationOperationLoadResponse = self.call(METHOD_OPERATION_LOAD, &request)?;
        let operation = response.operation.ok_or_else(|| anyhow!("shared chat operation disappeared"))?;
        self.validate_response_key(claim.request(), &operation)?;
        operation_receipt(operation)
    }

    fn call<Req: serde::Serialize, Resp: serde::de::DeserializeOwned>(
        &self,
        method: &str,
        request: &Req,
    ) -> Result<Resp> {
        let params = bind_actor_to_params(&self.actor, request)?;
        let value = self.transport.call(method, params)?;
        serde_json::from_value(value).with_context(|| format!("decoding {method} response"))
    }
}

impl SharedOperationTransport for PluginOperationTransport {
    fn call(&self, method: &str, params: serde_json::Value) -> Result<serde_json::Value> {
        let client = self.clone();
        let method = method.to_string();
        run_blocking(async move { client.call_async(&method, params).await })?
    }
}

impl PluginOperationTransport {
    async fn call_async(&self, method: &str, params: serde_json::Value) -> Result<serde_json::Value> {
        let options = PluginSpawnOptions::for_manifest(
            self.plugin.name.clone(),
            &self.plugin.manifest.env_required,
            std::iter::empty::<String>(),
            None,
        )
        .with_working_dir(self.project_root.clone());
        let host = PluginHost::spawn_with_options(&self.plugin.path, &[], options)
            .await
            .with_context(|| format!("spawning conversation_store plugin {}", self.plugin.name))?;
        let result = async {
            host.handshake().await?;
            let value = host.request_typed_with_timeout(method, Some(params), RPC_TIMEOUT).await?;
            Ok(value)
        }
        .await;
        let _ = host.shutdown().await;
        result
    }
}

impl PluginConversationStore {
    fn new(plugin: DiscoveredPlugin, project_root: PathBuf, actor: Actor, lock_store: FileConversationStore) -> Self {
        Self { plugin, project_root, actor, lock_store }
    }

    fn try_lock_conversation(&self, id: &str) -> Result<Option<ConversationLock>> {
        // Host-local turn serialization only. This guarantees correctness for a
        // single Animus host (the common case). When the SAME backend is shared
        // by multiple hosts, this lock cannot span them, so the backend MUST
        // make turn appends concurrency-safe itself (server-side seq allocation,
        // a `UNIQUE (conversation_id, seq)` constraint, or a per-conversation
        // advisory lock) — see the "Concurrency contract" section in
        // `animus_plugin_protocol::conversation_store`.
        self.lock_store.try_lock_conversation(id)
    }

    fn acting_user(&self) -> String {
        self.actor.user_id.clone()
    }

    fn scope(&self) -> BoundConversationScope {
        bound_scope(&self.project_root, &self.actor)
    }

    fn create(
        &self,
        id: Option<String>,
        owner: Option<String>,
        visibility: super::store::Visibility,
        agent_id: Option<String>,
    ) -> Result<ConversationMeta> {
        if owner.as_deref().is_some_and(|value| value != self.actor.user_id) {
            return Err(crate::invalid_input_error("conversation owner assertion must equal actor user_id"));
        }
        let owner = Some(self.actor.user_id.clone());
        let request = BoundConversationCreateRequest { scope: self.scope(), id, agent_id, owner, visibility };
        let resp: BoundConversationCreateResponse = self.call(proto::METHOD_CONVERSATION_CREATE, &request)?;
        Ok(resp.meta)
    }

    fn load_meta(&self, id: &str) -> Result<Option<ConversationMeta>> {
        let request =
            BoundConversationIdRequest { scope: self.scope(), id: id.to_string(), as_user: self.acting_user() };
        let resp: BoundConversationMetaResponse = self.call(proto::METHOD_CONVERSATION_LOAD_META, &request)?;
        Ok(resp.meta)
    }

    fn save_meta(&self, meta: &ConversationMeta, expected_revision: Option<u64>) -> Result<()> {
        let request = BoundConversationSaveMetaRequest {
            scope: self.scope(),
            meta,
            expected_revision,
            as_user: self.acting_user(),
        };
        let _: proto::ConversationSaveMetaResponse = self.call(proto::METHOD_CONVERSATION_SAVE_META, &request)?;
        Ok(())
    }

    fn append_message(&self, id: &str, message: &ChatMessage) -> Result<()> {
        self.append_message_fenced(id, message, None)
    }

    fn append_message_fenced(
        &self,
        id: &str,
        message: &ChatMessage,
        operation_fence: Option<&proto::ConversationOperationAppendFence>,
    ) -> Result<()> {
        let scope = self.scope();
        let request = proto::ConversationAppendMessageRequest {
            scope: proto::ConversationScope {
                tenant_id: Some(scope.tenant_id),
                project_root: scope.project_root,
                repo_scope: scope.repo_scope,
            },
            id: id.to_string(),
            message: to_proto_message(message)?,
            operation_fence: operation_fence.cloned(),
            as_user: Some(self.acting_user()),
        };
        let _: proto::ConversationAppendMessageResponse =
            self.call(proto::METHOD_CONVERSATION_APPEND_MESSAGE, &request)?;
        Ok(())
    }

    fn load_messages(&self, id: &str) -> Result<Vec<ChatMessage>> {
        let request =
            BoundConversationIdRequest { scope: self.scope(), id: id.to_string(), as_user: self.acting_user() };
        let resp: proto::ConversationLoadMessagesResponse =
            self.call(proto::METHOD_CONVERSATION_LOAD_MESSAGES, &request)?;
        resp.messages.into_iter().map(from_proto_message).collect()
    }

    fn list(&self, as_user: Option<&str>) -> Result<Vec<ConversationSummary>> {
        if as_user.is_some_and(|value| value != self.actor.user_id) {
            return Err(crate::invalid_input_error("conversation user assertion must equal actor user_id"));
        }
        let request = BoundConversationListRequest { scope: self.scope(), as_user: self.acting_user() };
        let resp: BoundConversationListResponse = self.call(proto::METHOD_CONVERSATION_LIST, &request)?;
        Ok(resp.conversations)
    }

    fn delete(&self, id: &str) -> Result<()> {
        let request =
            BoundConversationIdRequest { scope: self.scope(), id: id.to_string(), as_user: self.acting_user() };
        let _: proto::ConversationDeleteResponse = self.call(proto::METHOD_CONVERSATION_DELETE, &request)?;
        Ok(())
    }

    /// Spawn the plugin host, handshake, run one RPC, and ALWAYS reap the host.
    fn call<Req: serde::Serialize, Resp: serde::de::DeserializeOwned>(
        &self,
        method: &str,
        request: &Req,
    ) -> Result<Resp> {
        let params = self.actor_bound_params(request)?;
        let client = self.clone();
        let method = method.to_string();
        let response_method = method.clone();
        let value = run_blocking(async move { client.call_async(&method, params).await })??;
        serde_json::from_value(value).with_context(|| format!("decoding {response_method} response"))
    }

    fn actor_bound_params(&self, request: &impl serde::Serialize) -> Result<serde_json::Value> {
        bind_actor_to_params(&self.actor, request)
    }

    async fn call_async(&self, method: &str, params: serde_json::Value) -> Result<serde_json::Value> {
        let options = PluginSpawnOptions::for_manifest(
            self.plugin.name.clone(),
            &self.plugin.manifest.env_required,
            std::iter::empty::<String>(),
            None,
        )
        .with_working_dir(self.project_root.clone());
        let host = PluginHost::spawn_with_options(&self.plugin.path, &[], options)
            .await
            .with_context(|| format!("spawning conversation_store plugin {}", self.plugin.name))?;
        // Run the handshake + RPC, then ALWAYS shut the host down so the spawned
        // process (and any DB connection pool it holds) is reaped — a persistent
        // stdio plugin never EOFs, so dropping the handle alone would leak it.
        let result = self.handshake_and_call(&host, method, params).await;
        let _ = host.shutdown().await;
        result
    }

    async fn handshake_and_call(
        &self,
        host: &PluginHost,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value> {
        host.handshake()
            .await
            .with_context(|| format!("handshake with conversation_store plugin {}", self.plugin.name))?;
        host.request_typed_with_timeout(method, Some(params), RPC_TIMEOUT)
            .await
            .with_context(|| format!("{method} on conversation_store plugin {}", self.plugin.name))
    }

    /// Run a whole `chat search` over ONE spawned plugin host. The per-rpc
    /// `call` path spawns+reaps the host every call, so the search's N+1 (a
    /// `list` then one `load_messages` per conversation) would spawn the plugin
    /// N+1 times. Here we spawn once, handshake once, run every rpc over the live host, and
    /// reap once — turning ~N spawns into a single one.
    async fn search_async(
        &self,
        query: &str,
        case_insensitive: bool,
        limit: usize,
        as_user: Option<&str>,
    ) -> Result<Vec<super::SearchMatch>> {
        if query.is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        if as_user.is_some_and(|value| value != self.actor.user_id) {
            return Err(crate::invalid_input_error("conversation user assertion must equal actor user_id"));
        }
        let options = PluginSpawnOptions::for_manifest(
            self.plugin.name.clone(),
            &self.plugin.manifest.env_required,
            std::iter::empty::<String>(),
            None,
        )
        .with_working_dir(self.project_root.clone());
        let host = PluginHost::spawn_with_options(&self.plugin.path, &[], options)
            .await
            .with_context(|| format!("spawning conversation_store plugin {}", self.plugin.name))?;
        // Reap the host (and its DB pool) no matter how the scan ends — a
        // persistent stdio plugin never EOFs, so dropping the handle leaks it.
        let scan = async {
            host.handshake()
                .await
                .with_context(|| format!("handshake with conversation_store plugin {}", self.plugin.name))?;
            let list_params = self.actor_bound_params(&BoundConversationListRequest {
                scope: self.scope(),
                as_user: self.acting_user(),
            })?;
            let list_val = host
                .request_typed_with_timeout(proto::METHOD_CONVERSATION_LIST, Some(list_params), RPC_TIMEOUT)
                .await
                .with_context(|| format!("conversation/list on {}", self.plugin.name))?;
            let list_resp: BoundConversationListResponse = serde_json::from_value(list_val)?;
            let mut out: Vec<super::SearchMatch> = Vec::new();
            for summary in list_resp.conversations {
                if out.len() >= limit {
                    break;
                }
                let lm_params = self.actor_bound_params(&BoundConversationIdRequest {
                    scope: self.scope(),
                    id: summary.id.clone(),
                    as_user: self.acting_user(),
                })?;
                let lm_val = host
                    .request_typed_with_timeout(proto::METHOD_CONVERSATION_LOAD_MESSAGES, Some(lm_params), RPC_TIMEOUT)
                    .await
                    .with_context(|| format!("conversation/load_messages on {}", self.plugin.name))?;
                let lm_resp: proto::ConversationLoadMessagesResponse = serde_json::from_value(lm_val)?;
                for pm in lm_resp.messages {
                    if out.len() >= limit {
                        break;
                    }
                    let m = from_proto_message(pm)?;
                    if let Some(snippet) = super::snippet_around(&m.content, query, case_insensitive) {
                        out.push(super::SearchMatch {
                            conversation_id: summary.id.clone(),
                            title: summary.title.clone(),
                            role: super::role_str(m.role),
                            seq: m.seq,
                            snippet,
                        });
                    }
                }
            }
            Ok::<_, anyhow::Error>(out)
        }
        .await;
        let _ = host.shutdown().await;
        scan
    }
}

fn async_bridge_runtime() -> Result<&'static tokio::runtime::Runtime> {
    static BRIDGE: std::sync::OnceLock<std::result::Result<tokio::runtime::Runtime, String>> =
        std::sync::OnceLock::new();
    match BRIDGE.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .thread_name("animus-chat-rpc-bridge")
            .enable_all()
            .build()
            .map_err(|error| format!("building conversation-store async bridge: {error}"))
    }) {
        Ok(runtime) => Ok(runtime),
        Err(error) => Err(anyhow!(error.clone())),
    }
}

const ASYNC_BRIDGE_MAX_IN_FLIGHT: usize = 8;
static ASYNC_BRIDGE_IN_FLIGHT: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(ASYNC_BRIDGE_MAX_IN_FLIGHT);

/// Bridge an owned async future into the sync `ConversationStore` trait. All
/// callers share one bounded worker runtime and a fixed in-flight semaphore,
/// so this is safe under both Tokio runtime flavors, never creates a runtime
/// or thread per RPC, and cannot fan out an unbounded number of plugin hosts.
fn run_blocking<F>(fut: F) -> Result<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    let (result_tx, result_rx) = std::sync::mpsc::sync_channel(1);
    async_bridge_runtime()?.spawn(async move {
        let _permit = ASYNC_BRIDGE_IN_FLIGHT.acquire().await.expect("chat RPC bridge semaphore is never closed");
        let _ = result_tx.send(fut.await);
    });
    result_rx.recv().map_err(|_| anyhow!("conversation-store async bridge dropped an RPC result"))
}

fn to_proto_message(message: &ChatMessage) -> Result<proto::ChatMessage> {
    serde_json::from_value(serde_json::to_value(message).context("serializing ChatMessage")?)
        .context("converting ChatMessage to protocol shape")
}

fn from_proto_message(message: proto::ChatMessage) -> Result<ChatMessage> {
    serde_json::from_value(serde_json::to_value(message).context("serializing protocol ChatMessage")?)
        .context("converting protocol ChatMessage to kernel shape")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::runtime::runtime_chat::store::Visibility;
    use animus_plugin_protocol::PluginManifest;
    use orchestrator_plugin_host::DiscoverySource;
    use std::sync::Mutex;

    #[derive(Default)]
    struct MockOperationState {
        admitted: bool,
        terminal: bool,
        calls: Vec<(String, serde_json::Value)>,
    }

    #[derive(Default)]
    struct MockOperationTransport {
        state: Mutex<MockOperationState>,
    }

    struct MismatchedOperationTransport;

    impl SharedOperationTransport for MismatchedOperationTransport {
        fn call(&self, method: &str, _params: serde_json::Value) -> Result<serde_json::Value> {
            let operation = serde_json::json!({
                "operation_id": "op-hostile",
                "conversation_id": "other-conversation",
                "caller_key": "other-key",
                "user_message_id": "msg-user-hostile",
                "assistant_message_id": "msg-assistant-hostile",
                "status": "completed",
                "user_seq": 0,
                "assistant_seq": 1,
            });
            match method {
                METHOD_OPERATION_BEGIN => Ok(serde_json::json!({"outcome":"replay", "operation":operation})),
                METHOD_OPERATION_LOAD => Ok(serde_json::json!({"operation":operation})),
                other => Err(anyhow!("unexpected hostile mock method {other}")),
            }
        }
    }

    impl SharedOperationTransport for MockOperationTransport {
        fn call(&self, method: &str, params: serde_json::Value) -> Result<serde_json::Value> {
            let mut state = self.state.lock().unwrap();
            state.calls.push((method.to_string(), params.clone()));
            let operation = serde_json::json!({
                "operation_id": "op-1",
                "conversation_id": "conv-1",
                "caller_key": "request-1",
                "user_message_id": "msg-user-1",
                "assistant_message_id": "msg-assistant-1",
                "status": if state.terminal { "completed" } else { "pending" },
                "user_seq": if state.terminal { Some(0) } else { None },
                "assistant_seq": if state.terminal { Some(1) } else { None },
            });
            match method {
                METHOD_OPERATION_BEGIN if state.terminal => {
                    Ok(serde_json::json!({"outcome":"replay", "operation": operation}))
                }
                METHOD_OPERATION_BEGIN if state.admitted => Ok(serde_json::json!({"outcome":"in_progress"})),
                METHOD_OPERATION_BEGIN => {
                    state.admitted = true;
                    Ok(serde_json::json!({
                        "outcome":"acquired",
                        "claim": {
                            "operation_id":"op-1",
                            "conversation_id":"conv-1",
                            "caller_key":"request-1",
                            "user_message_id":"msg-user-1",
                            "assistant_message_id":"msg-assistant-1",
                            "status":"pending",
                            "lease_token":"secret-lease-token",
                            "lease_expires_at": 4_000_000_000_i64,
                            "recovered": false
                        }
                    }))
                }
                METHOD_OPERATION_TERMINALIZE => {
                    state.terminal = true;
                    Ok(serde_json::json!({"changed":true, "operation": operation}))
                }
                METHOD_OPERATION_LOAD => Ok(serde_json::json!({"operation": operation})),
                METHOD_OPERATION_RENEW | METHOD_OPERATION_BIND_EXECUTION | METHOD_OPERATION_ACCEPT_USER => {
                    Ok(serde_json::json!({"changed":true, "operation": operation}))
                }
                METHOD_OPERATION_RELEASE => Ok(serde_json::json!({"changed":false})),
                other => Err(anyhow!("unexpected mock operation method {other}")),
            }
        }
    }

    fn mock_shared_client(transport: Arc<dyn SharedOperationTransport>) -> SharedOperationClient {
        SharedOperationClient {
            transport,
            project_root: PathBuf::from("/repo"),
            actor: actor("alice", Some("tenant-a")),
        }
    }

    fn mock_operation_request() -> ChatOperationRequest {
        ChatOperationRequest {
            project_scope: "repo-scope".into(),
            workspace_id: "tenant-a".into(),
            actor_id: "alice".into(),
            conversation_id: "conv-1".into(),
            caller_key: "request-1".into(),
            request_hash: "intent-hash".into(),
        }
    }

    #[test]
    fn two_runtime_shared_rpc_contract_excludes_then_replays_without_leaking_lease() {
        let transport = Arc::new(MockOperationTransport::default());
        let runtime_a = mock_shared_client(transport.clone());
        let runtime_b = mock_shared_client(transport.clone());
        let ChatOperationBegin::Acquired(mut claim) = runtime_a.begin(mock_operation_request()).unwrap() else {
            panic!("first runtime must acquire");
        };
        assert!(matches!(runtime_b.begin(mock_operation_request()).unwrap(), ChatOperationBegin::InProgress));
        assert!(runtime_a.bind_execution(&mut claim, "execution-hash", false).unwrap());
        assert!(runtime_a.accept_user(&mut claim, 0).unwrap());
        assert!(runtime_a.terminalize(&claim, ChatOperationStatus::Completed, Some(1), None, None).unwrap());
        let ChatOperationBegin::Replay(receipt) = runtime_b.begin(mock_operation_request()).unwrap() else {
            panic!("second runtime must replay the shared terminal receipt");
        };
        let json = serde_json::to_value(&receipt).unwrap();
        assert_eq!(json["status"], "completed");
        assert!(json.get("lease_token").is_none(), "lease credentials must never enter a receipt");

        let state = transport.state.lock().unwrap();
        assert_eq!(state.calls[0].0, METHOD_OPERATION_BEGIN);
        assert_eq!(state.calls[0].1["tenant_id"], "tenant-a");
        assert_eq!(state.calls[0].1["actor"]["user_id"], "alice");
        assert_eq!(state.calls[0].1["repo_scope"], protocol::repository_scope_for_path(Path::new("/repo")));
        assert!(state.calls.iter().any(|(method, _)| method == METHOD_OPERATION_BIND_EXECUTION));
        assert!(state.calls.iter().any(|(method, _)| method == METHOD_OPERATION_ACCEPT_USER));
        assert!(state.calls.iter().any(|(method, _)| method == METHOD_OPERATION_TERMINALIZE));
    }

    #[test]
    fn hostile_backend_cannot_redirect_replay_or_load_across_operation_key() {
        let client = mock_shared_client(Arc::new(MismatchedOperationTransport));
        let replay_error = client.begin(mock_operation_request()).unwrap_err();
        assert!(replay_error.to_string().contains("mismatched key"));

        let claim = ChatOperationClaim::from_authority(
            mock_operation_request(),
            "op-1".to_string(),
            "msg-user-1".to_string(),
            "msg-assistant-1".to_string(),
            ChatOperationStatus::UserAccepted,
            Some(0),
            Some("execution-hash".to_string()),
            "lease-secret".to_string(),
            chrono::Utc::now().timestamp() + 300,
            false,
        )
        .unwrap();
        let load_error = client.receipt(&claim).unwrap_err();
        assert!(load_error.to_string().contains("mismatched key"));
    }

    #[test]
    fn backend_readiness_has_all_five_stable_variants() {
        let cases = [
            (
                BackendDiscovery::Failed,
                "unavailable",
                "unavailable",
                false,
                false,
                Some("conversation_store_discovery_failed"),
            ),
            (BackendDiscovery::File, "file", "local_sqlite", false, true, None),
            (
                BackendDiscovery::Plugin { shared: true, probe_succeeded: true },
                "plugin",
                "shared_conversation_store_rpc",
                true,
                true,
                None,
            ),
            (
                BackendDiscovery::Plugin { shared: true, probe_succeeded: false },
                "plugin",
                "unavailable",
                true,
                false,
                Some("conversation_store_probe_failed"),
            ),
            (
                BackendDiscovery::Plugin { shared: false, probe_succeeded: false },
                "plugin",
                "unavailable",
                false,
                false,
                Some("shared_operation_authority_missing"),
            ),
        ];
        for (input, kind, mode, observed, ready, error) in cases {
            let value = readiness_for(input);
            assert_eq!(value.schema, "animus.chat.backend_readiness.v1");
            assert_eq!(value.kind, kind);
            assert_eq!(value.authority_mode, mode);
            assert_eq!(value.required_capability, SHARED_OPERATION_CAPABILITY);
            assert_eq!(value.required_capability_observed, observed);
            assert_eq!(value.ready, ready);
            assert_eq!(value.error_code, error);
        }
    }

    #[test]
    fn advertised_shared_capability_is_not_ready_when_live_probe_fails() {
        let plugin = DiscoveredPlugin {
            name: "dead-conversation-store".to_string(),
            path: PathBuf::from("/definitely/missing/dead-conversation-store"),
            manifest: PluginManifest {
                name: "dead-conversation-store".to_string(),
                version: "0.0.0".to_string(),
                plugin_kind: CONVERSATION_STORE_KIND.to_string(),
                plugin_kinds: Vec::new(),
                description: "test fixture".to_string(),
                protocol_version: "1.0.0".to_string(),
                capabilities: REQUIRED_SHARED_OPERATION_METHODS
                    .iter()
                    .map(ToString::to_string)
                    .chain([SHARED_OPERATION_CAPABILITY.to_string(), FENCED_APPEND_CAPABILITY.to_string()])
                    .collect(),
                env_required: Vec::new(),
                notification_buffer_size: None,
                supports_mcp: None,
            },
            source: DiscoverySource::ExplicitConfig,
        };
        assert!(plugin_supports_shared_authority(&plugin));
        assert!(probe_plugin_backend(&plugin, Path::new("/repo")).is_err());
        let readiness = readiness_for(BackendDiscovery::Plugin { shared: true, probe_succeeded: false });
        assert!(!readiness.ready);
        assert_eq!(readiness.error_code, Some("conversation_store_probe_failed"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn async_bridge_is_safe_inside_a_current_thread_runtime() {
        let value = run_blocking(async { 42_u8 }).unwrap();
        assert_eq!(value, 42);
    }

    #[test]
    fn shared_authority_requires_marker_fence_and_all_seven_methods() {
        let mut capabilities: Vec<String> = REQUIRED_SHARED_OPERATION_METHODS.iter().map(ToString::to_string).collect();
        capabilities.push(SHARED_OPERATION_CAPABILITY.to_string());
        capabilities.push(FENCED_APPEND_CAPABILITY.to_string());
        let plugin = |capabilities| DiscoveredPlugin {
            name: "conversation-store".to_string(),
            path: PathBuf::from("/unused"),
            manifest: PluginManifest {
                name: "conversation-store".to_string(),
                version: "0.0.0".to_string(),
                plugin_kind: CONVERSATION_STORE_KIND.to_string(),
                plugin_kinds: Vec::new(),
                description: "test fixture".to_string(),
                protocol_version: "1.0.0".to_string(),
                capabilities,
                env_required: Vec::new(),
                notification_buffer_size: None,
                supports_mcp: None,
            },
            source: DiscoverySource::ExplicitConfig,
        };

        assert!(plugin_supports_shared_authority(&plugin(capabilities.clone())));
        for required in REQUIRED_SHARED_OPERATION_METHODS {
            let missing = capabilities.iter().filter(|value| value.as_str() != required).cloned().collect();
            assert!(!plugin_supports_shared_authority(&plugin(missing)), "missing {required} must fail closed");
        }
        assert!(!plugin_supports_shared_authority(&plugin(
            capabilities.iter().filter(|value| value.as_str() != SHARED_OPERATION_CAPABILITY).cloned().collect()
        )));
        assert!(!plugin_supports_shared_authority(&plugin(
            capabilities.iter().filter(|value| value.as_str() != FENCED_APPEND_CAPABILITY).cloned().collect()
        )));
    }

    #[test]
    fn live_operation_surface_and_missing_key_probe_fail_closed() {
        let complete: Vec<String> = REQUIRED_SHARED_OPERATION_METHODS.iter().map(ToString::to_string).collect();
        require_complete_operation_surface(&complete).unwrap();
        let incomplete =
            complete.into_iter().filter(|method| method != METHOD_OPERATION_TERMINALIZE).collect::<Vec<_>>();
        assert!(require_complete_operation_surface(&incomplete)
            .unwrap_err()
            .to_string()
            .contains(METHOD_OPERATION_TERMINALIZE));

        let missing_id = "__animus_readiness_probe_missing__";
        assert!(operation_probe_rejected_missing_key(
            &anyhow!("rpc error -32603: conversation '{missing_id}' not found or not writable for operation authority"),
            missing_id,
        ));
        assert!(!operation_probe_rejected_missing_key(
            &anyhow!("rpc error -32601: method not found: {missing_id}"),
            missing_id,
        ));
        assert!(!operation_probe_rejected_missing_key(&anyhow!("database unavailable"), missing_id));
    }

    #[test]
    fn async_bridge_bounds_concurrent_futures() {
        let active = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let peak = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut callers = Vec::new();
        for _ in 0..(ASYNC_BRIDGE_MAX_IN_FLIGHT * 3) {
            let active = active.clone();
            let peak = peak.clone();
            callers.push(std::thread::spawn(move || {
                run_blocking(async move {
                    let now = active.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                    peak.fetch_max(now, std::sync::atomic::Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(20)).await;
                    active.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
                })
                .unwrap();
            }));
        }
        for caller in callers {
            caller.join().unwrap();
        }
        assert!(peak.load(std::sync::atomic::Ordering::SeqCst) <= ASYNC_BRIDGE_MAX_IN_FLIGHT);
    }

    #[test]
    fn remote_authority_capability_is_fail_closed() {
        require_remote_shared_authority(true).unwrap();
        let error = require_remote_shared_authority(false).unwrap_err();
        assert!(error.to_string().contains("keyed sends fail closed"));
    }

    #[test]
    fn portal_required_authority_rejects_file_fallback_after_plugin_disappearance() {
        let root = tempfile::tempdir().unwrap();
        let store = ConversationStoreClient::with_root_for_test(root.path().to_path_buf());
        assert!(store.shared_operation_client(false).unwrap().is_none());
        let error = match store.shared_operation_client(true) {
            Ok(_) => panic!("portal-required sends must reject local fallback"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("forbids host-local SQLite"));
    }

    fn summary(id: &str, owner: Option<&str>, visibility: Visibility) -> ConversationSummary {
        ConversationSummary {
            id: id.to_string(),
            agent_id: None,
            revision: 0,
            title: None,
            tool: None,
            model: None,
            message_count: 0,
            updated_at: "2026-06-21T00:00:00Z".to_string(),
            owner: owner.map(ToOwned::to_owned),
            visibility,
        }
    }

    #[test]
    fn filter_none_returns_everything() {
        let all = vec![
            summary("a", Some("u1"), Visibility::Private),
            summary("b", None, Visibility::Private),
            summary("c", Some("u2"), Visibility::Shared),
        ];
        assert_eq!(filter_for_user(all, None).len(), 3, "no as_user is the legacy/admin view");
    }

    #[test]
    fn filter_user_returns_own_plus_shared() {
        let all = vec![
            summary("own-private", Some("u1"), Visibility::Private),
            summary("other-private", Some("u2"), Visibility::Private),
            summary("other-shared", Some("u2"), Visibility::Shared),
            summary("unowned-private", None, Visibility::Private),
        ];
        let got: Vec<String> = filter_for_user(all, Some("u1")).into_iter().map(|s| s.id).collect();
        assert_eq!(got, vec!["own-private".to_string(), "other-shared".to_string()]);
    }

    fn meta_with(owner: Option<&str>, visibility: Visibility) -> ConversationMeta {
        ConversationMeta {
            id: "conv-z".to_string(),
            agent_id: None,
            revision: 0,
            active_operation_id: None,
            tool: None,
            model: None,
            session_id: None,
            title: None,
            created_at: "2026-06-21T00:00:00Z".to_string(),
            updated_at: "2026-06-21T00:00:00Z".to_string(),
            message_count: 0,
            owner: owner.map(ToOwned::to_owned),
            visibility,
        }
    }

    #[test]
    fn user_may_access_enforces_owner_and_shared() {
        // No acting user = legacy/admin view: everything is accessible.
        assert!(user_may_access(&meta_with(Some("u2"), Visibility::Private), None));
        // Owner sees their own private conversation.
        assert!(user_may_access(&meta_with(Some("u1"), Visibility::Private), Some("u1")));
        // Anyone sees a shared conversation.
        assert!(user_may_access(&meta_with(Some("u2"), Visibility::Shared), Some("u1")));
        // A non-owner is denied another user's private conversation.
        assert!(!user_may_access(&meta_with(Some("u2"), Visibility::Private), Some("u1")));
        // An unowned private conversation is denied to any scoped user.
        assert!(!user_may_access(&meta_with(None, Visibility::Private), Some("u1")));
    }

    #[test]
    fn ensure_user_may_access_denies_as_not_found() {
        let denied = ensure_user_may_access(&meta_with(Some("u2"), Visibility::Private), "conv-z", Some("u1"));
        let err = denied.unwrap_err().to_string();
        assert!(err.contains("not found"), "denial must read as not-found, got: {err}");
        // Allowed access returns Ok.
        assert!(ensure_user_may_access(&meta_with(Some("u1"), Visibility::Private), "conv-z", Some("u1")).is_ok());
    }

    #[test]
    fn bound_meta_round_trips_through_wire_json() {
        let meta = ConversationMeta {
            id: "conv-x".to_string(),
            agent_id: Some("researcher".to_string()),
            revision: 7,
            active_operation_id: None,
            tool: Some("claude".to_string()),
            model: Some("claude-sonnet-4-6".to_string()),
            session_id: Some("sess-1".to_string()),
            title: Some("Hi".to_string()),
            created_at: "2026-06-21T00:00:00Z".to_string(),
            updated_at: "2026-06-21T01:00:00Z".to_string(),
            message_count: 4,
            owner: Some("u1".to_string()),
            visibility: Visibility::Shared,
        };
        let wire = serde_json::to_value(&meta).unwrap();
        let back: ConversationMeta = serde_json::from_value(wire).unwrap();
        assert_eq!(back.id, meta.id);
        assert_eq!(back.agent_id, meta.agent_id);
        assert_eq!(back.owner, meta.owner);
        assert_eq!(back.visibility, meta.visibility);
        assert_eq!(back.session_id, meta.session_id);
    }

    #[test]
    fn plugin_wire_preserves_create_binding_and_save_revision_precondition() {
        let scope = BoundConversationScope {
            tenant_id: "tenant-a".into(),
            project_root: Some("/repo".into()),
            repo_scope: Some("scope-1".into()),
        };
        let create = BoundConversationCreateRequest {
            scope: scope.clone(),
            id: Some("conv-x".into()),
            agent_id: Some("researcher".into()),
            owner: Some("u1".into()),
            visibility: Visibility::Private,
        };
        let value = serde_json::to_value(create).unwrap();
        assert_eq!(value["agent_id"], "researcher");
        assert_eq!(value["project_root"], "/repo");
        assert_eq!(value["tenant_id"], "tenant-a");

        let meta = meta_with(Some("u1"), Visibility::Private);
        let save =
            BoundConversationSaveMetaRequest { scope, meta: &meta, expected_revision: Some(7), as_user: "u1".into() };
        let value = serde_json::to_value(save).unwrap();
        assert_eq!(value["expected_revision"], 7);
        assert_eq!(value["meta"]["revision"], 0);
    }

    fn actor(user_id: &str, tenant_id: Option<&str>) -> Actor {
        Actor {
            user_id: user_id.to_string(),
            claims: vec!["member".to_string()],
            tenant_id: tenant_id.map(ToOwned::to_owned),
        }
    }

    #[test]
    fn as_user_is_only_a_matching_actor_assertion() {
        let alice = actor("alice", Some("tenant-a"));
        assert_eq!(assert_actor_matches_as_user(Some(&alice), Some("alice")).unwrap(), Some("alice"));
        assert!(assert_actor_matches_as_user(Some(&alice), Some("mallory"))
            .unwrap_err()
            .to_string()
            .contains("must equal"));
        assert!(assert_actor_matches_as_user(None, Some("mallory"))
            .unwrap_err()
            .to_string()
            .contains("requires --actor-json"));
    }

    #[test]
    fn plugin_actor_requires_non_empty_user_and_tenant() {
        assert!(require_plugin_actor(None).unwrap_err().to_string().contains("requires --actor-json"));
        assert!(require_plugin_actor(Some(actor("", Some("tenant-a")))).unwrap_err().to_string().contains("user_id"));
        assert!(require_plugin_actor(Some(actor("alice", None))).unwrap_err().to_string().contains("tenant_id"));
        assert!(require_plugin_actor(Some(actor("alice", Some("  ")))).unwrap_err().to_string().contains("tenant_id"));
    }

    #[test]
    fn actor_bound_wire_uses_one_authoritative_tenant_and_user() {
        let alice = require_plugin_actor(Some(actor("alice", Some("tenant-a")))).unwrap();
        let request = BoundConversationListRequest {
            scope: bound_scope(Path::new("/repo"), &alice),
            as_user: alice.user_id.clone(),
        };
        let value = bind_actor_to_params(&alice, &request).unwrap();
        assert_eq!(value["tenant_id"], "tenant-a");
        assert_eq!(value["as_user"], "alice");
        assert_eq!(value["actor"]["user_id"], "alice");
        assert_eq!(value["actor"]["tenant_id"], "tenant-a");

        let hostile = actor("mallory", Some("tenant-b"));
        assert!(assert_actor_matches_as_user(Some(&hostile), Some("alice")).is_err());
        assert_eq!(bound_scope(Path::new("/repo"), &hostile).tenant_id, "tenant-b");
        assert_ne!(bound_scope(Path::new("/repo"), &alice).tenant_id, "tenant-b");

        let forged = serde_json::json!({
            "tenant_id": "tenant-b",
            "actor": {"user_id": "mallory", "tenant_id": "tenant-b"},
            "as_user": "alice"
        });
        let rebound = bind_actor_to_params(&alice, &forged).unwrap();
        assert_eq!(rebound["tenant_id"], "tenant-a");
        assert_eq!(rebound["actor"]["user_id"], "alice");
        assert_eq!(rebound["actor"]["tenant_id"], "tenant-a");
    }
}
