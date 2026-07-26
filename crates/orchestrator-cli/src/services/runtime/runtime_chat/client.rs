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
use std::time::Duration;

use animus_actor::Actor;
use animus_plugin_protocol::conversation_store as proto;
use anyhow::{Context, Result};
use orchestrator_plugin_host::{discover_by_kind, DiscoveredPlugin, PluginHost, PluginSpawnOptions};

use super::store::{
    ChatMessage, ConversationLock, ConversationMeta, ConversationStore, ConversationSummary, FileConversationStore,
};

const CONVERSATION_STORE_KIND: &str = "conversation_store";
const RPC_TIMEOUT: Duration = Duration::from_secs(30);

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
            Self::Plugin(plugin) => run_blocking(plugin.search_async(query, case_insensitive, limit, as_user))?,
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
struct BoundConversationAppendMessageRequest {
    #[serde(flatten)]
    scope: BoundConversationScope,
    id: String,
    message: proto::ChatMessage,
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
        let request = BoundConversationAppendMessageRequest {
            scope: self.scope(),
            id: id.to_string(),
            message: to_proto_message(message)?,
            as_user: self.acting_user(),
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
        run_blocking(self.call_async(method, params))?
    }

    fn actor_bound_params(&self, request: &impl serde::Serialize) -> Result<serde_json::Value> {
        bind_actor_to_params(&self.actor, request)
    }

    async fn call_async<Resp: serde::de::DeserializeOwned>(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<Resp> {
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

    async fn handshake_and_call<Resp: serde::de::DeserializeOwned>(
        &self,
        host: &PluginHost,
        method: &str,
        params: serde_json::Value,
    ) -> Result<Resp> {
        host.handshake()
            .await
            .with_context(|| format!("handshake with conversation_store plugin {}", self.plugin.name))?;
        let value = host
            .request_typed_with_timeout(method, Some(params), RPC_TIMEOUT)
            .await
            .with_context(|| format!("{method} on conversation_store plugin {}", self.plugin.name))?;
        serde_json::from_value(value).with_context(|| format!("decoding {method} response"))
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

/// Bridge an async future into the sync `ConversationStore` trait. Works
/// whether or not a tokio runtime is already running (daemon = inside a
/// runtime; CLI = none). Mirrors `config_source_client::run_blocking`.
fn run_blocking<F: Future>(fut: F) -> Result<F::Output> {
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => Ok(tokio::task::block_in_place(|| handle.block_on(fut))),
        Err(_) => {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .context("building tokio runtime for conversation_store call")?;
            Ok(rt.block_on(fut))
        }
    }
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
