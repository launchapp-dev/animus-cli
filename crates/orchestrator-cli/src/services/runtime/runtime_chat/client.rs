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
    /// Resolve the conversation store for `project_root`, with no acting user.
    /// Routes to an installed `conversation_store` plugin when one is
    /// discovered, else falls back to the in-tree filesystem store.
    pub(crate) fn for_project(project_root: &Path) -> Result<Self> {
        Self::for_project_as_user(project_root, None)
    }

    /// Like [`Self::for_project`] but records the acting user id. When a
    /// `conversation_store` plugin is selected, that id rides every
    /// per-conversation RPC (`load_meta`, `save_meta`, `append_message`,
    /// `load_messages`, `delete`) so the backend can authorize the actor — the
    /// foundation for per-user access enforcement. The in-tree file store has
    /// no auth context and ignores it (filtering happens at the query layer).
    pub(crate) fn for_project_as_user(project_root: &Path, as_user: Option<&str>) -> Result<Self> {
        let plugins = discover_by_kind(project_root.to_path_buf(), CONVERSATION_STORE_KIND)
            .with_context(|| format!("discovering conversation_store plugins for {}", project_root.display()))?;
        match plugins.into_iter().next() {
            Some(plugin) => Ok(Self::Plugin(Box::new(PluginConversationStore::new(
                plugin,
                project_root.to_path_buf(),
                as_user.map(ToOwned::to_owned),
                FileConversationStore::for_project(project_root)?,
            )))),
            None => Ok(Self::File(FileConversationStore::for_project(project_root)?)),
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
            Self::Plugin(plugin) => plugin.save_meta(meta),
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
        match self {
            Self::File(store) => {
                let mut meta = store.create(id)?;
                if owner.is_some() || visibility != super::store::Visibility::Private {
                    meta.owner = owner;
                    meta.visibility = visibility;
                    store.save_meta(&meta)?;
                }
                Ok(meta)
            }
            Self::Plugin(plugin) => plugin.create(id, owner, visibility),
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
    /// Acting user id (from the chat verb's `--as-user`), threaded onto every
    /// per-conversation RPC so the backend can authorize the actor. `None` for
    /// unscoped/admin access.
    acting_user: Option<String>,
    /// Host-local file store used SOLELY for its per-conversation cross-process
    /// lock, so the turn loop's whole-turn serialization holds for the plugin
    /// backend too (seq is assigned client-side). No chat data is read or
    /// written through it.
    lock_store: FileConversationStore,
}

impl PluginConversationStore {
    fn new(
        plugin: DiscoveredPlugin,
        project_root: PathBuf,
        acting_user: Option<String>,
        lock_store: FileConversationStore,
    ) -> Self {
        Self { plugin, project_root, acting_user, lock_store }
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

    fn acting_user(&self) -> Option<String> {
        self.acting_user.clone()
    }

    fn scope(&self) -> proto::ConversationScope {
        proto::ConversationScope {
            project_root: Some(self.project_root.to_string_lossy().into_owned()),
            repo_scope: Some(protocol::repository_scope_for_path(&self.project_root)),
        }
    }

    fn create(
        &self,
        id: Option<String>,
        owner: Option<String>,
        visibility: super::store::Visibility,
    ) -> Result<ConversationMeta> {
        let request = proto::ConversationCreateRequest {
            scope: self.scope(),
            id,
            owner,
            visibility: to_proto_visibility(visibility),
        };
        let resp: proto::ConversationCreateResponse = self.call(proto::METHOD_CONVERSATION_CREATE, &request)?;
        from_proto_meta(resp.meta)
    }

    fn load_meta(&self, id: &str) -> Result<Option<ConversationMeta>> {
        let request =
            proto::ConversationLoadMetaRequest { scope: self.scope(), id: id.to_string(), as_user: self.acting_user() };
        let resp: proto::ConversationLoadMetaResponse = self.call(proto::METHOD_CONVERSATION_LOAD_META, &request)?;
        resp.meta.map(from_proto_meta).transpose()
    }

    fn save_meta(&self, meta: &ConversationMeta) -> Result<()> {
        let request = proto::ConversationSaveMetaRequest {
            scope: self.scope(),
            meta: to_proto_meta(meta)?,
            as_user: self.acting_user(),
        };
        let _: proto::ConversationSaveMetaResponse = self.call(proto::METHOD_CONVERSATION_SAVE_META, &request)?;
        Ok(())
    }

    fn append_message(&self, id: &str, message: &ChatMessage) -> Result<()> {
        let request = proto::ConversationAppendMessageRequest {
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
        let request = proto::ConversationLoadMessagesRequest {
            scope: self.scope(),
            id: id.to_string(),
            as_user: self.acting_user(),
        };
        let resp: proto::ConversationLoadMessagesResponse =
            self.call(proto::METHOD_CONVERSATION_LOAD_MESSAGES, &request)?;
        resp.messages.into_iter().map(from_proto_message).collect()
    }

    fn list(&self, as_user: Option<&str>) -> Result<Vec<ConversationSummary>> {
        let request = proto::ConversationListRequest { scope: self.scope(), as_user: as_user.map(ToOwned::to_owned) };
        let resp: proto::ConversationListResponse = self.call(proto::METHOD_CONVERSATION_LIST, &request)?;
        Ok(resp.conversations.into_iter().map(from_proto_summary).collect())
    }

    fn delete(&self, id: &str) -> Result<()> {
        let request =
            proto::ConversationDeleteRequest { scope: self.scope(), id: id.to_string(), as_user: self.acting_user() };
        let _: proto::ConversationDeleteResponse = self.call(proto::METHOD_CONVERSATION_DELETE, &request)?;
        Ok(())
    }

    /// Spawn the plugin host, handshake, run one RPC, and ALWAYS reap the host.
    fn call<Req: serde::Serialize, Resp: serde::de::DeserializeOwned>(
        &self,
        method: &str,
        request: &Req,
    ) -> Result<Resp> {
        let params = serde_json::to_value(request).context("serializing conversation_store request")?;
        run_blocking(self.call_async(method, params))?
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

fn to_proto_visibility(visibility: super::store::Visibility) -> proto::Visibility {
    match visibility {
        super::store::Visibility::Private => proto::Visibility::Private,
        super::store::Visibility::Shared => proto::Visibility::Shared,
    }
}

fn from_proto_visibility(visibility: proto::Visibility) -> super::store::Visibility {
    match visibility {
        proto::Visibility::Private => super::store::Visibility::Private,
        proto::Visibility::Shared => super::store::Visibility::Shared,
    }
}

/// Convert a kernel meta into the protocol shape. The two structs are
/// field-identical except `Visibility`, so a JSON round-trip is the cheapest
/// faithful conversion and keeps the two in lockstep.
fn to_proto_meta(meta: &ConversationMeta) -> Result<proto::ConversationMeta> {
    serde_json::from_value(serde_json::to_value(meta).context("serializing ConversationMeta")?)
        .context("converting ConversationMeta to protocol shape")
}

fn from_proto_meta(meta: proto::ConversationMeta) -> Result<ConversationMeta> {
    serde_json::from_value(serde_json::to_value(meta).context("serializing protocol ConversationMeta")?)
        .context("converting protocol ConversationMeta to kernel shape")
}

fn to_proto_message(message: &ChatMessage) -> Result<proto::ChatMessage> {
    serde_json::from_value(serde_json::to_value(message).context("serializing ChatMessage")?)
        .context("converting ChatMessage to protocol shape")
}

fn from_proto_message(message: proto::ChatMessage) -> Result<ChatMessage> {
    serde_json::from_value(serde_json::to_value(message).context("serializing protocol ChatMessage")?)
        .context("converting protocol ChatMessage to kernel shape")
}

fn from_proto_summary(summary: proto::ConversationSummary) -> ConversationSummary {
    ConversationSummary {
        id: summary.id,
        title: summary.title,
        tool: summary.tool,
        model: summary.model,
        message_count: summary.message_count,
        updated_at: summary.updated_at,
        owner: summary.owner,
        visibility: from_proto_visibility(summary.visibility),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::runtime::runtime_chat::store::Visibility;

    fn summary(id: &str, owner: Option<&str>, visibility: Visibility) -> ConversationSummary {
        ConversationSummary {
            id: id.to_string(),
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
    fn meta_round_trips_through_proto_shape() {
        let mut meta = ConversationMeta {
            id: "conv-x".to_string(),
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
        let proto = to_proto_meta(&meta).unwrap();
        assert_eq!(proto.owner.as_deref(), Some("u1"));
        assert_eq!(proto.visibility, proto::Visibility::Shared);
        let back = from_proto_meta(proto).unwrap();
        // updated_at is the same across the round-trip; whole struct matches.
        meta.updated_at = back.updated_at.clone();
        assert_eq!(back.id, meta.id);
        assert_eq!(back.owner, meta.owner);
        assert_eq!(back.visibility, meta.visibility);
        assert_eq!(back.session_id, meta.session_id);
    }
}
