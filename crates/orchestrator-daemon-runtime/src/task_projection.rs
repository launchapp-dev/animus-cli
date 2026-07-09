//! Router-backed [`TaskProjectionStore`] for the daemon / CLI layer.
//!
//! The execution-projection layer in `orchestrator-core` writes task status +
//! annotations through the [`TaskProjectionStore`] trait. On a plugin-backed /
//! portal deployment the legacy in-tree store (`hub.tasks()`) is EMPTY — tasks
//! live in the installed `subject_backend` plugin — so projecting a terminal
//! status into it left the real subject stuck `InProgress`.
//!
//! [`RouterTaskProjectionStore`] routes those writes through the same
//! `SubjectPluginDispatch` the rest of the subject surface uses
//! (`task/status`, `task/update`, `task/get`), and
//! [`resolve_task_projection_store`] selects it only when a backend actually
//! owns `task`, falling back to the in-tree store otherwise. This clones the
//! already-correct `RouterStaleTaskStore` pattern.

use std::sync::Arc;

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde_json::{json, Value};

use orchestrator_core::services::ServiceHub;
use orchestrator_core::{HubTaskProjectionStore, TaskProjectionStore, TaskProjectionView, TaskStatus};

use crate::subject_dispatch::{resolve_subject_dispatch, SubjectPluginDispatch};

/// The built-in `task` kind and the catch-all wildcard the router also serves
/// `task/*` through.
const TASK_KIND: &str = "task";
const CATCH_ALL_KIND: &str = "*";

/// Subject-router-backed task projection store. Every write routes
/// `task/<verb>` through the installed `subject_backend` plugin.
pub struct RouterTaskProjectionStore {
    dispatch: SubjectPluginDispatch,
}

impl RouterTaskProjectionStore {
    #[must_use]
    pub fn new(dispatch: SubjectPluginDispatch) -> Self {
        Self { dispatch }
    }

    /// `true` when a `subject_backend` plugin can serve the built-in `task`
    /// kind (an explicit `task` kind or a `*` catch-all), so routing the
    /// projections through it is meaningful.
    #[must_use]
    pub fn routes_tasks(dispatch: &SubjectPluginDispatch) -> bool {
        dispatch.is_active() && dispatch.kinds().iter().any(|kind| kind == TASK_KIND || kind == CATCH_ALL_KIND)
    }

    async fn route(&self, method: &str, params: Value) -> Result<Value> {
        self.dispatch
            .route_call(method, Some(params))
            .await
            .map_err(|err| anyhow!("subject backend {method} failed ({}): {}", err.code, err.message))
    }

    /// Best-effort `task/update` patch for informational fields. A backend that
    /// does not model the patched field (e.g. `blocked_reason`) must not fail
    /// the projection — the load-bearing status transition already landed via
    /// `task/status`.
    async fn best_effort_update(&self, id: &str, patch: Value) {
        if let Err(error) = self.route("task/update", json!({ "id": id, "patch": patch })).await {
            tracing::debug!(task_id = %id, error = %error, "best-effort task/update annotation skipped");
        }
    }
}

#[async_trait]
impl TaskProjectionStore for RouterTaskProjectionStore {
    async fn get(&self, id: &str) -> Result<TaskProjectionView> {
        let id = qualify_task_id(id);
        let raw = self.route("task/get", json!({ "id": id })).await?;
        let subject =
            subject_object(&raw).ok_or_else(|| anyhow!("subject backend task/get returned no subject for '{id}'"))?;
        Ok(TaskProjectionView {
            status: parse_status(subject).unwrap_or(TaskStatus::InProgress),
            blocked_reason: field_str(subject, "blocked_reason"),
            blocked_by: field_str(subject, "blocked_by"),
        })
    }

    async fn set_status(&self, id: &str, status: TaskStatus) -> Result<()> {
        let id = qualify_task_id(id);
        self.route("task/status", json!({ "id": &id, "status": wire_status(status) })).await?;
        // Mirror the in-tree `apply_task_status`: any NON-blocked transition
        // clears the blocked/paused bookkeeping. Without this, the `custom`
        // fields written by `block_with_reason` / pause annotations would linger
        // on the plugin-backed subject after e.g. a cancel or a stale-reset to
        // Ready. Best-effort: a backend that clears them itself just no-ops.
        if !status.is_blocked() {
            self.best_effort_update(&id, json!({ "custom": clear_block_bookkeeping() })).await;
        }
        Ok(())
    }

    async fn block_with_reason(&self, id: &str, reason: String, blocked_by: Option<String>) -> Result<()> {
        let id = qualify_task_id(id);
        // Status is the load-bearing fix: propagate its error. The annotation
        // fields are best-effort so a backend that does not model them still
        // gets the terminal Blocked transition.
        self.route("task/status", json!({ "id": &id, "status": wire_status(TaskStatus::Blocked) })).await?;
        // `blocked_reason` / `blocked_by` / `paused` are not top-level
        // `SubjectPatch` fields — carry them in the patch's `custom` map (the
        // read side already looks there). An explicit `null` clears the key.
        let custom = json!({
            "blocked_reason": reason,
            "blocked_by": blocked_by.map(Value::String).unwrap_or(Value::Null),
            "paused": true,
        });
        self.best_effort_update(&id, json!({ "custom": custom })).await;
        Ok(())
    }

    async fn annotate_blocked_reason(&self, id: &str, reason: String, set_blocked_by: Option<String>) -> Result<()> {
        let id = qualify_task_id(id);
        // Informational only (no status change). Route the annotation through
        // the patch's `custom` map; a backend that does not model it degrades
        // to a no-op.
        let mut custom = serde_json::Map::new();
        custom.insert("blocked_reason".to_string(), json!(reason));
        if let Some(blocked_by) = set_blocked_by {
            custom.insert("blocked_by".to_string(), json!(blocked_by));
        }
        self.best_effort_update(&id, json!({ "custom": Value::Object(custom) })).await;
        Ok(())
    }

    async fn clear_blocked_reason(&self, id: &str, clear_blocked_by: bool) -> Result<()> {
        let id = qualify_task_id(id);
        // An explicit JSON `null` in the patch's `custom` map clears the field.
        let mut custom = serde_json::Map::new();
        custom.insert("blocked_reason".to_string(), Value::Null);
        if clear_blocked_by {
            custom.insert("blocked_by".to_string(), Value::Null);
        }
        self.best_effort_update(&id, json!({ "custom": Value::Object(custom) })).await;
        Ok(())
    }

    async fn start_workflow(&self, id: &str, role: String, model: Option<String>, _updated_by: String) -> Result<()> {
        let id = qualify_task_id(id);
        // The InProgress transition is the load-bearing fix (finding 2); the
        // agent assignment is best-effort annotation. `assignee` is a top-level
        // `SubjectPatch` field; the model lands in `custom`. InProgress is a
        // non-blocked status, so also clear any lingering blocked/paused
        // bookkeeping (matching the in-tree `apply_task_status`).
        self.route("task/status", json!({ "id": &id, "status": wire_status(TaskStatus::InProgress) })).await?;
        let mut custom = clear_block_bookkeeping();
        custom.insert("assignee_model".to_string(), json!(model));
        self.best_effort_update(&id, json!({ "assignee": format!("agent:{role}"), "custom": custom })).await;
        Ok(())
    }
}

/// Choose the task projection store: the subject router when a `subject_backend`
/// plugin owns `task` (production / portal), else the in-tree store (stock
/// scaffold / no plugins). A routing-discovery failure falls back to the
/// in-tree store so a transient plugin problem never takes the projection down.
pub async fn resolve_task_projection_store(root: &str, hub: Arc<dyn ServiceHub>) -> Box<dyn TaskProjectionStore> {
    match resolve_subject_dispatch(std::path::Path::new(root)).await {
        Ok(resolution) if RouterTaskProjectionStore::routes_tasks(&resolution.selected) => {
            Box::new(RouterTaskProjectionStore::new(resolution.selected))
        }
        _ => Box::new(HubTaskProjectionStore::new(hub)),
    }
}

/// Qualify a task id to the `task:<native>` form the subject router / backend
/// keys subjects under. Projection callers hand us the workflow's `task_id`,
/// which may be the BARE native id (`TASK-1`, e.g. from `workflow run
/// --task-id`) or already qualified (`task:TASK-1`); routing a bare id through
/// `task/get` / `task/status` misses or is rejected by the backend. An id
/// already carrying a `<kind>:` qualifier is passed through unchanged.
fn qualify_task_id(id: &str) -> String {
    if id.contains(':') {
        id.to_string()
    } else {
        format!("{TASK_KIND}:{id}")
    }
}

/// `custom` patch that clears the blocked/paused bookkeeping this store writes
/// (`blocked_reason`, `blocked_by`, `paused`). An explicit JSON `null` clears a
/// custom field; `paused` is reset to `false`. Mirrors the in-tree
/// `apply_task_status` cleanup on a non-blocked transition.
fn clear_block_bookkeeping() -> serde_json::Map<String, Value> {
    let mut custom = serde_json::Map::new();
    custom.insert("blocked_reason".to_string(), Value::Null);
    custom.insert("blocked_by".to_string(), Value::Null);
    custom.insert("paused".to_string(), Value::Bool(false));
    custom
}

/// Wire status token the subject_backend plugin expects. `TaskStatus`
/// serializes kebab-case, matching the `SubjectStatus` vocabulary.
fn wire_status(status: TaskStatus) -> String {
    serde_json::to_value(status).ok().and_then(|value| value.as_str().map(str::to_string)).unwrap_or_default()
}

/// Unwrap a `<kind>/get` response into the subject object (top-level or
/// `{ "subject": { ... } }` wrapped).
fn subject_object(value: &Value) -> Option<&serde_json::Map<String, Value>> {
    if let Some(inner) = value.get("subject").and_then(Value::as_object) {
        return Some(inner);
    }
    value.as_object().filter(|map| map.contains_key("id"))
}

/// Parse the subject's `status` string into a [`TaskStatus`], tolerating
/// `in_progress` / casing variants.
fn parse_status(subject: &serde_json::Map<String, Value>) -> Option<TaskStatus> {
    let raw = subject.get("status").and_then(Value::as_str)?;
    let normalized = raw.trim().to_ascii_lowercase().replace('_', "-");
    serde_json::from_value::<TaskStatus>(json!(normalized)).ok()
}

/// Read a string field from the subject object, checking the top level first
/// then a nested `custom` map (where some backends persist annotations).
fn field_str(subject: &serde_json::Map<String, Value>, key: &str) -> Option<String> {
    subject
        .get(key)
        .and_then(Value::as_str)
        .or_else(|| subject.get("custom").and_then(|c| c.get(key)).and_then(Value::as_str))
        .map(str::to_string)
        .filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_status_is_kebab_case() {
        assert_eq!(wire_status(TaskStatus::InProgress), "in-progress");
        assert_eq!(wire_status(TaskStatus::Blocked), "blocked");
        assert_eq!(wire_status(TaskStatus::Cancelled), "cancelled");
        assert_eq!(wire_status(TaskStatus::Ready), "ready");
    }

    #[test]
    fn subject_object_unwraps_both_shapes() {
        let wrapped = json!({ "subject": { "id": "task:T1", "status": "blocked" } });
        assert!(subject_object(&wrapped).unwrap().contains_key("status"));
        let bare = json!({ "id": "task:T1", "status": "in-progress" });
        assert!(subject_object(&bare).unwrap().contains_key("status"));
        assert!(subject_object(&json!({ "ok": true })).is_none());
    }

    #[test]
    fn parse_status_tolerates_underscore_and_case() {
        let s = json!({ "status": "In_Progress" });
        assert_eq!(parse_status(s.as_object().unwrap()), Some(TaskStatus::InProgress));
    }

    #[test]
    fn field_str_reads_top_level_and_custom() {
        let top = json!({ "blocked_reason": "boom" });
        assert_eq!(field_str(top.as_object().unwrap(), "blocked_reason"), Some("boom".to_string()));
        let nested = json!({ "custom": { "blocked_by": "wf-1" } });
        assert_eq!(field_str(nested.as_object().unwrap(), "blocked_by"), Some("wf-1".to_string()));
        let empty = json!({ "blocked_reason": "" });
        assert_eq!(field_str(empty.as_object().unwrap(), "blocked_reason"), None);
    }

    #[test]
    fn routes_tasks_requires_active_dispatch_owning_task() {
        // An empty dispatch never routes tasks (stock scaffold / no plugins).
        assert!(!RouterTaskProjectionStore::routes_tasks(&SubjectPluginDispatch::empty()));
    }

    // --- Router-routing integration tests ------------------------------------

    use animus_plugin_protocol::{
        InitializeResult, PluginCapabilities, PluginInfo, RpcRequest, RpcResponse, PLUGIN_KIND_SUBJECT_BACKEND,
    };
    use orchestrator_plugin_host::{PluginHost, SubjectRouter};
    use std::collections::HashMap;
    use std::sync::Mutex;
    use tokio::io::{duplex, AsyncBufReadExt, AsyncWriteExt, BufReader};

    /// One routed call the recording backend observed: `(method, id)`.
    type RoutedCall = (String, String);

    /// A fake `task` subject backend that records every routed `(method, id)`
    /// and answers `task/get` with a minimal in-progress subject.
    async fn recording_task_host(recorded: Arc<Mutex<Vec<RoutedCall>>>) -> PluginHost {
        let (host_reader, mut plugin_writer) = duplex(8192);
        let (plugin_reader, host_writer) = duplex(8192);

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
                                name: "tasks".to_string(),
                                version: "0.1.0".to_string(),
                                plugin_kind: PLUGIN_KIND_SUBJECT_BACKEND.to_string(),
                                plugin_kinds: vec![],
                                description: None,
                            },
                            capabilities: PluginCapabilities {
                                subject_kinds: vec!["task".to_string()],
                                methods: vec![
                                    "task/get".to_string(),
                                    "task/status".to_string(),
                                    "task/update".to_string(),
                                ],
                                ..PluginCapabilities::default()
                            },
                            kind_capabilities: std::collections::HashMap::new(),
                        }),
                    ),
                    "initialized" => continue,
                    method => {
                        let routed_id = request
                            .params
                            .as_ref()
                            .and_then(|p| p.get("id"))
                            .and_then(|v| v.as_str())
                            .unwrap_or_default()
                            .to_string();
                        recorded.lock().unwrap().push((method.to_string(), routed_id));
                        let body = if method == "task/get" {
                            serde_json::json!({ "id": "task:TASK-1", "kind": "task", "status": "in-progress" })
                        } else {
                            serde_json::json!({ "ok": true })
                        };
                        RpcResponse::ok(request.id, body)
                    }
                };
                let mut encoded = serde_json::to_string(&response).expect("encode response");
                encoded.push('\n');
                plugin_writer.write_all(encoded.as_bytes()).await.expect("write response");
            }
        });

        PluginHost::from_streams("tasks", host_reader, host_writer)
    }

    async fn router_store_over_task_backend(recorded: Arc<Mutex<Vec<RoutedCall>>>) -> RouterTaskProjectionStore {
        let mut hosts = HashMap::new();
        hosts.insert("tasks".to_string(), recording_task_host(recorded).await);
        let router = SubjectRouter::from_initialized_hosts(hosts).await.expect("router builds");
        let dispatch = SubjectPluginDispatch::from_router(router, vec!["task".to_string()], 1);
        assert!(RouterTaskProjectionStore::routes_tasks(&dispatch), "a backend owning `task` must route");
        RouterTaskProjectionStore::new(dispatch)
    }

    fn methods(recorded: &Arc<Mutex<Vec<RoutedCall>>>) -> Vec<String> {
        recorded.lock().unwrap().iter().map(|(method, _)| method.clone()).collect()
    }

    #[tokio::test]
    async fn set_status_to_non_blocked_clears_bookkeeping() {
        let recorded = Arc::new(Mutex::new(Vec::new()));
        let store = router_store_over_task_backend(recorded.clone()).await;

        store.set_status("task:TASK-1", TaskStatus::Cancelled).await.expect("status routes");

        // A non-blocked transition routes the status AND a clear-annotation
        // update, matching the in-tree `apply_task_status` cleanup.
        assert_eq!(methods(&recorded), vec!["task/status".to_string(), "task/update".to_string()]);
    }

    #[tokio::test]
    async fn set_status_to_blocked_does_not_emit_clear_update() {
        let recorded = Arc::new(Mutex::new(Vec::new()));
        let store = router_store_over_task_backend(recorded.clone()).await;

        store.set_status("task:TASK-1", TaskStatus::Blocked).await.expect("status routes");

        // Blocking must NOT clear the bookkeeping it (or a sibling projection)
        // is establishing.
        assert_eq!(methods(&recorded), vec!["task/status".to_string()]);
    }

    #[tokio::test]
    async fn bare_task_id_is_qualified_before_routing() {
        // The load-bearing P1 fix: a bare native id (as carried by
        // `workflow run --task-id TASK-1`) must be qualified to `task:TASK-1`
        // before it hits the backend, or `task/status` misses the subject.
        let recorded = Arc::new(Mutex::new(Vec::new()));
        let store = router_store_over_task_backend(recorded.clone()).await;

        store.set_status("TASK-1", TaskStatus::InProgress).await.expect("status routes");

        // Every routed call carries the qualified id.
        assert!(
            recorded.lock().unwrap().iter().all(|(_, id)| id == "task:TASK-1"),
            "all routed ids must be qualified: {:?}",
            recorded.lock().unwrap()
        );
        assert_eq!(recorded.lock().unwrap()[0], ("task/status".to_string(), "task:TASK-1".to_string()));
    }

    #[tokio::test]
    async fn block_with_reason_routes_status_then_annotation() {
        let recorded = Arc::new(Mutex::new(Vec::new()));
        let store = router_store_over_task_backend(recorded.clone()).await;

        store.block_with_reason("task:TASK-1", "boom".to_string(), Some("wf-1".to_string())).await.expect("blocks");

        // The load-bearing status transition routes first, the annotation second.
        assert_eq!(methods(&recorded), vec!["task/status".to_string(), "task/update".to_string()]);
    }

    #[tokio::test]
    async fn get_reads_the_task_backend_status() {
        let recorded = Arc::new(Mutex::new(Vec::new()));
        let store = router_store_over_task_backend(recorded.clone()).await;

        let view = store.get("task:TASK-1").await.expect("get routes");
        assert_eq!(view.status, TaskStatus::InProgress);
        assert_eq!(methods(&recorded), vec!["task/get".to_string()]);
    }

    #[tokio::test]
    async fn start_workflow_routes_in_progress_transition() {
        let recorded = Arc::new(Mutex::new(Vec::new()));
        let store = router_store_over_task_backend(recorded.clone()).await;

        store.start_workflow("task:TASK-1", "impl".to_string(), None, "daemon".to_string()).await.expect("starts");

        assert_eq!(methods(&recorded), vec!["task/status".to_string(), "task/update".to_string()]);
    }

    #[test]
    fn qualify_task_id_adds_task_prefix_to_bare_ids_only() {
        assert_eq!(qualify_task_id("TASK-1"), "task:TASK-1");
        assert_eq!(qualify_task_id("task:TASK-1"), "task:TASK-1");
        // An already-qualified id of any kind is passed through unchanged.
        assert_eq!(qualify_task_id("blog:BLOG-1"), "blog:BLOG-1");
    }

    #[tokio::test]
    async fn resolve_falls_back_to_in_tree_store_when_no_backend_owns_task() {
        use orchestrator_core::{InMemoryServiceHub, Priority, TaskCreateInput, TaskStatus, TaskType};
        use protocol::test_utils::EnvVarGuard;

        // Force discovery to surface no subject backend, so the resolver must
        // fall back to the in-tree store.
        let _disable = EnvVarGuard::set(crate::SUBJECT_PLUGINS_DISABLE_ENV, Some("1"));
        let temp = tempfile::tempdir().expect("temp dir");
        let root = temp.path().to_string_lossy().to_string();

        let hub: Arc<dyn ServiceHub> = Arc::new(InMemoryServiceHub::new());
        let task = hub
            .tasks()
            .create(TaskCreateInput {
                title: "fallback".to_string(),
                description: "in-tree fallback".to_string(),
                task_type: Some(TaskType::Feature),
                priority: Some(Priority::Medium),
                created_by: Some("test".to_string()),
                tags: Vec::new(),
                linked_requirements: Vec::new(),
                linked_architecture_entities: Vec::new(),
            })
            .await
            .expect("create task");

        let store = resolve_task_projection_store(&root, hub.clone()).await;
        store.set_status(&task.id, TaskStatus::Blocked).await.expect("fallback write");

        // The write landed in the in-tree hub — proof the fallback store is
        // hub-backed, not a no-op / plugin route.
        let after = hub.tasks().get(&task.id).await.expect("task still present");
        assert_eq!(after.status, TaskStatus::Blocked);
    }
}
