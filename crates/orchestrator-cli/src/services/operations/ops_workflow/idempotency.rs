//! Actor-scoped durable admission for `workflow run --idempotency-key`.

use std::collections::BTreeMap;
use std::sync::Arc;

use animus_actor::Actor;
use anyhow::{anyhow, Context, Result};
use orchestrator_core::{
    services::ServiceHub, OrchestratorWorkflow, WorkflowLaunchBegin, WorkflowLaunchClaim,
    WorkflowLaunchIdempotencyStore, WorkflowRunInput, DEFAULT_WORKFLOW_LAUNCH_LEASE_SECS,
};
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::phases::{self, DetachedRunnerOverrides};

struct LeaseHeartbeat {
    stop: Option<std::sync::mpsc::Sender<()>>,
    join: Option<std::thread::JoinHandle<Result<bool>>>,
}

impl LeaseHeartbeat {
    fn start(store: WorkflowLaunchIdempotencyStore, claim: Box<WorkflowLaunchClaim>) -> Self {
        let (stop, receiver) = std::sync::mpsc::channel();
        let interval =
            std::time::Duration::from_secs(u64::try_from((DEFAULT_WORKFLOW_LAUNCH_LEASE_SECS / 3).max(1)).unwrap_or(1));
        let join = std::thread::spawn(move || loop {
            match receiver.recv_timeout(interval) {
                Ok(()) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return Ok(true),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    if !store.renew(&claim)? {
                        return Ok(false);
                    }
                }
            }
        });
        Self { stop: Some(stop), join: Some(join) }
    }

    fn finish(mut self) -> Result<bool> {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        self.join
            .take()
            .expect("lease heartbeat join handle")
            .join()
            .map_err(|_| anyhow!("workflow launch lease heartbeat panicked"))?
    }
}

impl Drop for LeaseHeartbeat {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

fn canonical_json(value: Value) -> Value {
    match value {
        Value::Array(values) => Value::Array(values.into_iter().map(canonical_json).collect()),
        Value::Object(values) => {
            let sorted: BTreeMap<_, _> = values.into_iter().map(|(key, value)| (key, canonical_json(value))).collect();
            Value::Object(sorted.into_iter().collect())
        }
        scalar => scalar,
    }
}

fn launch_request_hash(
    project_root: &str,
    actor: &Actor,
    input: &WorkflowRunInput,
    overrides: &DetachedRunnerOverrides,
) -> Result<String> {
    let subject =
        input.subject().ok_or_else(|| anyhow!("--idempotency-key requires one actor-authorized --subject-id"))?;
    let workflow_ref = input
        .workflow_ref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!("idempotent workflow launch requires a resolved workflow ref"))?;
    let vars: BTreeMap<_, _> = input.vars.iter().collect();
    let request = canonical_json(serde_json::json!({
        "version": 1,
        "project_scope": protocol::repository_scope_for_path(std::path::Path::new(project_root)),
        "workspace_id": actor.tenant_id,
        "actor_id": actor.user_id,
        "workflow_ref": workflow_ref,
        "subject": { "kind": subject.kind(), "id": subject.id() },
        "input": input.input,
        "vars": vars,
        "overrides": {
            "model": overrides.model,
            "tool": overrides.tool,
            "phase_timeout_secs": overrides.phase_timeout_secs,
        },
    }));
    let encoded = serde_json::to_vec(&request).context("failed to encode effective workflow launch request")?;
    Ok(format!("{:x}", Sha256::digest(encoded)))
}

pub(crate) async fn start_workflow_idempotently(
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
    input: WorkflowRunInput,
    overrides: DetachedRunnerOverrides,
    caller_key: String,
) -> Result<OrchestratorWorkflow> {
    // The idempotent surface is application-only. Never turn an absent actor
    // into the local/global namespace, and require a workspace partition in
    // addition to the stable user id.
    let actor = overrides
        .actor
        .as_ref()
        .ok_or_else(|| anyhow!("--idempotency-key requires a transport-asserted --actor-json"))?;
    if actor.user_id.trim().is_empty() {
        return Err(anyhow!("idempotent workflow launch requires a non-empty actor user_id"));
    }
    let workspace_id = actor
        .tenant_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!("idempotent workflow launch requires actor tenant_id (workspace)"))?;
    let subject =
        input.subject().ok_or_else(|| anyhow!("--idempotency-key requires one actor-authorized --subject-id"))?;
    let workflow_ref = input
        .workflow_ref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!("idempotent workflow launch requires a resolved workflow ref"))?;

    // Fail before reserving if this installation cannot drive a run. Once a
    // reservation exists, every later error intentionally leaves it pending so
    // a restarted process can reconcile rather than accidentally launch twice.
    phases::ensure_workflow_runner_plugin(std::path::Path::new(project_root))?;
    let store = WorkflowLaunchIdempotencyStore::for_project(std::path::Path::new(project_root));
    let request_hash = launch_request_hash(project_root, actor, &input, &overrides)?;
    let qualified_subject_id = format!("{}:{}", subject.kind(), subject.id());
    let request = store.request(
        workspace_id,
        actor.user_id.clone(),
        caller_key,
        request_hash,
        workflow_ref,
        qualified_subject_id,
    );
    let claim = match store.begin(request)? {
        WorkflowLaunchBegin::Replay(replay) => return Ok(replay.workflow),
        WorkflowLaunchBegin::Conflict => {
            return Err(crate::conflict_error(
                "idempotency_conflict: key was already used for a different effective workflow launch",
            ));
        }
        WorkflowLaunchBegin::InProgress => {
            return Err(crate::conflict_error(
                "idempotency_in_progress: a workflow launch with this key is still pending",
            ));
        }
        WorkflowLaunchBegin::Acquired(claim) => claim,
    };

    // A live but slow config/journal/subject RPC must not be reclaimed while it
    // still owns the bootstrap. A process crash stops this heartbeat, after
    // which the durable lease expires and another process can safely recover.
    let heartbeat = LeaseHeartbeat::start(store.clone(), claim.clone());
    let workflow =
        phases::bootstrap_reserved_workflow(hub.clone(), project_root, claim.workflow_id.clone(), input, actor).await?;
    if !heartbeat.finish()? || !store.renew(&claim)? {
        return Err(crate::conflict_error(
            "idempotency_in_progress: workflow launch authority moved to a recovery process",
        ));
    }
    let before_spawn = || {
        if store.renew(&claim)? {
            Ok(())
        } else {
            Err(crate::conflict_error("idempotency_in_progress: workflow launch authority moved before runner spawn"))
        }
    };
    let workflow =
        phases::finalize_workflow_runner_start_guarded(hub, project_root, workflow, overrides, Some(&before_spawn))
            .await?;
    if !store.complete(&claim, &workflow)? {
        return Err(crate::conflict_error(
            "idempotency_in_progress: workflow launched but canonical replay is being reconciled",
        ));
    }
    Ok(workflow)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn actor(user: &str, tenant: &str) -> Actor {
        Actor { user_id: user.to_string(), claims: vec!["mutable-claim".to_string()], tenant_id: Some(tenant.into()) }
    }

    #[test]
    fn hash_is_stable_across_json_object_and_var_order() {
        let subject = protocol::orchestrator::SubjectRef::task("TASK-1");
        let first = WorkflowRunInput::for_subject(subject.clone(), Some("wf".into()))
            .with_input(Some(serde_json::json!({"b": 2, "a": {"z": 1, "y": 0}})))
            .with_vars(std::collections::HashMap::from([("b".into(), "2".into()), ("a".into(), "1".into())]));
        let second = WorkflowRunInput::for_subject(subject, Some("wf".into()))
            .with_input(Some(serde_json::json!({"a": {"y": 0, "z": 1}, "b": 2})))
            .with_vars(std::collections::HashMap::from([("a".into(), "1".into()), ("b".into(), "2".into())]));
        let overrides = DetachedRunnerOverrides::default();
        assert_eq!(
            launch_request_hash("/project", &actor("alice", "acme"), &first, &overrides).unwrap(),
            launch_request_hash("/project", &actor("alice", "acme"), &second, &overrides).unwrap()
        );
    }

    #[test]
    fn hash_changes_for_every_effective_launch_boundary() {
        let base = WorkflowRunInput::for_task("TASK-1".into(), Some("wf-a".into()));
        let base_hash = launch_request_hash(
            "/project-a",
            &actor("alice", "workspace-a"),
            &base,
            &DetachedRunnerOverrides::default(),
        )
        .unwrap();
        let cases = [
            launch_request_hash(
                "/project-b",
                &actor("alice", "workspace-a"),
                &base,
                &DetachedRunnerOverrides::default(),
            )
            .unwrap(),
            launch_request_hash("/project-a", &actor("bob", "workspace-a"), &base, &DetachedRunnerOverrides::default())
                .unwrap(),
            launch_request_hash(
                "/project-a",
                &actor("alice", "workspace-b"),
                &base,
                &DetachedRunnerOverrides::default(),
            )
            .unwrap(),
            launch_request_hash(
                "/project-a",
                &actor("alice", "workspace-a"),
                &WorkflowRunInput::for_task("TASK-2".into(), Some("wf-a".into())),
                &DetachedRunnerOverrides::default(),
            )
            .unwrap(),
            launch_request_hash(
                "/project-a",
                &actor("alice", "workspace-a"),
                &WorkflowRunInput::for_task("TASK-1".into(), Some("wf-b".into())),
                &DetachedRunnerOverrides::default(),
            )
            .unwrap(),
        ];
        assert!(cases.iter().all(|hash| hash != &base_hash));
    }
}
