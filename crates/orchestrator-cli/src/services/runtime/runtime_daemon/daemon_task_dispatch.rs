use super::*;
use animus_execution_protocol::ExecutionFence;
use animus_queue_protocol::{self as queue_proto, QueueLeaseMutationOutcome};
use anyhow::Context;
use orchestrator_daemon_runtime::{
    execute_dispatch_plan_via_runner, CodingRunResources, CodingScheduler, DispatchNoticeSink, DispatchSelectionSource,
    PlannedDispatchStart, ReservationOutcome, CODING_SCHEDULER_CAPACITY,
};
pub use orchestrator_daemon_runtime::{DispatchNotice, DispatchWorkflowStartSummary};
use tracing::warn;

use crate::services::plugin_clients;

pub async fn dispatch_queued_entries_via_runner(
    root: &str,
    process_manager: &mut ProcessManager,
    coding_scheduler: &CodingScheduler,
    owner_id: &str,
    limit: usize,
) -> anyhow::Result<DispatchWorkflowStartSummary> {
    let project_root = std::path::Path::new(root);
    let scheduler_status = coding_scheduler.status()?;
    let max = limit.min(scheduler_status.available).min(CODING_SCHEDULER_CAPACITY);
    if max == 0 {
        return Ok(DispatchWorkflowStartSummary::default());
    }

    let workflow_ids = (0..max).map(|_| uuid::Uuid::new_v4().to_string()).collect();
    let request = queue_proto::QueueLeaseV2Request {
        max,
        owner_id: owner_id.to_string(),
        workflow_ids,
        exclude: scheduler_status.reservations.iter().map(|lease| lease.execution.clone()).collect(),
    };
    request.validate().map_err(anyhow::Error::msg)?;

    let Some(response) = plugin_clients::call_queue_lease_v2(project_root, &request).await? else {
        anyhow::bail!("queue plugin not installed; generation-fenced dispatch cannot continue");
    };

    for block in &response.blocked {
        warn!(
            actor = protocol::ACTOR_DAEMON,
            entry_id = %block.entry_id,
            reason = ?block.reason,
            conflict_workflow_id = block.conflicts_with.as_ref().map(|fence| fence.workflow_id.as_str()),
            "queue/v2/lease left a candidate blocked without consuming a coding slot"
        );
    }

    let mut planned_starts = Vec::new();
    let mut leased = Vec::new();
    for fenced in response.leased {
        if let Err(error) = fenced.validate().and_then(|()| fenced.execution.validate_queue_backed()) {
            warn!(
                actor = protocol::ACTOR_DAEMON,
                entry_id = %fenced.entry.entry_id,
                error = %error,
                "queue/v2/lease returned an invalid fleet fence; leaving it assigned for explicit recovery"
            );
            continue;
        }
        let queue = fenced.execution.queue_lease.as_ref().expect("validated queue-backed lease");
        if queue.owner_id != owner_id {
            warn!(
                actor = protocol::ACTOR_DAEMON,
                entry_id = %fenced.entry.entry_id,
                expected_owner = %owner_id,
                actual_owner = %queue.owner_id,
                "queue/v2/lease returned a different owner; leaving entry assigned and refusing spawn"
            );
            continue;
        }

        let dispatch_value = match serde_json::to_value(&fenced.entry.subject_dispatch) {
            Ok(value) => value,
            Err(error) => {
                warn!(
                    actor = protocol::ACTOR_DAEMON,
                    entry_id = %fenced.entry.entry_id,
                    error = %error,
                    "queue/v2/lease returned an undecodable dispatch"
                );
                complete_failed(project_root, coding_scheduler, &fenced.execution, None).await;
                continue;
            }
        };
        let dispatch: protocol::SubjectDispatch = match serde_json::from_value(dispatch_value) {
            Ok(dispatch) => dispatch,
            Err(error) => {
                warn!(
                    actor = protocol::ACTOR_DAEMON,
                    entry_id = %fenced.entry.entry_id,
                    error = %error,
                    "queue/v2/lease subject dispatch drifted from the kernel protocol"
                );
                complete_failed(project_root, coding_scheduler, &fenced.execution, None).await;
                continue;
            }
        };

        let mut resources = CodingRunResources::from_execution(&fenced.execution)?;
        resources.pull_request = ["pull_request", "pull_request_url", "pr_url", "pr_number"]
            .iter()
            .find_map(|key| dispatch.vars.get(*key).cloned())
            .filter(|value| !value.trim().is_empty());
        match coding_scheduler.track(fenced.execution.clone(), resources)? {
            ReservationOutcome::Reserved { .. } => {}
            ReservationOutcome::Rejected { reason } => {
                warn!(
                    actor = protocol::ACTOR_DAEMON,
                    entry_id = %fenced.entry.entry_id,
                    collision = ?reason,
                    "local fleet projection rejected queue-owned lease; returning exact lease to pending"
                );
                release_to_pending(project_root, coding_scheduler, &fenced.execution, "local-fleet-collision").await;
                continue;
            }
        }

        planned_starts.push(PlannedDispatchStart {
            dispatch,
            workflow_id: Some(fenced.execution.workflow_id.clone()),
            execution_fence: Some(fenced.execution.clone()),
            selection_source: DispatchSelectionSource::DispatchQueue,
        });
        leased.push(fenced.execution);
    }

    let mut notice_sink = CliDispatchNoticeSink { outcomes: Vec::new() };
    let summary = execute_dispatch_plan_via_runner(root, process_manager, &planned_starts, max, &mut notice_sink);

    for (index, execution) in leased.iter().enumerate() {
        match notice_sink.outcomes.get(index) {
            Some(DispatchEntryOutcome::Started) => {
                // A fresh backend-clock lease is already live. Heartbeats renew
                // it; avoiding another RPC here leaves no spawn/renew ambiguity.
            }
            Some(DispatchEntryOutcome::Deferred) | None => {
                release_to_pending(project_root, coding_scheduler, execution, "spawn-deferred").await;
            }
            Some(DispatchEntryOutcome::Failed) => {
                complete_failed(
                    project_root,
                    coding_scheduler,
                    execution,
                    planned_starts.get(index).map(|start| start.dispatch.workflow_ref.as_str()),
                )
                .await;
            }
        }
    }
    Ok(summary)
}

pub(crate) async fn renew_execution_lease(
    project_root: &std::path::Path,
    coding_scheduler: &CodingScheduler,
    execution: &ExecutionFence,
) -> anyhow::Result<ExecutionFence> {
    let request = queue_proto::QueueLeaseRenewRequest { execution: execution.clone(), ttl_secs: None };
    request.validate().map_err(anyhow::Error::msg)?;
    let response = plugin_clients::call_queue_lease_renew(project_root, &request)
        .await?
        .context("queue plugin disappeared during lease renewal")?;
    match response.outcome {
        QueueLeaseMutationOutcome::Applied | QueueLeaseMutationOutcome::AlreadyApplied => {
            let current = response.execution.context("queue renewal succeeded without returning the current fence")?;
            match coding_scheduler.update_execution(execution, current.clone())? {
                ReservationOutcome::Reserved { .. } => Ok(current),
                ReservationOutcome::Rejected { reason } => {
                    anyhow::bail!("local fleet rejected renewed queue fence: {reason:?}")
                }
            }
        }
        outcome => anyhow::bail!(
            "queue lease renewal rejected with {outcome:?}: {}",
            response.reason.unwrap_or_else(|| "no reason".to_string())
        ),
    }
}

pub(crate) async fn recover_execution_lease(
    project_root: &std::path::Path,
    coding_scheduler: &CodingScheduler,
    execution: &ExecutionFence,
    new_owner_id: &str,
) -> anyhow::Result<ExecutionFence> {
    let request = queue_proto::QueueLeaseRecoverRequest {
        execution: execution.clone(),
        new_owner_id: new_owner_id.to_string(),
        ttl_secs: None,
    };
    request.validate().map_err(anyhow::Error::msg)?;
    let response = plugin_clients::call_queue_lease_recover(project_root, &request)
        .await?
        .context("queue plugin disappeared during lease recovery")?;
    match response.outcome {
        QueueLeaseMutationOutcome::Applied | QueueLeaseMutationOutcome::AlreadyApplied => {
            let current = response.execution.context("queue recovery succeeded without returning the current fence")?;
            match coding_scheduler.update_execution(execution, current.clone())? {
                ReservationOutcome::Reserved { .. } => Ok(current),
                ReservationOutcome::Rejected { reason } => {
                    anyhow::bail!("local fleet rejected recovered queue fence: {reason:?}")
                }
            }
        }
        outcome => anyhow::bail!(
            "queue lease recovery rejected with {outcome:?}: {}",
            response.reason.unwrap_or_else(|| "no reason".to_string())
        ),
    }
}

async fn release_to_pending(
    project_root: &std::path::Path,
    coding_scheduler: &CodingScheduler,
    execution: &ExecutionFence,
    reason: &str,
) {
    let request =
        queue_proto::QueueReleasePendingV2Request { execution: execution.clone(), reason: reason.to_string() };
    match plugin_clients::call_queue_release_pending_v2(project_root, &request).await {
        Ok(Some(response))
            if matches!(
                response.outcome,
                QueueLeaseMutationOutcome::Applied | QueueLeaseMutationOutcome::AlreadyApplied
            ) =>
        {
            let _ = coding_scheduler.release(execution);
        }
        Ok(Some(response)) => warn!(
            actor = protocol::ACTOR_DAEMON,
            workflow_id = %execution.workflow_id,
            outcome = ?response.outcome,
            reason = response.reason.as_deref().unwrap_or("no reason"),
            "queue/v2/release_pending rejected exact fence; retaining local reservation"
        ),
        Ok(None) => warn!(
            actor = protocol::ACTOR_DAEMON,
            workflow_id = %execution.workflow_id,
            "queue plugin disappeared during release_pending; retaining local reservation"
        ),
        Err(error) => warn!(
            actor = protocol::ACTOR_DAEMON,
            workflow_id = %execution.workflow_id,
            error = %error,
            "queue/v2/release_pending failed; retaining local reservation"
        ),
    }
}

pub(crate) async fn complete_execution(
    project_root: &std::path::Path,
    coding_scheduler: &CodingScheduler,
    execution: &ExecutionFence,
    status: &str,
    workflow_ref: Option<&str>,
) -> anyhow::Result<()> {
    let request = queue_proto::QueueCompletionV2Request {
        execution: execution.clone(),
        status: status.to_string(),
        workflow_ref: workflow_ref.map(str::to_string),
    };
    request.validate().map_err(anyhow::Error::msg)?;
    let response = plugin_clients::call_queue_completion_v2(project_root, &request)
        .await?
        .context("queue plugin disappeared during completion")?;
    anyhow::ensure!(
        matches!(response.outcome, QueueLeaseMutationOutcome::Applied | QueueLeaseMutationOutcome::AlreadyApplied),
        "queue completion rejected with {:?}: {}",
        response.outcome,
        response.reason.unwrap_or_else(|| "no reason".to_string())
    );
    // Queue completion is the authority. Local projection cleanup is
    // idempotent and may already have happened after a prior successful RPC.
    let _ = coding_scheduler.release(execution)?;
    Ok(())
}

async fn complete_failed(
    project_root: &std::path::Path,
    coding_scheduler: &CodingScheduler,
    execution: &ExecutionFence,
    workflow_ref: Option<&str>,
) {
    if let Err(error) = complete_execution(
        project_root,
        coding_scheduler,
        execution,
        queue_proto::completion_status::FAILED,
        workflow_ref,
    )
    .await
    {
        warn!(
            actor = protocol::ACTOR_DAEMON,
            workflow_id = %execution.workflow_id,
            error = %error,
            "queue/v2/completion failed; retaining local reservation for recovery"
        );
    }
}

enum DispatchEntryOutcome {
    Started,
    Deferred,
    Failed,
}

struct CliDispatchNoticeSink {
    outcomes: Vec<DispatchEntryOutcome>,
}

impl DispatchNoticeSink for CliDispatchNoticeSink {
    fn notice(&mut self, notice: DispatchNotice) {
        match notice {
            DispatchNotice::Started { .. } => self.outcomes.push(DispatchEntryOutcome::Started),
            DispatchNotice::Failed { dispatch, error } => {
                warn!(
                    actor = protocol::ACTOR_DAEMON,
                    subject_id = %dispatch.subject_id().unwrap_or_default(),
                    error = %error,
                    "failed to start workflow runner"
                );
                self.outcomes.push(DispatchEntryOutcome::Failed);
            }
            DispatchNotice::Deferred { dispatch, reason } => {
                warn!(
                    actor = protocol::ACTOR_DAEMON,
                    subject_id = %dispatch.subject_id().unwrap_or_default(),
                    reason = %reason,
                    "workflow runner spawn deferred; exact queue fence returns to pending"
                );
                self.outcomes.push(DispatchEntryOutcome::Deferred);
            }
            _ => {}
        }
    }
}
