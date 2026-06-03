//! Production [`DurableStoreClient`] impl backed by the
//! `launchapp-dev/animus-step-durable-dbos` plugin.
//!
//! The plugin speaks the v0.5 `durable_store` JSON-RPC surface
//! (`durable/begin_workflow_run`, `durable/begin_step`,
//! `durable/commit_step`, `durable/abandon_step`, `durable/query_run`)
//! defined in `docs/architecture/v0.5-protocol-specs.md` §3. The
//! `animus-durable-store-protocol` Rust crate is not yet pulled into
//! this workspace; we inline minimal request/response types here so the
//! agent-runner can integrate without an upstream version bump.
//!
//! Lifecycle:
//!
//! - One [`DbosDurableStoreClient`] binds to one project root + workflow id.
//! - It spawns the DBOS plugin (via [`orchestrator_plugin_host`]) on first
//!   use and reuses the [`PluginHost`] for subsequent RPCs.
//! - [`DbosDurableStoreClient::shutdown`] terminates the plugin process
//!   cleanly. Drop is best-effort.
//!
//! Idempotency-key shape: `<repo_scope>:<workflow_id>:<phase_id>:<call_index>`
//! (encoded by [`super::fence::IdempotencyKey`]). The plugin uses the
//! full key string for dedupe; the agent-runner additionally parses out
//! `phase_id` + `call_index` to populate the structured `begin_step`
//! request fields.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use animus_plugin_protocol::EnvRequirement;
use anyhow::{anyhow, Context, Result};
use orchestrator_plugin_host::{
    discover_by_kind, DiscoveredPlugin, PluginHost, PluginSpawnOptions, PLUGIN_BASE_ENV_ALLOWLIST,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::Mutex;

use super::fence::{DurableStoreClient, FenceState, IdempotencyKey};

const PLUGIN_KIND_DURABLE_STORE: &str = "durable_store";

const METHOD_DURABLE_BEGIN_WORKFLOW_RUN: &str = "durable/begin_workflow_run";
const METHOD_DURABLE_BEGIN_STEP: &str = "durable/begin_step";
const METHOD_DURABLE_COMMIT_STEP: &str = "durable/commit_step";
const METHOD_DURABLE_ABANDON_STEP: &str = "durable/abandon_step";
const METHOD_DURABLE_QUERY_RUN: &str = "durable/query_run";

mod step_status {
    pub const NEW: &str = "new";
    pub const IN_PROGRESS: &str = "in_progress";
    pub const ALREADY_COMMITTED: &str = "already_committed";
    pub const PRIOR_ERROR: &str = "prior_error";
}

mod commit_outcome {
    pub const SUCCESS: &str = "success";
    pub const ERROR: &str = "error";
}

const PLUGIN_INIT_TIMEOUT_SECS: u64 = 30;
const STEP_RPC_TIMEOUT_SECS: u64 = 60;

#[derive(Debug, Serialize)]
struct BeginWorkflowRunRequest<'a> {
    run_id: &'a str,
    phase_id: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    inputs: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct BeginWorkflowRunResponse {
    #[allow(dead_code)]
    epoch: u64,
}

#[derive(Debug, Serialize)]
struct BeginStepRequest<'a> {
    run_id: &'a str,
    phase_id: &'a str,
    step_name: &'a str,
    idempotency_key: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    payload: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reservation_ttl_secs: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct BeginStepResponse {
    step_id: String,
    status: String,
    #[serde(default)]
    prior_output: Option<Value>,
    #[serde(default)]
    prior_error: Option<StepError>,
    #[serde(default)]
    #[allow(dead_code)]
    reservation_expires_at: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
struct StepError {
    #[allow(dead_code)]
    code: String,
    message: String,
    #[serde(default)]
    #[allow(dead_code)]
    details: Value,
}

#[derive(Debug, Serialize)]
struct CommitStepRequest<'a> {
    step_id: &'a str,
    outcome: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    output: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<StepErrorPayload>,
}

#[derive(Debug, Serialize)]
struct StepErrorPayload {
    code: String,
    message: String,
}

#[derive(Debug, Serialize)]
struct AbandonStepRequest<'a> {
    step_id: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
}

#[derive(Debug, Serialize)]
struct QueryRunRequest<'a> {
    run_id: &'a str,
    phase_id: &'a str,
}

#[derive(Debug, Deserialize)]
struct QueryRunResponse {
    #[allow(dead_code)]
    run_id: String,
    #[allow(dead_code)]
    status: String,
    #[serde(default)]
    steps: Vec<StepRecord>,
}

#[derive(Debug, Deserialize)]
struct StepRecord {
    #[allow(dead_code)]
    step_id: String,
    #[allow(dead_code)]
    step_name: String,
    idempotency_key: String,
    #[allow(dead_code)]
    #[serde(default)]
    committed_at: Option<String>,
    outcome: String,
    #[serde(default)]
    output: Option<Value>,
    #[serde(default)]
    error: Option<StepError>,
}

/// Production-facing [`DurableStoreClient`] backed by a long-running
/// `animus-step-durable-dbos` plugin process.
///
/// The plugin process is bound to one `(project_root, run_id, phase_id)`
/// tuple at construction. Reusing the client across runs is not supported;
/// callers spawn one per workflow run.
pub struct DbosDurableStoreClient {
    inner: Arc<DbosInner>,
}

struct DbosInner {
    project_root: PathBuf,
    run_id: String,
    phase_id: String,
    host: Mutex<Option<PluginHost>>,
    plugin_path: PathBuf,
    plugin_label: String,
    env_required: Vec<EnvRequirement>,
    workflow_run_initialized: Mutex<bool>,
}

impl DbosDurableStoreClient {
    /// Discover the installed `durable_store` plugin under `project_root`
    /// and prepare a client bound to `run_id` + `phase_id`. The plugin
    /// process is not spawned until the first RPC.
    pub fn discover(project_root: &Path, run_id: &str, phase_id: &str) -> Result<Option<Self>> {
        let discovered = discover_by_kind(project_root.to_path_buf(), PLUGIN_KIND_DURABLE_STORE)
            .context("durable_store plugin discovery failed")?;
        let Some(plugin) = discovered.into_iter().next() else {
            return Ok(None);
        };
        Ok(Some(Self::new_for_discovered(project_root.to_path_buf(), plugin, run_id.to_string(), phase_id.to_string())))
    }

    fn new_for_discovered(project_root: PathBuf, plugin: DiscoveredPlugin, run_id: String, phase_id: String) -> Self {
        Self {
            inner: Arc::new(DbosInner {
                project_root,
                run_id,
                phase_id,
                host: Mutex::new(None),
                plugin_path: plugin.path,
                plugin_label: plugin.name,
                env_required: plugin.manifest.env_required,
                workflow_run_initialized: Mutex::new(false),
            }),
        }
    }

    /// Best-effort shutdown of the underlying plugin process. After
    /// `shutdown` further calls to `step_*` will re-spawn the plugin.
    pub async fn shutdown(&self) {
        // Lock order: host BEFORE init. `ensure_workflow_run` mirrors this
        // ordering (see below) so the AB-BA deadlock codex flagged for v0.5.1
        // is ruled out by construction. We also hold host across the init
        // reset so a concurrent step can't slip in, re-init on the old host,
        // and have its init bit silently survive our shutdown.
        let mut host_guard = self.inner.host.lock().await;
        let mut init_guard = self.inner.workflow_run_initialized.lock().await;
        *init_guard = false;
        if let Some(host) = host_guard.take() {
            if let Err(err) = host.shutdown().await {
                tracing::warn!(plugin = %self.inner.plugin_label, %err, "durable_store plugin shutdown errored");
            }
        }
    }

    async fn ensure_host(&self) -> Result<()> {
        let mut guard = self.inner.host.lock().await;
        if guard.is_some() {
            return Ok(());
        }
        let options = PluginSpawnOptions::for_manifest(
            self.inner.plugin_label.clone(),
            &self.inner.env_required,
            PLUGIN_BASE_ENV_ALLOWLIST.iter().map(|s| (*s).to_string()),
            None,
        )
        .with_working_dir(self.inner.project_root.clone());

        let host = PluginHost::spawn_with_options(&self.inner.plugin_path, &[], options)
            .await
            .with_context(|| format!("spawn durable_store plugin '{}'", self.inner.plugin_label))?;

        let repo_scope = protocol::repository_scope_for_path(&self.inner.project_root);
        let init_params = json!({
            "protocol_version": "1.1.0",
            "host_info": { "name": "animus-agent-runner", "version": env!("CARGO_PKG_VERSION") },
            "capabilities": { "streaming": false, "progress": false, "cancellation": true },
            "init_extensions": {
                "project_binding": {
                    "project_root": self.inner.project_root.to_string_lossy(),
                    "repo_scope": repo_scope,
                },
            },
        });
        host.request_typed_with_timeout(
            "initialize",
            Some(init_params),
            std::time::Duration::from_secs(PLUGIN_INIT_TIMEOUT_SECS),
        )
        .await
        .with_context(|| format!("durable_store plugin '{}' initialize failed", self.inner.plugin_label))?;
        host.notify("initialized", None)
            .await
            .with_context(|| format!("durable_store plugin '{}' initialized notification failed", self.inner.plugin_label))?;
        *guard = Some(host);
        Ok(())
    }

    async fn ensure_workflow_run(&self) -> Result<()> {
        // ensure_host briefly takes/releases host; after it returns the
        // host slot is populated. We then re-take host, then init, so the
        // critical section that runs begin_workflow_run holds locks in the
        // same order as `shutdown` (host BEFORE init). Without this order,
        // shutdown can race a step and either deadlock or leave the init
        // bit set across a host swap.
        self.ensure_host().await?;
        let host_guard = self.inner.host.lock().await;
        let mut init_guard = self.inner.workflow_run_initialized.lock().await;
        if *init_guard {
            return Ok(());
        }
        let host = host_guard.as_ref().ok_or_else(|| anyhow!("durable_store host not initialized after ensure_host"))?;
        let req = BeginWorkflowRunRequest { run_id: &self.inner.run_id, phase_id: &self.inner.phase_id, inputs: None };
        let params = serde_json::to_value(&req).context("encode BeginWorkflowRunRequest")?;
        let value = host
            .request_typed_with_timeout(
                METHOD_DURABLE_BEGIN_WORKFLOW_RUN,
                Some(params),
                std::time::Duration::from_secs(STEP_RPC_TIMEOUT_SECS),
            )
            .await
            .context("durable_store begin_workflow_run failed")?;
        let _: BeginWorkflowRunResponse =
            serde_json::from_value(value).context("decode BeginWorkflowRunResponse")?;
        *init_guard = true;
        Ok(())
    }

    async fn call_method<T: for<'de> Deserialize<'de>>(&self, method: &str, params: Value) -> Result<T> {
        self.ensure_host().await?;
        let host_guard = self.inner.host.lock().await;
        let host = host_guard.as_ref().ok_or_else(|| anyhow!("durable_store host not initialized"))?;
        let value = host
            .request_typed_with_timeout(
                method,
                Some(params),
                std::time::Duration::from_secs(STEP_RPC_TIMEOUT_SECS),
            )
            .await
            .with_context(|| format!("durable_store {method} failed"))?;
        serde_json::from_value(value).with_context(|| format!("decode {method} response"))
    }

    fn step_name_for_key(key: &IdempotencyKey) -> &str {
        let s = key.as_str();
        match s.rsplit_once(':') {
            Some((_, suffix)) => suffix,
            None => s,
        }
    }
}

#[async_trait::async_trait]
impl DurableStoreClient for DbosDurableStoreClient {
    async fn step_query(&self, key: &IdempotencyKey) -> Result<FenceState> {
        self.ensure_workflow_run().await?;
        let params = serde_json::to_value(QueryRunRequest {
            run_id: &self.inner.run_id,
            phase_id: &self.inner.phase_id,
        })
        .context("encode QueryRunRequest")?;
        let resp: QueryRunResponse = self.call_method(METHOD_DURABLE_QUERY_RUN, params).await?;
        for step in resp.steps {
            if step.idempotency_key == key.as_str() {
                return Ok(match step.outcome.as_str() {
                    commit_outcome::SUCCESS => FenceState::PriorSuccess { response: step.output.unwrap_or(Value::Null) },
                    commit_outcome::ERROR => {
                        let msg = step
                            .error
                            .map(|e| e.message)
                            .unwrap_or_else(|| "(no error message captured)".to_string());
                        FenceState::PriorError { message: msg }
                    }
                    other => {
                        return Err(anyhow!("durable_store returned unknown step outcome `{}` for key {}", other, key));
                    }
                });
            }
        }
        // Not yet in the commit log. Issue a no-side-effect probe via
        // `begin_step` and immediately abandon if NEW so the query stays
        // read-only from the caller's perspective.
        let params = serde_json::to_value(BeginStepRequest {
            run_id: &self.inner.run_id,
            phase_id: &self.inner.phase_id,
            step_name: Self::step_name_for_key(key),
            idempotency_key: key.as_str(),
            payload: None,
            reservation_ttl_secs: None,
        })
        .context("encode BeginStepRequest (probe)")?;
        let resp: BeginStepResponse = self.call_method(METHOD_DURABLE_BEGIN_STEP, params).await?;
        match resp.status.as_str() {
            step_status::NEW => {
                let _ = self
                    .call_method::<Value>(
                        METHOD_DURABLE_ABANDON_STEP,
                        serde_json::to_value(AbandonStepRequest {
                            step_id: &resp.step_id,
                            reason: Some("step_query probe".to_string()),
                        })
                        .context("encode AbandonStepRequest")?,
                    )
                    .await;
                Ok(FenceState::Absent)
            }
            step_status::IN_PROGRESS => Ok(FenceState::InProgress { reservation_id: resp.step_id }),
            step_status::ALREADY_COMMITTED => Ok(FenceState::PriorSuccess {
                response: resp.prior_output.unwrap_or(Value::Null),
            }),
            step_status::PRIOR_ERROR => Ok(FenceState::PriorError {
                message: resp
                    .prior_error
                    .map(|e| e.message)
                    .unwrap_or_else(|| "(no error message captured)".to_string()),
            }),
            other => Err(anyhow!("durable_store returned unknown step status `{}` for key {}", other, key)),
        }
    }

    async fn step_begin(&self, key: &IdempotencyKey) -> Result<String> {
        self.ensure_workflow_run().await?;
        let params = serde_json::to_value(BeginStepRequest {
            run_id: &self.inner.run_id,
            phase_id: &self.inner.phase_id,
            step_name: Self::step_name_for_key(key),
            idempotency_key: key.as_str(),
            payload: None,
            reservation_ttl_secs: None,
        })
        .context("encode BeginStepRequest")?;
        let resp: BeginStepResponse = self.call_method(METHOD_DURABLE_BEGIN_STEP, params).await?;
        match resp.status.as_str() {
            step_status::NEW => Ok(resp.step_id),
            other => Err(anyhow!(
                "step_begin for key {} returned non-NEW status `{}` (durable_store treated this as collision)",
                key,
                other
            )),
        }
    }

    async fn step_complete(&self, reservation_id: &str, response: Value) -> Result<()> {
        let params = serde_json::to_value(CommitStepRequest {
            step_id: reservation_id,
            outcome: commit_outcome::SUCCESS,
            output: Some(response),
            error: None,
        })
        .context("encode CommitStepRequest (success)")?;
        let _: Value = self.call_method(METHOD_DURABLE_COMMIT_STEP, params).await?;
        Ok(())
    }

    async fn step_fail(&self, reservation_id: &str, error: &str) -> Result<()> {
        let params = serde_json::to_value(CommitStepRequest {
            step_id: reservation_id,
            outcome: commit_outcome::ERROR,
            output: None,
            error: Some(StepErrorPayload { code: "STEP_FAILED".to_string(), message: error.to_string() }),
        })
        .context("encode CommitStepRequest (error)")?;
        let _: Value = self.call_method(METHOD_DURABLE_COMMIT_STEP, params).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn step_name_for_key_takes_suffix_after_last_colon() {
        let k = IdempotencyKey::new("scope", "wf-1", "impl", 0);
        assert_eq!(DbosDurableStoreClient::step_name_for_key(&k), "0");
    }

    #[test]
    fn step_name_for_key_handles_no_colon() {
        let k = IdempotencyKey("just-name".to_string());
        assert_eq!(DbosDurableStoreClient::step_name_for_key(&k), "just-name");
    }
}
