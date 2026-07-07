use crate::cli_types::DaemonRunArgs;
use crate::services::operations::{
    build_plugin_routing, build_queue_routing, build_workflow_routing, run_plugin_install, PluginInstallRequest,
};
use crate::services::runtime::runtime_daemon::build_daemon_ops_routing;
use crate::services::runtime::runtime_daemon::daemon_reconciliation::{
    journal_resume_enabled, recover_orphaned_running_workflows,
};
use anyhow::Result;
use async_trait::async_trait;
use orchestrator_core::services::DaemonStartConfig;
use orchestrator_core::DaemonStatus;
use orchestrator_core::FileServiceHub;
use orchestrator_core::ServiceHub;
use orchestrator_core::{
    load_daemon_project_config, write_daemon_project_config, InstalledPluginSummary, PluginInstaller,
};
use orchestrator_daemon_runtime::control::{DaemonOpsRouting, PluginRouting, QueueRouting, WorkflowRouting};
use orchestrator_daemon_runtime::{
    discover_installed_plugins, run_daemon, DaemonRunEvent, DaemonRunHooks, ProcessManager,
};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::SystemTime;

#[cfg(test)]
use super::canonicalize_lossy;
use super::daemon_run_host::DefaultDaemonRunHost;
use super::daemon_scheduler::{runtime_options_from_cli, slim_project_tick_driver, SlimProjectTickDriver};
use animus_runtime_shared::persist_resumed_phase_completion;
use animus_runtime_shared::phase_session::{
    list_running_checkpoints, update_session_blocked, update_session_completed, update_session_running_after_resume,
    SessionCheckpoint,
};
use orchestrator_plugin_host::session::{
    canonical_tool_alias, discover_provider_plugins, PluginSessionBackend, ResumeAgentOutcome,
};
use std::collections::HashMap;
use std::path::Path;

pub(super) enum ResumeLookup {
    NotInstalled,
    Outcome(ResumeAgentOutcome),
}

#[async_trait::async_trait(?Send)]
pub(super) trait ResumeProviderRegistry {
    async fn try_resume(&self, checkpoint: &SessionCheckpoint) -> ResumeLookup;
}

// Applies a resumed phase outcome back to durable workflow state once the
// provider plugin's resumed session has drained to a terminal Finished.
// Production wires this through `FileServiceHub`: it persists a minimal
// completion via `persist_resumed_phase_completion`, advances the workflow
// state machine via `complete_current_phase_with_decision`, and flips the
// session checkpoint to Completed so the next restart does not retry it.
// Tests stub it with `RecordingApplier` so the resume-flow unit tests stay
// in-memory and focused on the dispatch surface.
#[async_trait::async_trait(?Send)]
pub(super) trait ResumedOutcomeApplier {
    async fn apply(&self, checkpoint: &SessionCheckpoint, new_session_id: Option<&str>) -> ResumedOutcomeApplyResult;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ResumedOutcomeApplyResult {
    // Workflow state was advanced and session checkpoint flipped to Completed.
    Advanced,
    // The workflow had already advanced past this phase (durable marker was
    // already on disk from a previous restart). We still flipped the session
    // checkpoint to Completed but did not double-advance.
    AlreadyAdvanced,
    // The phase is decision-gated (review/QA/testing/etc.) so we refuse to
    // synthesize an `Advance` verdict from the resumed session. The session
    // checkpoint is marked Blocked with an actionable reason so the operator
    // can re-run the phase via `animus workflow resume <id> --force`.
    BlockedAwaitingDecision,
    // Apply failed before any state was advanced. The session checkpoint is
    // left as-is (still Running) so the next restart can retry.
    Failed(String),
}

// Codex round-7 P1 follow-up: phases whose verdict gates downstream work
// (review, QA, testing, requirements review) MUST NOT have an implicit
// Advance synthesized when we only have a Finished signal but no captured
// PhaseDecision. Synthesizing Advance for these would let a Rework/Fail
// result from the resumed agent silently bypass the gate after restart.
//
// Codex round-7 round-2 P1+P2 extends this to:
//   - PROJECT-CONFIGURED phases that set capabilities.is_review /
//     capabilities.is_testing in agent-runtime-config under custom names
//     (e.g. `security-audit`).
//   - Any phase that declares a `decision_contract` — those phases own a
//     verdict and a synthesized Advance silently bypasses it.
//   - Phases that require a commit message (built-in `implementation` +
//     custom equivalents) — we can't synthesize a commit message, and a
//     resumed agent could have made file changes that were never committed;
//     downstream review/PR/merge would otherwise miss the work.
//
// Codex round-7 round-4 P2 adds workflow-level `on_verdict` routing as a
// fifth gate signal — a phase entry in workflows.yaml may declare
// verdict-conditional routing even when the phase itself has no
// decision_contract, and synthesizing Advance would silently bypass it.
//
// Implementation/research/design phases default-to-advance under the same
// no-decision path during normal execution (see
// `phase_output::persist_phase_output`'s "None" branch), so it is safe to
// keep mirroring that behavior for them when none of the gate signals fire.
//
// Test-only thin wrapper around the workflow-aware variant: production
// callers always pass an explicit `workflow_ref`, so the no-workflow
// overload exists purely to keep the R7 round-7 regression tests below
// readable without threading `None` through every assertion.
#[cfg(test)]
fn phase_requires_explicit_decision_for_resume(project_root: &str, phase_id: &str) -> bool {
    phase_requires_explicit_decision_for_resume_in_workflow(project_root, phase_id, None)
}

fn phase_requires_explicit_decision_for_resume_in_workflow(
    project_root: &str,
    phase_id: &str,
    workflow_ref: Option<&str>,
) -> bool {
    let ctx = animus_runtime_shared::config_context::RuntimeConfigContext::load(project_root);

    if animus_runtime_shared::phase_prompt::phase_requires_commit_message_with_ctx(&ctx, phase_id) {
        return true;
    }

    let runtime_config = orchestrator_core::load_agent_runtime_config_or_default(std::path::Path::new(project_root));
    if let Some(definition) = runtime_config.phase_execution(phase_id) {
        if definition.decision_contract.is_some() {
            return true;
        }
        if let Some(caps) = definition.capabilities.as_ref() {
            let merged = caps.clone().merge_with_defaults(phase_id);
            if merged.is_review || merged.is_testing || merged.requires_commit {
                return true;
            }
        }
    }

    let caps = protocol::PhaseCapabilities::defaults_for_phase(phase_id);
    if caps.is_review || caps.is_testing || caps.requires_commit {
        return true;
    }

    // Workflow YAML-level on_verdict gating. The presence of ANY
    // on_verdict entry for this phase signals the workflow owner expects
    // a real verdict; synthesizing Advance would bypass their routing.
    let workflow_config = orchestrator_core::load_workflow_config_or_default(std::path::Path::new(project_root));
    let routing = orchestrator_core::resolve_workflow_verdict_routing(&workflow_config.config, workflow_ref);
    if let Some(verdicts) = routing.get(phase_id) {
        if !verdicts.is_empty() {
            return true;
        }
    }

    false
}

struct ProductionResumeProviderRegistry {
    providers: HashMap<String, Arc<PluginSessionBackend>>,
}

impl ProductionResumeProviderRegistry {
    fn discover(project_root: &Path) -> Self {
        let providers = discover_provider_plugins(project_root)
            .into_iter()
            .map(|plugin| (plugin.provider_tool.to_ascii_lowercase(), plugin.into_backend()))
            .collect();
        Self { providers }
    }
}

// Pure resume-lookup helper, factored out for testability. Returns the
// canonicalized provider key actually used to look up the resume backend.
// Mirrors the alias normalization done by the normal `SessionBackendResolver`
// so historical `checkpoint.provider = "oai-runner"` (or `"animus-oai-runner"`)
// still finds the installed plugin registered under `provider_tool = "oai"`.
// `provider_keys` is a string-only stand-in for the real `providers` map keys
// so tests can exercise the lookup without standing up a `PluginSessionBackend`.
// Production callers inline the same canonicalization in `try_resume` below;
// this helper exists only so the W6 alias-canonicalization regression tests
// stay focused on the pure string-key logic.
#[cfg(test)]
fn resolve_resume_provider_key(provider_keys: &[String], checkpoint_provider: &str) -> Option<String> {
    let canonical = canonical_tool_alias(checkpoint_provider);
    provider_keys.iter().find(|k| **k == canonical).cloned()
}

#[async_trait::async_trait(?Send)]
impl ResumeProviderRegistry for ProductionResumeProviderRegistry {
    async fn try_resume(&self, checkpoint: &SessionCheckpoint) -> ResumeLookup {
        // Codex round 8 P2 #2: historical workflows wrote
        // `checkpoint.provider = "oai-runner"` (or `"animus-oai-runner"`),
        // but the installed first-party plugin registers under
        // `provider_tool = "oai"`. The normal session resolver
        // canonicalizes aliases before lookup (see
        // `orchestrator_session_host::canonical_tool_alias` and
        // `SessionBackendResolver::resolve`); restart-resume must mirror
        // that or it mis-classifies an installed plugin as
        // `NotInstalled` and blocks the checkpoint with a misleading
        // reinstall hint.
        let canonical = canonical_tool_alias(&checkpoint.provider);
        let Some(backend) = self.providers.get(&canonical).cloned() else {
            return ResumeLookup::NotInstalled;
        };
        let Some(session_id) = checkpoint.provider_session_id.as_deref().map(str::trim).filter(|s| !s.is_empty())
        else {
            return ResumeLookup::Outcome(ResumeAgentOutcome::Failed {
                reason: "resume_agent failed: no provider session_id captured before crash; phase will re-run from scratch on resume --force".to_string(),
            });
        };
        let Some(request) = checkpoint.request.as_ref() else {
            return ResumeLookup::Outcome(ResumeAgentOutcome::Failed {
                reason: "resume_agent failed: checkpoint missing original request snapshot".to_string(),
            });
        };
        ResumeLookup::Outcome(backend.resume_agent_for_restart(session_id, request).await)
    }
}

// Production wiring of `ResumedOutcomeApplier` that drives the in-tree
// workflow service hub. Owns the scoped state root and a project-root
// `ServiceHub` so we can both write the phase-output marker and advance the
// workflow state machine in one place.
pub(super) struct FileResumedOutcomeApplier {
    project_root: String,
    scoped_root: std::path::PathBuf,
    hub: Arc<dyn ServiceHub>,
}

impl FileResumedOutcomeApplier {
    pub(super) fn new(project_root: String, scoped_root: std::path::PathBuf, hub: Arc<dyn ServiceHub>) -> Self {
        Self { project_root, scoped_root, hub }
    }
}

#[async_trait::async_trait(?Send)]
impl ResumedOutcomeApplier for FileResumedOutcomeApplier {
    async fn apply(&self, checkpoint: &SessionCheckpoint, new_session_id: Option<&str>) -> ResumedOutcomeApplyResult {
        apply_resumed_outcome_in_tree(
            &self.project_root,
            &self.scoped_root,
            self.hub.clone(),
            checkpoint,
            new_session_id,
        )
        .await
    }
}

// Shared implementation: keeps the rewrite of session-checkpoint state, the
// durable phase-output write, and the workflow state mutation in lock-step.
// Order matters for crash-recovery idempotency:
//   1. Rewrite session checkpoint to Running w/ rotated provider_session_id
//      first, so even if we crash before the rest a future restart still
//      sees the freshest session id to dispatch against.
//   2. Look up the workflow + locate the attempt counter for the phase the
//      session belongs to. If the workflow has already moved past this
//      phase (durable marker carried it forward in a previous restart) the
//      apply collapses to "just mark checkpoint Completed" so we never
//      double-advance.
//   3. Persist the resumed completion via the existing tmp+rename writer
//      (so a re-apply rewrites the same bytes rather than racing).
//   4. Call `complete_current_phase_with_decision(workflow_id, None)` —
//      mirrors the verdict-less branch of the normal phase-success path.
//   5. Mark the session checkpoint Completed last so a crash between (3)
//      and (5) leaves the marker readable and the next restart finishes
//      the job by replaying the persisted decision via the workflow loop.
pub(super) async fn apply_resumed_outcome_in_tree(
    project_root: &str,
    scoped_root: &Path,
    hub: Arc<dyn ServiceHub>,
    checkpoint: &SessionCheckpoint,
    new_session_id: Option<&str>,
) -> ResumedOutcomeApplyResult {
    let workflow_id = checkpoint.workflow_id.as_str();
    let phase_id = checkpoint.phase_id.as_str();

    if let Err(err) = update_session_running_after_resume(scoped_root, workflow_id, phase_id, new_session_id) {
        return ResumedOutcomeApplyResult::Failed(format!("failed to refresh session checkpoint after resume: {err}"));
    }

    let workflow = match hub.workflows().get(workflow_id).await {
        Ok(workflow) => workflow,
        Err(err) => {
            return ResumedOutcomeApplyResult::Failed(format!(
                "failed to load workflow '{workflow_id}' for resumed phase application: {err}"
            ));
        }
    };

    let current_phase_id = workflow
        .current_phase
        .clone()
        .or_else(|| workflow.phases.get(workflow.current_phase_index).map(|phase| phase.phase_id.clone()));

    // Workflow already advanced past this phase in a prior restart pass —
    // do NOT re-persist or re-advance; just mark the session checkpoint
    // terminal so it stops showing up in the resume scan. We treat
    // "already advanced" as ANY of: current_phase no longer matches the
    // checkpoint's phase_id, OR the workflow's specific phase entry has
    // already transitioned out of Running/Ready/Pending (a previous apply
    // marked it Success on its way to Completed), OR the workflow status
    // is itself terminal.
    let phase_done_already = workflow
        .phases
        .iter()
        .find(|p| p.phase_id == phase_id)
        .map(|p| {
            !matches!(
                p.status,
                orchestrator_core::WorkflowPhaseStatus::Running
                    | orchestrator_core::WorkflowPhaseStatus::Ready
                    | orchestrator_core::WorkflowPhaseStatus::Pending
            )
        })
        .unwrap_or(false);
    let workflow_terminal = matches!(
        workflow.status,
        orchestrator_core::WorkflowStatus::Completed
            | orchestrator_core::WorkflowStatus::Cancelled
            | orchestrator_core::WorkflowStatus::Failed
            | orchestrator_core::WorkflowStatus::Escalated
    );
    if current_phase_id.as_deref() != Some(phase_id) || phase_done_already || workflow_terminal {
        if let Err(err) = update_session_completed(scoped_root, workflow_id, phase_id) {
            return ResumedOutcomeApplyResult::Failed(format!(
                "workflow already advanced past phase '{phase_id}' but failed to mark session completed: {err}"
            ));
        }
        return ResumedOutcomeApplyResult::AlreadyAdvanced;
    }

    // Decision-gated phases (review/QA/testing), phases that own a
    // decision_contract, and phases that require a commit message MUST
    // NOT have an implicit Advance synthesized — we never captured the
    // resumed agent's actual verdict (or commit message), so silently
    // advancing would let a Rework/Fail bypass the gate or push
    // downstream review/PR/merge past uncommitted file changes. Block
    // with an explicit, --force-aware reason so the operator knows to
    // re-run the phase.
    //
    // Codex round-7 round-3 P1: we ALSO need to flip the workflow itself
    // to Paused — otherwise the next scheduler tick's
    // recover_orphaned_running_workflows (no longer shielded once we drop
    // back into the steady-state loop) cancels the workflow before the
    // operator can run --force.
    if phase_requires_explicit_decision_for_resume_in_workflow(project_root, phase_id, workflow.workflow_ref.as_deref())
    {
        let reason = format!(
            "resumed agent finished but daemon could not recover the phase decision for decision-gated phase '{phase_id}'; re-run with `animus workflow resume {workflow_id} --force` to capture a fresh verdict"
        );
        let _ = update_session_blocked(scoped_root, workflow_id, phase_id, &reason);
        // Codex round-7 round-4 P2: do not re-pause an already-paused
        // workflow — `WorkflowLifecycleExecutor::pause` uses `expect` on
        // a transition that does not exist from Paused and would panic
        // and crash the daemon startup recovery loop.
        if !matches!(workflow.status, orchestrator_core::WorkflowStatus::Paused) {
            if let Err(err) = hub.workflows().pause(workflow_id).await {
                return ResumedOutcomeApplyResult::Failed(format!(
                    "blocked decision-gated phase '{phase_id}' but failed to pause workflow '{workflow_id}': {err}"
                ));
            }
        }
        return ResumedOutcomeApplyResult::BlockedAwaitingDecision;
    }

    let phase_attempt = workflow.phases.iter().find(|p| p.phase_id == phase_id).map(|p| p.attempt).unwrap_or(0);

    if let Err(err) = persist_resumed_phase_completion(project_root, workflow_id, phase_id, phase_attempt) {
        return ResumedOutcomeApplyResult::Failed(format!(
            "failed to persist resumed phase output for '{workflow_id}/{phase_id}': {err}"
        ));
    }

    let advanced_workflow = match hub.workflows().complete_current_phase_with_decision(workflow_id, None).await {
        Ok(workflow) => workflow,
        Err(err) => {
            return ResumedOutcomeApplyResult::Failed(format!(
                "failed to advance workflow '{workflow_id}' after resumed phase '{phase_id}': {err}"
            ));
        }
    };

    // Codex round-7 round-3 P1: if the advance moved the workflow into a
    // NEW Running phase (not the terminal Completed/Failed state) we are
    // running outside of any animus-workflow-runner process, so nothing is
    // driving the new phase. The next zombie sweep would cancel the
    // workflow. Pause it so the operator can resume it under a fresh
    // runner via `animus workflow resume <id>`. Terminal advances
    // (Completed / Failed / Cancelled) are safe to leave alone.
    let workflow_terminal_after = matches!(
        advanced_workflow.status,
        orchestrator_core::WorkflowStatus::Completed
            | orchestrator_core::WorkflowStatus::Cancelled
            | orchestrator_core::WorkflowStatus::Failed
            | orchestrator_core::WorkflowStatus::Escalated
    );
    if !workflow_terminal_after && !matches!(advanced_workflow.status, orchestrator_core::WorkflowStatus::Paused) {
        if let Err(err) = hub.workflows().pause(workflow_id).await {
            return ResumedOutcomeApplyResult::Failed(format!(
                "advanced workflow '{workflow_id}' after resumed phase '{phase_id}' but failed to pause it for handoff: {err}"
            ));
        }
    }

    if let Err(err) = update_session_completed(scoped_root, workflow_id, phase_id) {
        return ResumedOutcomeApplyResult::Failed(format!(
            "advanced workflow '{workflow_id}' but failed to mark session checkpoint completed: {err}"
        ));
    }

    ResumedOutcomeApplyResult::Advanced
}

// Returns the set of workflow_ids that have a Running session checkpoint
// with a captured provider_session_id — i.e. workflows that
// auto_resume_running_checkpoints can actually attempt to resume. These
// must be excluded from orphan recovery at daemon startup; otherwise
// recover_orphaned_running_workflows would cancel them before the resume
// pass ever runs. Returns an empty set when the scoped state root is
// missing or the checkpoint listing fails — safer than over-shielding.
pub(super) fn resumable_workflow_ids_for_project(project_root: &str) -> std::collections::HashSet<String> {
    let scoped_root = match protocol::repository_scope::scoped_state_root(std::path::Path::new(project_root)) {
        Some(root) => root,
        None => return std::collections::HashSet::new(),
    };
    let items = match list_running_checkpoints(&scoped_root) {
        Ok(items) => items,
        Err(_) => return std::collections::HashSet::new(),
    };
    items
        .into_iter()
        .filter_map(|(_, checkpoint)| {
            let has_sid =
                checkpoint.provider_session_id.as_deref().map(str::trim).as_ref().is_some_and(|s| !s.is_empty());
            if has_sid {
                Some(checkpoint.workflow_id)
            } else {
                None
            }
        })
        .collect()
}

pub(super) async fn auto_resume_running_checkpoints<R, A>(
    scoped_root: &Path,
    registry: &R,
    applier: &A,
) -> AutoResumeReport
where
    R: ResumeProviderRegistry + ?Sized,
    A: ResumedOutcomeApplier + ?Sized,
{
    let mut report = AutoResumeReport::default();
    let running = match list_running_checkpoints(scoped_root) {
        Ok(items) => items,
        Err(_) => return report,
    };
    for (_path, checkpoint) in running {
        // Guard rail: provider plugins can only resume an external session
        // they themselves issued. If no provider_session_id was captured
        // before the daemon crashed, dispatching ANY id (the run_id, an
        // empty string, …) makes the plugin reject + the daemon mark the
        // phase blocked with a misleading error. Skip the dispatch and
        // block with an actionable, --force-aware reason instead.
        let provider_sid_present =
            checkpoint.provider_session_id.as_deref().map(str::trim).as_ref().is_some_and(|sid| !sid.is_empty());
        if !provider_sid_present {
            let reason =
                "no provider session_id captured before crash; phase will re-run from scratch on resume --force"
                    .to_string();
            let _ = update_session_blocked(scoped_root, &checkpoint.workflow_id, &checkpoint.phase_id, &reason);
            report.blocked_on_failure += 1;
            continue;
        }
        match registry.try_resume(&checkpoint).await {
            ResumeLookup::Outcome(ResumeAgentOutcome::Resumed { session_id }) => {
                // Codex round-7 P1: the resumed agent has finished, but
                // until we apply the outcome back through the workflow
                // service the run stays stuck in Running with no
                // persisted phase decision. Persist a minimal completion
                // (verdict defaults to "advance" like the normal
                // no-decision path), advance the workflow, and flip the
                // session checkpoint to Completed so subsequent restarts
                // don't pick it back up.
                match applier.apply(&checkpoint, session_id.as_deref()).await {
                    ResumedOutcomeApplyResult::Advanced | ResumedOutcomeApplyResult::AlreadyAdvanced => {
                        report.resumed += 1;
                    }
                    ResumedOutcomeApplyResult::BlockedAwaitingDecision => {
                        // Decision-gated phase. The applier already wrote
                        // the descriptive blocked reason; just account it.
                        report.blocked_on_failure += 1;
                    }
                    ResumedOutcomeApplyResult::Failed(reason) => {
                        let blocked_reason = format!("resume succeeded but applying outcome failed: {reason}");
                        let _ = update_session_blocked(
                            scoped_root,
                            &checkpoint.workflow_id,
                            &checkpoint.phase_id,
                            &blocked_reason,
                        );
                        report.blocked_on_failure += 1;
                    }
                }
            }
            ResumeLookup::Outcome(ResumeAgentOutcome::Failed { reason }) => {
                let _ = update_session_blocked(scoped_root, &checkpoint.workflow_id, &checkpoint.phase_id, &reason);
                report.blocked_on_failure += 1;
            }
            ResumeLookup::NotInstalled => {
                let reason = format!(
                    "provider '{}' not installed; reinstall with `animus plugin install launchapp-dev/animus-provider-{}` then resume with --force",
                    checkpoint.provider, checkpoint.provider
                );
                let _ = update_session_blocked(scoped_root, &checkpoint.workflow_id, &checkpoint.phase_id, &reason);
                report.blocked_uninstalled += 1;
            }
        }
    }
    report
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) struct AutoResumeReport {
    pub resumed: usize,
    pub blocked_on_failure: usize,
    pub blocked_uninstalled: usize,
}

pub(super) struct CliPluginInstaller {
    project_root: String,
}

impl CliPluginInstaller {
    pub(super) fn new(project_root: impl Into<String>) -> Self {
        Self { project_root: project_root.into() }
    }
}

#[async_trait(?Send)]
impl PluginInstaller for CliPluginInstaller {
    async fn install(&self, repo_spec: &str) -> Result<String> {
        // Daemon auto-install only targets curated launchapp-dev provider
        // repos (e.g. launchapp-dev/animus-provider-claude) when preflight
        // detects a missing provider role. Those repos legitimately claim
        // the reserved in-tree provider_tool names, so bypass the
        // shadow-builtin guard here — user-typed installs still must opt in
        // explicitly via --allow-shadow-builtin.
        let req = PluginInstallRequest {
            source: Some(repo_spec.to_string()),
            yes: true,
            allow_shadow_builtin: true,
            ..Default::default()
        };
        let output = run_plugin_install(req).await?;
        Ok(output.name)
    }

    async fn rediscover(&self) -> Result<Vec<InstalledPluginSummary>> {
        discover_installed_plugins(&self.project_root)
    }
}

struct CliDaemonRunHost {
    inner: DefaultDaemonRunHost,
    start_config: DaemonStartConfig,
    installer: Arc<CliPluginInstaller>,
    plugin_routing: Arc<dyn PluginRouting>,
    daemon_ops_routing: Arc<dyn DaemonOpsRouting>,
    workflow_routing: Arc<dyn WorkflowRouting>,
    queue_routing: Arc<dyn QueueRouting>,
}

impl CliDaemonRunHost {
    fn new(project_root: &str, json: bool, start_config: DaemonStartConfig) -> Self {
        let project_root_path = PathBuf::from(project_root);
        let plugin_routing = build_plugin_routing(project_root_path.clone());
        let daemon_ops_routing = build_daemon_ops_routing(project_root_path.clone(), SystemTime::now());
        let workflow_routing = build_workflow_routing(project_root_path.clone());
        let queue_routing = build_queue_routing(project_root_path);
        let installer = Arc::new(CliPluginInstaller::new(project_root));
        Self {
            inner: DefaultDaemonRunHost::new(project_root, json),
            start_config,
            installer,
            plugin_routing,
            daemon_ops_routing,
            workflow_routing,
            queue_routing,
        }
    }

    fn logger(&self) -> std::sync::Arc<orchestrator_logging::Logger> {
        self.inner.logger.clone()
    }
}

#[async_trait::async_trait(?Send)]
impl DaemonRunHooks for CliDaemonRunHost {
    fn handle_event(&mut self, event: DaemonRunEvent) -> Result<()> {
        self.inner.handle_event(event)
    }

    async fn queue_next_deadline(&mut self, project_root: &str) -> Option<chrono::DateTime<chrono::Utc>> {
        // Query the installed queue plugin for the earliest future deferred
        // run_at. Any failure (no plugin, RPC error, unparseable timestamp)
        // degrades to None so the wake loop falls back to heartbeat pacing.
        let response = crate::services::plugin_clients::call_queue_next_deadline(std::path::Path::new(project_root))
            .await
            .ok()??;
        let raw = response.next_run_at?;
        match chrono::DateTime::parse_from_rfc3339(&raw) {
            Ok(dt) => Some(dt.with_timezone(&chrono::Utc)),
            Err(error) => {
                tracing::warn!(run_at = %raw, %error, "queue/next_deadline returned unparseable run_at; ignoring");
                None
            }
        }
    }

    async fn daemon_status(&mut self, project_root: &str) -> Result<DaemonStatus> {
        let hub = FileServiceHub::new(project_root)?;
        hub.daemon().status().await
    }

    async fn start_daemon(&mut self, project_root: &str) -> Result<()> {
        let hub = FileServiceHub::new(project_root)?;
        hub.daemon().start(self.start_config.clone()).await
    }

    async fn stop_daemon(&mut self, project_root: &str) -> Result<()> {
        let hub = FileServiceHub::new(project_root)?;
        hub.daemon().stop().await
    }

    async fn recover_startup_orphans(&mut self, project_root: &str) -> Result<usize> {
        let startup_hub = Arc::new(FileServiceHub::new(project_root)?);

        // Resume-before-orphan-cancel: shield workflows that hold a
        // resumable Running session checkpoint from the orphan sweep.
        // Otherwise recover_orphaned_running_workflows cancels every
        // persisted Running workflow before the auto-resume pass runs,
        // and resume has nothing left to recover. See codex round-4 P2.
        let resumable_workflow_ids = resumable_workflow_ids_for_project(project_root);

        // BU-4: when the durable journal is active, preserve ALL resumable
        // in-flight runs (not just those with a mid-phase session checkpoint)
        // so a restart/redeploy resumes them instead of destroying them. The
        // no-sid runs are re-dispatched from their phase boundary by the
        // steady-state dispatch leg on the first tick after boot.
        let resume_orphans = journal_resume_enabled(project_root);

        let orphans = recover_orphaned_running_workflows(
            startup_hub as Arc<dyn ServiceHub>,
            project_root,
            &resumable_workflow_ids,
            resume_orphans,
        )
        .await;

        if let Some(scoped_root) = protocol::repository_scope::scoped_state_root(std::path::Path::new(project_root)) {
            let registry = ProductionResumeProviderRegistry::discover(std::path::Path::new(project_root));
            let apply_hub: Arc<dyn ServiceHub> = Arc::new(FileServiceHub::new(project_root)?);
            let applier = FileResumedOutcomeApplier::new(project_root.to_string(), scoped_root.clone(), apply_hub);
            auto_resume_running_checkpoints(&scoped_root, &registry, &applier).await;
        }

        Ok(orphans)
    }

    async fn flush_notifications(&mut self, project_root: &str) -> Result<()> {
        self.inner.flush_notifications(project_root).await
    }

    async fn shutdown_drain_notifications(&mut self, project_root: &str) -> Result<()> {
        self.inner.shutdown_drain_notifications(project_root).await
    }

    fn plugin_routing(&self) -> Option<Arc<dyn PluginRouting>> {
        Some(self.plugin_routing.clone())
    }

    fn daemon_ops_routing(&self) -> Option<Arc<dyn DaemonOpsRouting>> {
        Some(self.daemon_ops_routing.clone())
    }

    fn workflow_routing(&self) -> Option<Arc<dyn WorkflowRouting>> {
        Some(self.workflow_routing.clone())
    }

    fn queue_routing(&self) -> Option<Arc<dyn QueueRouting>> {
        Some(self.queue_routing.clone())
    }

    fn plugin_installer(&self) -> Option<Arc<dyn PluginInstaller>> {
        Some(self.installer.clone())
    }
}

fn apply_scheduler_overrides_to_pm_config(args: &DaemonRunArgs, project_root: &str) {
    let project_path = std::path::Path::new(project_root);
    let mut config = load_daemon_project_config(project_path).unwrap_or_default();
    let mut changed = false;

    // Persist runtime-reconfigurable settings from CLI overrides so they survive
    // daemon restart and are available for hot-reload.
    if let Some(value) = args.scheduler.pool_size {
        if config.pool_size != Some(value) {
            config.pool_size = Some(value);
            changed = true;
        }
    }
    if let Some(value) = args.scheduler.interval_secs {
        if config.interval_secs != Some(value) {
            config.interval_secs = Some(value);
            changed = true;
        }
    }
    if let Some(value) = args.scheduler.max_tasks_per_tick {
        if config.max_tasks_per_tick != Some(value) {
            config.max_tasks_per_tick = Some(value);
            changed = true;
        }
    }
    if let Some(value) = args.scheduler.stale_threshold_hours {
        if config.stale_threshold_hours != Some(value) {
            config.stale_threshold_hours = Some(value);
            changed = true;
        }
    }
    if args.scheduler.phase_timeout_secs.is_some() && config.phase_timeout_secs != args.scheduler.phase_timeout_secs {
        config.phase_timeout_secs = args.scheduler.phase_timeout_secs;
        changed = true;
    }
    if changed {
        let _ = write_daemon_project_config(project_path, &config);
    }
}

pub(super) async fn handle_daemon_run(args: DaemonRunArgs, project_root: &str, json: bool) -> Result<()> {
    // The daemon is a long-running process that re-scans foreign agent-host skill
    // dirs (~/.claude, ~/.codex, ~/.cursor, ~/.config/opencode) on every config
    // reload. Those dirs ship SKILL.md files Animus does not parse (and can contain
    // unresolved merge markers), so without this the daemon sprays parse warnings on
    // every tick — millions of lines that bloat daemon.log and fill the disk. Suppress
    // them for the daemon's lifetime; first-party project/user skill parse failures
    // still warn elsewhere, and `animus skill list --verbose` re-enables them.
    orchestrator_config::skill_scoping::set_suppress_markdown_skill_parse_warnings(true);
    apply_scheduler_overrides_to_pm_config(&args, project_root);
    let mut runtime_options = runtime_options_from_cli(&args, project_root);
    let start_config = DaemonStartConfig { pool_size: runtime_options.pool_size };
    let workflow_config = orchestrator_core::load_workflow_config_or_default(std::path::Path::new(project_root));
    let daemon_config = workflow_config.config.daemon.as_ref();
    // Install the process-wide RuntimeQuotas BEFORE constructing
    // `ProcessManager`. `ProcessManager::new()` reads
    // `RuntimeQuotas::workflow_concurrency_max` to seed its spawn cap, so
    // the install must win the OnceLock race here even though
    // `run_daemon` later calls `install_runtime_quotas` again (the second
    // install is a no-op by design). First-installer-wins keeps tests and
    // tweaked quota setters intact.
    orchestrator_daemon_runtime::install_runtime_quotas(orchestrator_daemon_runtime::RuntimeQuotas::from_env());
    let mut process_manager = ProcessManager::new().with_timeout(runtime_options.phase_timeout_secs);
    process_manager.phase_routing = daemon_config.and_then(|d| d.phase_routing.clone());
    process_manager.mcp_config = daemon_config.and_then(|d| d.mcp.clone());
    let mut host = CliDaemonRunHost::new(project_root, json, start_config);
    let logger = host.logger();
    let mut driver: SlimProjectTickDriver<'_> =
        slim_project_tick_driver(&runtime_options, &mut process_manager, logger);

    crate::services::metrics::record_event(
        std::path::Path::new(project_root),
        crate::services::metrics::EventTags::DaemonStarted {},
    );

    // NB: the live `daemon.pid` file is written/cleared by `DaemonRunGuard`
    // (acquired inside `run_daemon`), tied to the single-winner daemon lock so
    // foreground `daemon run` registers liveness race-free. See
    // `daemon_run_guard.rs`.
    let run_result =
        run_daemon(project_root, &mut runtime_options, &mut driver, &mut host, |driver| driver.active_process_count())
            .await;

    run_result
}

#[cfg(test)]
mod tests {
    #![allow(clippy::await_holding_lock)]

    use super::*;
    use crate::services::runtime::runtime_daemon::{daemon_events_log_path, DaemonEventRecord};
    use crate::DaemonSchedulerArgs;
    use std::sync::MutexGuard;
    use tempfile::TempDir;

    fn lock_env() -> MutexGuard<'static, ()> {
        crate::shared::test_env_lock().lock().unwrap_or_else(|p| p.into_inner())
    }

    use protocol::test_utils::EnvVarGuard;

    // The foreground `daemon run` path registers its own PID so the daemon's
    // status/health handlers can confirm liveness (without it a healthy daemon
    // reports `running: false` / `unhealthy`). Verify the write creates the
    // scoped daemon dir on a fresh scope, round-trips the PID, and the cleanup
    // helper clears it.
    #[test]
    fn write_daemon_pid_creates_dir_and_round_trips() {
        let _lock = lock_env();
        let config_root = TempDir::new().expect("config temp dir");
        let home_root = TempDir::new().expect("home temp dir");
        let _config_guard = EnvVarGuard::set("ANIMUS_CONFIG_DIR", Some(config_root.path().to_string_lossy().as_ref()));
        let _home_guard = EnvVarGuard::set("HOME", Some(home_root.path().to_string_lossy().as_ref()));
        let _legacy_guard = EnvVarGuard::set("AGENT_ORCHESTRATOR_CONFIG_DIR", None);

        let primary = TempDir::new().expect("primary project dir");
        let primary_root = primary.path().to_string_lossy().to_string();

        // Fresh scope: the scoped daemon dir does not exist yet.
        assert_eq!(super::super::read_daemon_pid(&primary_root), None);

        let pid = std::process::id();
        super::super::write_daemon_pid(&primary_root, pid);
        assert_eq!(
            super::super::read_daemon_pid(&primary_root),
            Some(pid),
            "write_daemon_pid must create the daemon dir and persist the pid"
        );

        super::super::remove_daemon_pid(&primary_root);
        assert_eq!(super::super::read_daemon_pid(&primary_root), None);
    }

    #[tokio::test]
    async fn daemon_run_once_processes_current_project_root() {
        let _lock = lock_env();

        let config_root = TempDir::new().expect("config temp dir");
        let home_root = TempDir::new().expect("home temp dir");
        let _config_guard = EnvVarGuard::set("ANIMUS_CONFIG_DIR", Some(config_root.path().to_string_lossy().as_ref()));
        let _home_guard = EnvVarGuard::set("HOME", Some(home_root.path().to_string_lossy().as_ref()));
        let _legacy_guard = EnvVarGuard::set("AGENT_ORCHESTRATOR_CONFIG_DIR", None);

        let primary = TempDir::new().expect("primary project dir");
        let primary_root = primary.path().to_string_lossy().to_string();

        let args = DaemonRunArgs {
            scheduler: DaemonSchedulerArgs {
                pool_size: None,
                interval_secs: Some(1),
                startup_cleanup: true,
                reconcile_stale: false,
                stale_threshold_hours: Some(24),
                max_tasks_per_tick: Some(1),
                phase_timeout_secs: None,
            },
            once: true,
            auto_install: false,
            skip_preflight: true,
        };
        handle_daemon_run(args, &primary_root, true).await.expect("daemon run should succeed");

        // A completed `--once` run registers then clears its PID; no stale PID
        // should remain afterward.
        assert_eq!(super::super::read_daemon_pid(&primary_root), None);

        let events_path = daemon_events_log_path();
        let events_content = std::fs::read_to_string(events_path).expect("daemon events log should exist");
        let events: Vec<DaemonEventRecord> = events_content
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| serde_json::from_str::<DaemonEventRecord>(line).expect("event json"))
            .collect();

        let queue_event = events
            .iter()
            .find(|event| {
                event.event_type == "queue"
                    && event.project_root.as_deref() == Some(canonicalize_lossy(&primary_root).as_str())
            })
            .expect("queue event for primary project should exist");
        for field in [
            "stale_in_progress_count",
            "stale_in_progress_threshold_hours",
            "started_ready_workflows",
            "executed_workflow_phases",
            "failed_workflow_phases",
        ] {
            assert!(
                queue_event.data.get(field).and_then(serde_json::Value::as_u64).is_some(),
                "queue event field `{field}` should be present as an integer"
            );
        }
        assert!(
            queue_event.data.get("stale_in_progress_task_ids").and_then(serde_json::Value::as_array).is_some(),
            "queue event field `stale_in_progress_task_ids` should be present as an array"
        );
    }

    #[tokio::test]
    async fn daemon_run_emits_task_state_change_events() {
        let _lock = lock_env();

        let config_root = TempDir::new().expect("config temp dir");
        let home_root = TempDir::new().expect("home temp dir");
        let _config_guard = EnvVarGuard::set("ANIMUS_CONFIG_DIR", Some(config_root.path().to_string_lossy().as_ref()));
        let _home_guard = EnvVarGuard::set("HOME", Some(home_root.path().to_string_lossy().as_ref()));
        let _legacy_guard = EnvVarGuard::set("AGENT_ORCHESTRATOR_CONFIG_DIR", None);

        let primary = TempDir::new().expect("primary project dir");
        let primary_root = primary.path().to_string_lossy().to_string();
        let primary_hub = Arc::new(FileServiceHub::new(&primary_root).expect("primary hub"));
        // v0.6: stand in for the config_source plugin after the hub scaffolds.
        let _config_source_seam =
            orchestrator_config::workflow_config::config_source_client::install_yaml_config_source_base(primary.path());

        let task = primary_hub
            .tasks()
            .create(orchestrator_core::TaskCreateInput {
                title: "transition task".to_string(),
                description: "verify task-state-change daemon events".to_string(),
                task_type: Some(orchestrator_core::TaskType::Feature),
                priority: Some(orchestrator_core::Priority::Medium),
                created_by: Some("test".to_string()),
                tags: Vec::new(),
                linked_requirements: Vec::new(),
                linked_architecture_entities: Vec::new(),
            })
            .await
            .expect("task should be created");

        // Crashed-workflow ordering: the task transitions to InProgress
        // BEFORE its workflow ends. The reconciler only re-transitions tasks
        // whose latest terminal workflow postdates the task's own last
        // transition — setting InProgress after the cancel would look like
        // an operator manually re-working the task and be left alone.
        primary_hub
            .tasks()
            .set_status(&task.id, orchestrator_core::TaskStatus::InProgress, false)
            .await
            .expect("task should be stale in-progress");

        let workflow = primary_hub
            .workflows()
            .run(orchestrator_core::WorkflowRunInput::for_task(task.id.clone(), None), None)
            .await
            .expect("workflow should run");
        // Cancel the workflow so all task workflows are terminal with no success.
        // The stale-in-progress reconciler auto-transitions the task to a
        // terminal state when every workflow failed/cancelled (it never
        // auto-completes tasks). A Cancelled terminal workflow projects the
        // task Cancelled (a Failed/Escalated one would project Blocked).
        let workflow = primary_hub.workflows().cancel(&workflow.id).await.expect("workflow should cancel");
        assert_eq!(workflow.status, orchestrator_core::WorkflowStatus::Cancelled);

        let args = DaemonRunArgs {
            scheduler: DaemonSchedulerArgs {
                pool_size: None,
                interval_secs: Some(1),
                startup_cleanup: false,
                reconcile_stale: true,
                stale_threshold_hours: Some(24),
                max_tasks_per_tick: Some(1),
                phase_timeout_secs: None,
            },
            once: true,
            auto_install: false,
            skip_preflight: true,
        };
        handle_daemon_run(args, &primary_root, true).await.expect("daemon run should emit transition event");

        let events_path = daemon_events_log_path();
        let events_content = std::fs::read_to_string(events_path).expect("daemon events log should exist");
        let events: Vec<DaemonEventRecord> = events_content
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| serde_json::from_str::<DaemonEventRecord>(line).expect("event json"))
            .collect();

        let transition_event = events
            .iter()
            .find(|event| {
                event.event_type == "task-state-change"
                    && event.project_root.as_deref() == Some(canonicalize_lossy(&primary_root).as_str())
                    && event.data.get("task_id").and_then(serde_json::Value::as_str) == Some(task.id.as_str())
            })
            .expect("task-state-change event should be emitted");
        assert_eq!(transition_event.data.get("from_status").and_then(serde_json::Value::as_str), Some("in-progress"));
        assert_eq!(transition_event.data.get("to_status").and_then(serde_json::Value::as_str), Some("cancelled"));
        assert!(transition_event
            .data
            .get("changed_at")
            .and_then(serde_json::Value::as_str)
            .map(|value| !value.trim().is_empty())
            .unwrap_or(false));
    }

    #[tokio::test]
    async fn daemon_run_does_not_dispatch_backend_ready_tasks() {
        let _lock = lock_env();

        let config_root = TempDir::new().expect("config temp dir");
        let home_root = TempDir::new().expect("home temp dir");
        let _config_guard = EnvVarGuard::set("ANIMUS_CONFIG_DIR", Some(config_root.path().to_string_lossy().as_ref()));
        let _home_guard = EnvVarGuard::set("HOME", Some(home_root.path().to_string_lossy().as_ref()));
        let _legacy_guard = EnvVarGuard::set("AGENT_ORCHESTRATOR_CONFIG_DIR", None);

        let test_bin_dir = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.to_path_buf()))
            .expect("test binary directory");
        let release_bin_dir = test_bin_dir.parent().unwrap_or(&test_bin_dir);
        let path_with_bin_dir = format!(
            "{}:{}:{}",
            release_bin_dir.display(),
            test_bin_dir.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let _path_guard = EnvVarGuard::set("PATH", Some(&path_with_bin_dir));

        let primary = TempDir::new().expect("primary project dir");
        let primary_root = primary.path().to_string_lossy().to_string();
        let primary_hub = Arc::new(FileServiceHub::new(&primary_root).expect("primary hub"));
        // v0.6: stand in for the config_source plugin after the hub scaffolds.
        let _config_source_seam =
            orchestrator_config::workflow_config::config_source_client::install_yaml_config_source_base(primary.path());

        let task = primary_hub
            .tasks()
            .create(orchestrator_core::TaskCreateInput {
                title: "start selection source task".to_string(),
                description: "verify daemon emits selection source on workflow start".to_string(),
                task_type: Some(orchestrator_core::TaskType::Feature),
                priority: Some(orchestrator_core::Priority::Medium),
                created_by: Some("test".to_string()),
                tags: Vec::new(),
                linked_requirements: Vec::new(),
                linked_architecture_entities: Vec::new(),
            })
            .await
            .expect("task should be created");
        primary_hub
            .tasks()
            .set_status(&task.id, orchestrator_core::TaskStatus::Ready, false)
            .await
            .expect("task should be ready");

        let args = DaemonRunArgs {
            scheduler: DaemonSchedulerArgs {
                pool_size: None,
                interval_secs: Some(1),
                startup_cleanup: false,
                reconcile_stale: false,
                stale_threshold_hours: Some(24),
                max_tasks_per_tick: Some(1),
                phase_timeout_secs: None,
            },
            once: true,
            auto_install: false,
            skip_preflight: true,
        };
        handle_daemon_run(args, &primary_root, true).await.expect("daemon run should complete");

        // Queue-only model: the daemon must NOT pull a Ready task out of the
        // subject backend. The task is never dispatched — it stays Ready and
        // no task-state-change
        // selection-source event is emitted for it. Ingestion into the queue
        // is the end user's responsibility.
        let events_path = daemon_events_log_path();
        let events: Vec<DaemonEventRecord> = std::fs::read_to_string(events_path)
            .unwrap_or_default()
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| serde_json::from_str::<DaemonEventRecord>(line).expect("event json"))
            .collect();

        let dispatched = events.iter().any(|event| {
            event.event_type == "task-state-change"
                && event.project_root.as_deref() == Some(canonicalize_lossy(&primary_root).as_str())
                && event.data.get("task_id").and_then(serde_json::Value::as_str) == Some(task.id.as_str())
                && event.data.get("selection_source").and_then(serde_json::Value::as_str).is_some()
        });
        assert!(!dispatched, "daemon must not auto-dispatch a backend Ready task in the queue-only model");

        let reloaded = primary_hub.tasks().get(&task.id).await.expect("task still present");
        assert_eq!(
            reloaded.status,
            orchestrator_core::TaskStatus::Ready,
            "Ready task must remain Ready (not dispatched)"
        );
    }

    /// Notification delivery moved out-of-tree to the `notifier` plugin
    /// kind in v0.5.3 (Task D). When no notifier plugin is installed the
    /// daemon must start, run a tick, and stop cleanly even though the
    /// project has a configured `notification_config` block — the
    /// dispatcher silently skips delivery instead of producing a
    /// dead-letter event. This test guards against regressions that
    /// would couple daemon liveness to notifier presence again.
    #[tokio::test]
    async fn daemon_run_continues_when_notification_delivery_fails() {
        let _lock = lock_env();

        let config_root = TempDir::new().expect("config temp dir");
        let home_root = TempDir::new().expect("home temp dir");
        let _config_guard = EnvVarGuard::set("ANIMUS_CONFIG_DIR", Some(config_root.path().to_string_lossy().as_ref()));
        let _home_guard = EnvVarGuard::set("HOME", Some(home_root.path().to_string_lossy().as_ref()));
        let _legacy_guard = EnvVarGuard::set("AGENT_ORCHESTRATOR_CONFIG_DIR", None);
        let _missing_url = EnvVarGuard::set("ANIMUS_NOTIFY_MISSING_URL", None);

        let primary = TempDir::new().expect("primary project dir");
        let primary_root = primary.path().to_string_lossy().to_string();

        let pm_config_path = orchestrator_core::daemon_project_config_path(std::path::Path::new(&primary_root));
        std::fs::create_dir_all(pm_config_path.parent().expect("pm-config path should have parent"))
            .expect("scoped daemon config directory should be created");
        let pm_config = serde_json::json!({
            "notification_config": {
                "schema": "animus.daemon-notification-config.v1",
                "version": 1,
                "connectors": [
                    {
                        "type": "webhook",
                        "id": "ops-webhook",
                        "enabled": true,
                        "url_env": "ANIMUS_NOTIFY_MISSING_URL"
                    }
                ],
                "subscriptions": [
                    {
                        "id": "all-events",
                        "enabled": true,
                        "connector_id": "ops-webhook",
                        "event_types": ["*"]
                    }
                ],
                "retry_policy": {
                    "max_attempts": 1,
                    "base_delay_secs": 1,
                    "max_delay_secs": 5
                },
                "max_deliveries_per_tick": 8
            }
        });
        std::fs::write(
            &pm_config_path,
            format!("{}\n", serde_json::to_string_pretty(&pm_config).expect("serialize config")),
        )
        .expect("pm-config should be written");

        let args = DaemonRunArgs {
            scheduler: DaemonSchedulerArgs {
                pool_size: None,
                interval_secs: Some(1),
                startup_cleanup: true,
                reconcile_stale: false,
                stale_threshold_hours: Some(24),
                max_tasks_per_tick: Some(1),
                phase_timeout_secs: None,
            },
            once: true,
            auto_install: false,
            skip_preflight: true,
        };
        handle_daemon_run(args, &primary_root, true)
            .await
            .expect("daemon run should succeed even when notification delivery fails");

        let events_path = daemon_events_log_path();
        let events_content = std::fs::read_to_string(events_path).expect("daemon events log should exist");
        let events: Vec<DaemonEventRecord> = events_content
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| serde_json::from_str::<DaemonEventRecord>(line).expect("event json"))
            .collect();

        // The daemon must reach `status:running` (kept alive past startup)
        // and `status:stopped` (clean shutdown after `--once`) regardless
        // of whether a notifier plugin is installed.
        assert!(events.iter().any(|event| event.event_type == "status" && event.data["status"] == "running"));
        assert!(events.iter().any(|event| event.event_type == "status" && event.data["status"] == "stopped"));
    }

    #[test]
    fn daemon_run_does_not_clobber_persisted_config_when_omitted() {
        let _lock = lock_env();

        let config_root = TempDir::new().expect("config temp dir");
        let home_root = TempDir::new().expect("home temp dir");
        let _config_guard = EnvVarGuard::set("ANIMUS_CONFIG_DIR", Some(config_root.path().to_string_lossy().as_ref()));
        let _home_guard = EnvVarGuard::set("HOME", Some(home_root.path().to_string_lossy().as_ref()));
        let _legacy_guard = EnvVarGuard::set("AGENT_ORCHESTRATOR_CONFIG_DIR", None);

        let project_root = TempDir::new().expect("project dir");
        let config = orchestrator_core::DaemonProjectConfig {
            interval_secs: Some(11),
            max_tasks_per_tick: Some(7),
            stale_threshold_hours: Some(42),
            ..Default::default()
        };
        orchestrator_core::write_daemon_project_config(project_root.path(), &config).expect("seed daemon config");

        let args = DaemonRunArgs {
            scheduler: DaemonSchedulerArgs {
                pool_size: None,
                interval_secs: None,
                startup_cleanup: true,
                reconcile_stale: true,
                stale_threshold_hours: None,
                max_tasks_per_tick: None,
                phase_timeout_secs: None,
            },
            once: true,
            auto_install: false,
            skip_preflight: true,
        };

        apply_scheduler_overrides_to_pm_config(&args, project_root.path().to_string_lossy().as_ref());

        let loaded = orchestrator_core::load_daemon_project_config(project_root.path()).expect("load daemon config");
        assert_eq!(loaded.interval_secs, Some(11));
        assert_eq!(loaded.max_tasks_per_tick, Some(7));
        assert_eq!(loaded.stale_threshold_hours, Some(42));
    }

    mod auto_install_request {
        use crate::services::operations::PluginInstallRequest;

        /// Regression for codex P1 2026-05-25: daemon auto-install / preflight
        /// targets curated launchapp-dev provider repos whose manifest names
        /// (claude / codex / gemini / opencode) hit the reserved-tool guard.
        /// `CliPluginInstaller::install` MUST opt into the bypass so
        /// `AtLeastOneProvider` preflight can be satisfied.
        #[test]
        fn daemon_auto_install_request_bypasses_reserved_name_guard() {
            let req = PluginInstallRequest {
                source: Some("launchapp-dev/animus-provider-claude".to_string()),
                yes: true,
                allow_shadow_builtin: true,
                ..Default::default()
            };
            assert!(req.allow_shadow_builtin, "daemon auto-install MUST bypass shadow-builtin guard");
            assert!(req.yes, "daemon auto-install MUST auto-confirm TOFU (non-interactive)");
            assert_eq!(req.source.as_deref(), Some("launchapp-dev/animus-provider-claude"));
        }

        #[test]
        fn default_user_install_request_does_not_bypass_guard() {
            let req = PluginInstallRequest::default();
            assert!(!req.allow_shadow_builtin, "default user request MUST keep the reserved-name guard active");
            assert!(!req.yes, "default user request MUST keep TOFU prompts on");
        }
    }

    mod resumable_orphan_shielding {
        use super::super::resumable_workflow_ids_for_project;
        use super::*;
        use animus_runtime_shared::phase_session::{
            update_provider_session_id, update_session_running, write_session_pending,
        };
        use serde_json::json;

        // Codex round-4 P2: daemon restart MUST shield workflows with a
        // resumable provider session id from the orphan-recovery sweep.
        // Otherwise recover_orphaned_running_workflows cancels them before
        // auto_resume_running_checkpoints runs, and resume has nothing to
        // recover.
        #[tokio::test]
        async fn daemon_restart_resumes_workflow_with_provider_session_id_instead_of_cancelling() {
            let _lock = lock_env();
            let home_root = TempDir::new().expect("home temp dir");
            let config_root = TempDir::new().expect("config temp dir");
            let _config_guard =
                EnvVarGuard::set("ANIMUS_CONFIG_DIR", Some(config_root.path().to_string_lossy().as_ref()));
            let _home_guard = EnvVarGuard::set("HOME", Some(home_root.path().to_string_lossy().as_ref()));
            let _legacy_guard = EnvVarGuard::set("AGENT_ORCHESTRATOR_CONFIG_DIR", None);

            let project = TempDir::new().expect("project temp dir");
            let project_root = project.path().to_string_lossy().to_string();

            let scoped_root =
                protocol::repository_scope::scoped_state_root(project.path()).expect("scoped state root for project");

            // Seed two checkpoints: one with a provider_session_id (resumable),
            // one without (un-resumable). Only the first should appear in the
            // shield set.
            write_session_pending(
                &scoped_root,
                "wf-resumable",
                "impl",
                "claude",
                "run-resumable",
                Some(json!({"protocol_version": "1", "run_id": "run-resumable"})),
            )
            .expect("seed resumable pending");
            update_session_running(&scoped_root, "wf-resumable", "impl").expect("seed resumable running");
            update_provider_session_id(&scoped_root, "wf-resumable", "impl", "sess-real").expect("seed provider sid");

            write_session_pending(
                &scoped_root,
                "wf-noid",
                "impl",
                "claude",
                "run-noid",
                Some(json!({"protocol_version": "1", "run_id": "run-noid"})),
            )
            .expect("seed noid pending");
            update_session_running(&scoped_root, "wf-noid", "impl").expect("seed noid running");

            let shielded = resumable_workflow_ids_for_project(&project_root);
            assert!(
                shielded.contains("wf-resumable"),
                "workflow with provider_session_id MUST be shielded from orphan cancellation"
            );
            assert!(
                !shielded.contains("wf-noid"),
                "workflow without provider_session_id MUST NOT be shielded (nothing to resume)"
            );
            assert_eq!(shielded.len(), 1, "shield set contains exactly the resumable workflows");
        }

        #[test]
        fn resumable_set_is_empty_when_no_checkpoints_exist() {
            let _lock = lock_env();
            let home_root = TempDir::new().expect("home temp dir");
            let _home_guard = EnvVarGuard::set("HOME", Some(home_root.path().to_string_lossy().as_ref()));

            let project = TempDir::new().expect("project temp dir");
            let project_root = project.path().to_string_lossy().to_string();

            let shielded = resumable_workflow_ids_for_project(&project_root);
            assert!(shielded.is_empty(), "no checkpoints => empty shield set => orphan recovery proceeds normally");
        }
    }

    mod auto_resume {
        use super::super::{
            auto_resume_running_checkpoints, ResumeLookup, ResumeProviderRegistry, ResumedOutcomeApplier,
            ResumedOutcomeApplyResult,
        };
        use animus_runtime_shared::phase_session::{
            read_checkpoint, update_provider_session_id, update_session_running, update_session_running_after_resume,
            write_session_pending, SessionCheckpoint, SessionCheckpointStatus,
        };
        use async_trait::async_trait;
        use orchestrator_plugin_host::session::ResumeAgentOutcome;
        use serde_json::json;
        use std::collections::HashMap;
        use std::sync::Mutex;
        use tempfile::TempDir;

        enum Script {
            Resumed { new_session_id: Option<String> },
            Failed { reason: String },
        }

        struct ScriptedRegistry {
            scripts: Mutex<HashMap<String, Script>>,
        }

        impl ScriptedRegistry {
            fn new(entries: Vec<(&str, Script)>) -> Self {
                let scripts = entries.into_iter().map(|(k, v)| (k.to_string(), v)).collect();
                Self { scripts: Mutex::new(scripts) }
            }
        }

        #[async_trait(?Send)]
        impl ResumeProviderRegistry for ScriptedRegistry {
            async fn try_resume(&self, checkpoint: &SessionCheckpoint) -> ResumeLookup {
                let mut guard = self.scripts.lock().expect("scripts mutex");
                match guard.remove(&checkpoint.provider) {
                    Some(Script::Resumed { new_session_id }) => {
                        ResumeLookup::Outcome(ResumeAgentOutcome::Resumed { session_id: new_session_id })
                    }
                    Some(Script::Failed { reason }) => ResumeLookup::Outcome(ResumeAgentOutcome::Failed { reason }),
                    None => ResumeLookup::NotInstalled,
                }
            }
        }

        // Test applier that records calls and returns a configurable
        // result. For the older dispatch-surface tests we mimic the
        // pre-fix behaviour (rewrite checkpoint as Running) so we don't
        // need to wire a full ServiceHub for tests that only care about
        // the resume-provider dispatch path. New durability tests pass a
        // result that flips the checkpoint to Completed.
        struct RecordingApplier {
            calls: Mutex<Vec<(String, String, Option<String>)>>,
            result: ResumedOutcomeApplyResult,
            // When set, also rotate provider_session_id on the checkpoint
            // (kept Running) so legacy dispatch-surface assertions still
            // see the rotated id. The newer apply-path tests do not need
            // this and should leave it `false`.
            rewrite_checkpoint_running: bool,
            scoped_root: std::path::PathBuf,
        }

        impl RecordingApplier {
            fn new(
                result: ResumedOutcomeApplyResult,
                rewrite_checkpoint_running: bool,
                scoped_root: std::path::PathBuf,
            ) -> Self {
                Self { calls: Mutex::new(Vec::new()), result, rewrite_checkpoint_running, scoped_root }
            }

            fn calls(&self) -> Vec<(String, String, Option<String>)> {
                self.calls.lock().expect("calls mutex").clone()
            }
        }

        #[async_trait(?Send)]
        impl ResumedOutcomeApplier for RecordingApplier {
            async fn apply(
                &self,
                checkpoint: &SessionCheckpoint,
                new_session_id: Option<&str>,
            ) -> ResumedOutcomeApplyResult {
                self.calls.lock().expect("calls mutex").push((
                    checkpoint.workflow_id.clone(),
                    checkpoint.phase_id.clone(),
                    new_session_id.map(str::to_string),
                ));
                if self.rewrite_checkpoint_running {
                    let _ = update_session_running_after_resume(
                        &self.scoped_root,
                        &checkpoint.workflow_id,
                        &checkpoint.phase_id,
                        new_session_id,
                    );
                }
                self.result.clone()
            }
        }

        fn seed_running_checkpoint(scoped: &std::path::Path, workflow_id: &str, phase_id: &str, provider: &str) {
            write_session_pending(
                scoped,
                workflow_id,
                phase_id,
                provider,
                "run-1",
                Some(json!({
                    "protocol_version": "1",
                    "run_id": "run-1",
                    "model": "claude-sonnet-4-6",
                    "context": {
                        "tool": provider,
                        "prompt": "continue",
                        "cwd": "/tmp",
                        "project_root": "/tmp"
                    }
                })),
            )
            .expect("seed pending");
            update_session_running(scoped, workflow_id, phase_id).expect("seed running");
            update_provider_session_id(scoped, workflow_id, phase_id, "sess-original").expect("seed provider sid");
        }

        #[tokio::test]
        async fn auto_resume_succeeds_when_provider_returns_resumed_session() {
            let temp = TempDir::new().expect("tempdir");
            let scoped = temp.path();
            seed_running_checkpoint(scoped, "wf-ok", "impl", "claude");
            let registry = ScriptedRegistry::new(vec![(
                "claude",
                Script::Resumed { new_session_id: Some("sess-new".to_string()) },
            )]);
            let applier = RecordingApplier::new(ResumedOutcomeApplyResult::Advanced, true, scoped.to_path_buf());

            let report = auto_resume_running_checkpoints(scoped, &registry, &applier).await;
            assert_eq!(report.resumed, 1);
            assert_eq!(report.blocked_on_failure, 0);
            assert_eq!(report.blocked_uninstalled, 0);

            let after = read_checkpoint(scoped, "wf-ok", "impl").expect("read").expect("present");
            assert_eq!(after.status, SessionCheckpointStatus::Running);
            assert_eq!(after.provider_session_id.as_deref(), Some("sess-new"));
            assert!(after.blocked_reason.is_none());
            assert_eq!(applier.calls(), vec![("wf-ok".to_string(), "impl".to_string(), Some("sess-new".to_string()))]);
        }

        #[tokio::test]
        async fn auto_resume_marks_blocked_with_specific_error_when_provider_returns_session_expired() {
            let temp = TempDir::new().expect("tempdir");
            let scoped = temp.path();
            seed_running_checkpoint(scoped, "wf-expired", "review", "codex");
            let registry = ScriptedRegistry::new(vec![(
                "codex",
                Script::Failed { reason: "resume_agent failed: session expired".to_string() },
            )]);
            let applier = RecordingApplier::new(ResumedOutcomeApplyResult::Advanced, false, scoped.to_path_buf());

            let report = auto_resume_running_checkpoints(scoped, &registry, &applier).await;
            assert_eq!(report.resumed, 0);
            assert_eq!(report.blocked_on_failure, 1);
            assert_eq!(report.blocked_uninstalled, 0);

            let after = read_checkpoint(scoped, "wf-expired", "review").expect("read").expect("present");
            assert_eq!(after.status, SessionCheckpointStatus::Blocked);
            let reason = after.blocked_reason.unwrap_or_default();
            assert!(
                reason.contains("session expired") && reason.contains("resume_agent failed"),
                "expected specific resume failure reason, got: {reason}"
            );
            assert!(applier.calls().is_empty(), "applier must NOT be invoked when resume fails");
        }

        #[tokio::test]
        async fn auto_resume_blocks_when_provider_session_id_not_captured_before_crash() {
            let temp = TempDir::new().expect("tempdir");
            let scoped = temp.path();
            // Seed a Running checkpoint that NEVER captured a provider_session_id
            // (simulates crash between dispatch and the plugin's first response).
            write_session_pending(
                scoped,
                "wf-crashed",
                "impl",
                "claude",
                "run-crashed",
                Some(json!({
                    "protocol_version": "1",
                    "run_id": "run-crashed",
                    "model": "claude-sonnet-4-6",
                    "context": {"tool": "claude", "prompt": "x", "cwd": "/tmp", "project_root": "/tmp"}
                })),
            )
            .expect("pending");
            update_session_running(scoped, "wf-crashed", "impl").expect("running");

            let registry = ScriptedRegistry::new(vec![(
                "claude",
                // Script must NOT be consumed: try_resume should short-circuit
                // before dispatching to the plugin when provider_session_id
                // is None.
                Script::Resumed { new_session_id: Some("sess-should-never-arrive".to_string()) },
            )]);
            let applier = RecordingApplier::new(ResumedOutcomeApplyResult::Advanced, false, scoped.to_path_buf());
            let report = auto_resume_running_checkpoints(scoped, &registry, &applier).await;
            assert_eq!(report.resumed, 0);
            assert_eq!(report.blocked_on_failure, 1);

            let after = read_checkpoint(scoped, "wf-crashed", "impl").expect("read").expect("present");
            assert_eq!(after.status, SessionCheckpointStatus::Blocked);
            let reason = after.blocked_reason.unwrap_or_default();
            assert!(
                reason.contains("no provider session_id captured") && reason.contains("--force"),
                "expected explicit 'no provider session_id captured' guidance, got: {reason}"
            );
            assert!(applier.calls().is_empty(), "applier must NOT be invoked when provider_session_id is missing");
        }

        #[tokio::test]
        async fn auto_resume_passes_correct_provider_session_id_to_resume_agent() {
            let temp = TempDir::new().expect("tempdir");
            let scoped = temp.path();
            seed_running_checkpoint(scoped, "wf-correct", "impl", "claude");
            // ScriptedRegistry distinguishes by provider name; assert that
            // the resumed outcome propagates a fresh session_id, confirming
            // the production `try_resume` reached the plugin (i.e. it did
            // NOT short-circuit on a missing provider_session_id).
            let registry = ScriptedRegistry::new(vec![(
                "claude",
                Script::Resumed { new_session_id: Some("sess-rotated".to_string()) },
            )]);
            let applier = RecordingApplier::new(ResumedOutcomeApplyResult::Advanced, true, scoped.to_path_buf());
            let report = auto_resume_running_checkpoints(scoped, &registry, &applier).await;
            assert_eq!(report.resumed, 1);
            let after = read_checkpoint(scoped, "wf-correct", "impl").expect("read").expect("present");
            assert_eq!(after.provider_session_id.as_deref(), Some("sess-rotated"));
            assert_ne!(
                after.provider_session_id.as_deref(),
                Some(after.run_id.as_str()),
                "auto-resume must NOT confuse run_id with provider_session_id"
            );
            assert_eq!(
                applier.calls(),
                vec![("wf-correct".to_string(), "impl".to_string(), Some("sess-rotated".to_string()))]
            );
        }

        #[tokio::test]
        async fn auto_resume_marks_blocked_with_reinstall_hint_when_provider_uninstalled() {
            let temp = TempDir::new().expect("tempdir");
            let scoped = temp.path();
            seed_running_checkpoint(scoped, "wf-gone", "design", "gemini");
            let registry = ScriptedRegistry::new(vec![]);
            let applier = RecordingApplier::new(ResumedOutcomeApplyResult::Advanced, false, scoped.to_path_buf());

            let report = auto_resume_running_checkpoints(scoped, &registry, &applier).await;
            assert_eq!(report.resumed, 0);
            assert_eq!(report.blocked_on_failure, 0);
            assert_eq!(report.blocked_uninstalled, 1);

            let after = read_checkpoint(scoped, "wf-gone", "design").expect("read").expect("present");
            assert_eq!(after.status, SessionCheckpointStatus::Blocked);
            let reason = after.blocked_reason.unwrap_or_default();
            assert!(
                reason.contains("provider 'gemini' not installed")
                    && reason.contains("animus plugin install launchapp-dev/animus-provider-gemini"),
                "expected reinstall hint, got: {reason}"
            );
            assert!(applier.calls().is_empty(), "applier must NOT be invoked when provider plugin is missing");
        }

        // Codex round-7 P1 regression: when the resumed agent drains
        // successfully but the applier fails (e.g. workflow service can't
        // load the workflow), the checkpoint must be marked Blocked with a
        // descriptive reason so the next restart doesn't loop on the same
        // stuck Running checkpoint.
        #[tokio::test]
        async fn auto_resume_marks_blocked_when_applier_fails_after_successful_drain() {
            let temp = TempDir::new().expect("tempdir");
            let scoped = temp.path();
            seed_running_checkpoint(scoped, "wf-apply-fail", "impl", "claude");
            let registry = ScriptedRegistry::new(vec![(
                "claude",
                Script::Resumed { new_session_id: Some("sess-new".to_string()) },
            )]);
            let applier = RecordingApplier::new(
                ResumedOutcomeApplyResult::Failed("workflow service offline".to_string()),
                false,
                scoped.to_path_buf(),
            );

            let report = auto_resume_running_checkpoints(scoped, &registry, &applier).await;
            assert_eq!(report.resumed, 0);
            assert_eq!(report.blocked_on_failure, 1);

            let after = read_checkpoint(scoped, "wf-apply-fail", "impl").expect("read").expect("present");
            assert_eq!(after.status, SessionCheckpointStatus::Blocked);
            let reason = after.blocked_reason.unwrap_or_default();
            assert!(
                reason.contains("resume succeeded but applying outcome failed")
                    && reason.contains("workflow service offline"),
                "expected combined apply-failure reason, got: {reason}"
            );
        }

        // Codex round-7 P1: idempotent re-apply. When a previous restart
        // already advanced the workflow past this phase (durable marker
        // carried it forward) the apply must NOT double-advance — it
        // should report AlreadyAdvanced and the resume report still counts
        // it as resumed (i.e. successfully recovered).
        #[tokio::test]
        async fn auto_resume_treats_already_advanced_as_resumed() {
            let temp = TempDir::new().expect("tempdir");
            let scoped = temp.path();
            seed_running_checkpoint(scoped, "wf-already", "impl", "claude");
            let registry = ScriptedRegistry::new(vec![(
                "claude",
                Script::Resumed { new_session_id: Some("sess-new".to_string()) },
            )]);
            let applier =
                RecordingApplier::new(ResumedOutcomeApplyResult::AlreadyAdvanced, false, scoped.to_path_buf());

            let report = auto_resume_running_checkpoints(scoped, &registry, &applier).await;
            assert_eq!(report.resumed, 1, "AlreadyAdvanced still counts as a successful resume recovery");
            assert_eq!(report.blocked_on_failure, 0);
        }

        // Codex round 8 P2 #2: historical workflows wrote
        // `checkpoint.provider = "oai-runner"` (or `"animus-oai-runner"`),
        // but the installed first-party plugin registers under
        // `provider_tool = "oai"`. The restart-resume lookup must
        // canonicalize the alias before the map lookup; otherwise
        // restart-recovery mis-classifies a present plugin as
        // `NotInstalled` and shoves the checkpoint into Blocked with a
        // reinstall hint that wouldn't help.
        #[test]
        fn restart_resume_finds_oai_plugin_via_canonical_alias_when_checkpoint_says_oai_runner() {
            use super::super::resolve_resume_provider_key;
            // Post-PR#249 the legacy `oai-runner` alias canonicalizes to the
            // agentic `oai-agent` provider (the one-shot is `oai`).
            let providers = vec!["oai-agent".to_string()];
            assert_eq!(
                resolve_resume_provider_key(&providers, "oai-runner").as_deref(),
                Some("oai-agent"),
                "checkpoint provider 'oai-runner' must canonicalize to installed key 'oai-agent'"
            );
        }

        #[test]
        fn restart_resume_finds_oai_plugin_when_checkpoint_says_animus_oai_runner() {
            use super::super::resolve_resume_provider_key;
            let providers = vec!["oai-agent".to_string()];
            assert_eq!(
                resolve_resume_provider_key(&providers, "animus-oai-runner").as_deref(),
                Some("oai-agent"),
                "checkpoint provider 'animus-oai-runner' must canonicalize to installed key 'oai-agent'"
            );
        }

        #[test]
        fn restart_resume_canonical_alias_lookup_returns_none_when_plugin_absent() {
            use super::super::resolve_resume_provider_key;
            // No oai-family plugin installed at all: must classify as
            // NotInstalled even after canonicalization, so the production
            // code falls through to the Blocked/uninstalled branch.
            let providers = vec!["claude".to_string(), "gemini".to_string()];
            assert!(resolve_resume_provider_key(&providers, "oai-runner").is_none());
            assert!(resolve_resume_provider_key(&providers, "animus-oai-runner").is_none());
            assert!(resolve_resume_provider_key(&providers, "oai").is_none());
        }

        #[test]
        fn restart_resume_canonical_alias_lookup_is_case_insensitive() {
            use super::super::resolve_resume_provider_key;
            let providers = vec!["oai-agent".to_string()];
            // Mirror the resolver: canonicalization lowercases the input
            // before comparing, so uppercase aliases must still match.
            assert_eq!(resolve_resume_provider_key(&providers, "OAI-RUNNER").as_deref(), Some("oai-agent"));
            assert_eq!(resolve_resume_provider_key(&providers, "Animus-OAI-Runner").as_deref(), Some("oai-agent"));
        }

        #[test]
        fn restart_resume_canonical_alias_passes_through_non_aliased_providers() {
            use super::super::resolve_resume_provider_key;
            // Regression guard: non-aliased tools (claude, codex, gemini, ...)
            // must still lookup against their literal lowercase key after
            // canonicalization. Don't accidentally remap them through the
            // oai-runner branch.
            let providers = vec!["claude".to_string(), "codex".to_string(), "gemini".to_string()];
            assert_eq!(resolve_resume_provider_key(&providers, "claude").as_deref(), Some("claude"));
            assert_eq!(resolve_resume_provider_key(&providers, "Codex").as_deref(), Some("codex"));
            assert_eq!(resolve_resume_provider_key(&providers, "GEMINI").as_deref(), Some("gemini"));
        }
    }

    mod apply_resumed_outcome {
        use super::super::{apply_resumed_outcome_in_tree, ResumedOutcomeApplyResult};
        use animus_runtime_shared::phase_session::{
            read_checkpoint, update_provider_session_id, update_session_running, write_session_pending,
            SessionCheckpointStatus,
        };
        use animus_runtime_shared::{is_phase_completed, read_persisted_decision};
        use orchestrator_core::{services::ServiceHub, FileServiceHub};
        use protocol::test_utils::EnvVarGuard;
        use serde_json::json;
        use std::sync::{Arc, MutexGuard};
        use tempfile::TempDir;

        // Serialize on the crate-wide env lock — these tests mutate HOME /
        // ANIMUS_CONFIG_DIR, which are process-global, so a private mutex
        // would still race sibling modules (and the memory-tool tests that
        // read HOME-derived scope).
        fn lock_env() -> MutexGuard<'static, ()> {
            crate::shared::test_env_lock().lock().unwrap_or_else(|p| p.into_inner())
        }

        // Seed a workflow + checkpoint for a SAFE-to-auto-advance phase.
        // The standard pipeline's first phase (`requirements`) is gated by
        // a default decision_contract, so we use the `research-workflow`
        // template (phases: [requirements, research]) and advance past
        // `requirements` so the checkpoint lands on `research` — a phase
        // with no decision_contract and no requires_commit flag.
        async fn seed_workflow_and_safe_phase_checkpoint(
            project_root: &str,
            scoped_root: &std::path::Path,
        ) -> (Arc<dyn ServiceHub>, String, String) {
            seed_workflow_and_checkpoint_on_phase(project_root, scoped_root, Some("research-workflow"), &["research"])
                .await
        }

        // Seed a workflow + checkpoint targeting any one of the given
        // phase_ids. Advances through earlier phases via
        // `complete_current_phase_with_decision(None)` so the seeded
        // checkpoint lines up with workflow.current_phase. This bypasses
        // the apply path's decision-gating because the workflow service's
        // direct mutation method does not enforce it.
        async fn seed_workflow_and_checkpoint_on_phase(
            project_root: &str,
            scoped_root: &std::path::Path,
            workflow_ref: Option<&str>,
            preferred_phases: &[&str],
        ) -> (Arc<dyn ServiceHub>, String, String) {
            let hub: Arc<dyn ServiceHub> = Arc::new(FileServiceHub::new(project_root).expect("hub"));
            let task = hub
                .tasks()
                .create(orchestrator_core::TaskCreateInput {
                    title: "resume durability".to_string(),
                    description: "verify resumed outcome application".to_string(),
                    task_type: Some(orchestrator_core::TaskType::Feature),
                    priority: Some(orchestrator_core::Priority::Medium),
                    created_by: Some("test".to_string()),
                    tags: Vec::new(),
                    linked_requirements: Vec::new(),
                    linked_architecture_entities: Vec::new(),
                })
                .await
                .expect("create task");
            let mut workflow = hub
                .workflows()
                .run(
                    orchestrator_core::WorkflowRunInput::for_task(task.id.clone(), workflow_ref.map(String::from)),
                    None,
                )
                .await
                .expect("run workflow");

            let mut hops = 0usize;
            loop {
                let phase = workflow.current_phase.clone().unwrap_or_default();
                if preferred_phases.iter().any(|p| *p == phase) {
                    break;
                }
                assert!(
                    hops <= 12,
                    "workflow never reached one of {:?}; current phase={phase} all={:?}",
                    preferred_phases,
                    workflow.phases
                );
                workflow = hub
                    .workflows()
                    .complete_current_phase_with_decision(&workflow.id, None)
                    .await
                    .expect("advance workflow");
                hops += 1;
            }
            let phase_id = workflow.current_phase.clone().expect("workflow should have a current phase");

            write_session_pending(
                scoped_root,
                &workflow.id,
                &phase_id,
                "claude",
                "run-resume-1",
                Some(json!({
                    "protocol_version": "1",
                    "run_id": "run-resume-1",
                    "model": "claude-sonnet-4-6",
                    "context": {"tool": "claude", "prompt": "x", "cwd": "/tmp", "project_root": "/tmp"}
                })),
            )
            .expect("pending");
            update_session_running(scoped_root, &workflow.id, &phase_id).expect("running");
            update_provider_session_id(scoped_root, &workflow.id, &phase_id, "sess-original").expect("provider sid");

            (hub, workflow.id, phase_id)
        }

        // Codex round-7 P1: successful drain MUST persist the phase output
        // (so `is_phase_completed` returns true and the next scheduler tick
        // can replay the decision) AND flip the session checkpoint to
        // Completed.
        #[tokio::test]
        async fn auto_resume_persists_phase_output_after_successful_drain() {
            let _lock = lock_env();
            let config_root = TempDir::new().expect("config temp dir");
            let home_root = TempDir::new().expect("home temp dir");
            let _config_guard =
                EnvVarGuard::set("ANIMUS_CONFIG_DIR", Some(config_root.path().to_string_lossy().as_ref()));
            let _home_guard = EnvVarGuard::set("HOME", Some(home_root.path().to_string_lossy().as_ref()));
            let _legacy_guard = EnvVarGuard::set("AGENT_ORCHESTRATOR_CONFIG_DIR", None);

            let project = TempDir::new().expect("project temp dir");
            let project_root = project.path().to_string_lossy().to_string();
            let _config_source_seam =
                orchestrator_config::workflow_config::config_source_client::install_yaml_config_source_base(
                    project.path(),
                );
            let scoped_root = protocol::repository_scope::scoped_state_root(project.path()).expect("scoped root");

            let (hub, workflow_id, phase_id) =
                seed_workflow_and_safe_phase_checkpoint(&project_root, &scoped_root).await;

            let checkpoint =
                read_checkpoint(&scoped_root, &workflow_id, &phase_id).expect("read").expect("checkpoint present");

            // Look up the workflow's current attempt for this phase BEFORE
            // applying, so we can assert the marker was written at the
            // correct attempt index (bootstrap sets first phase attempt=1).
            let workflow_before = hub.workflows().get(&workflow_id).await.expect("workflow before");
            let attempt =
                workflow_before.phases.iter().find(|p| p.phase_id == phase_id).map(|p| p.attempt).unwrap_or(0);

            let result = apply_resumed_outcome_in_tree(
                &project_root,
                &scoped_root,
                hub.clone(),
                &checkpoint,
                Some("sess-rotated"),
            )
            .await;
            assert_eq!(result, ResumedOutcomeApplyResult::Advanced);

            let after = read_checkpoint(&scoped_root, &workflow_id, &phase_id).expect("read").expect("present");
            assert_eq!(after.status, SessionCheckpointStatus::Completed);
            assert_eq!(after.provider_session_id.as_deref(), Some("sess-rotated"));
            assert!(after.completed_at.is_some(), "completed_at must be stamped");

            assert!(
                is_phase_completed(&project_root, &workflow_id, &phase_id, attempt),
                "completion marker for attempt {attempt} must exist"
            );
            let decision = read_persisted_decision(&project_root, &workflow_id, &phase_id).expect("decision");
            assert_eq!(decision.verdict, orchestrator_core::PhaseDecisionVerdict::Advance);
        }

        // Codex round-7 P1: successful apply must transition workflow state
        // (current_phase advances OR workflow becomes terminal). Codex
        // round-7 round-3 P1: if the advance lands on a NEW Running phase
        // (non-terminal) the workflow must end up Paused, not Running, so
        // the orphan sweep does not cancel it.
        #[tokio::test]
        async fn auto_resume_transitions_workflow_state_after_drain() {
            let _lock = lock_env();
            let config_root = TempDir::new().expect("config temp dir");
            let home_root = TempDir::new().expect("home temp dir");
            let _config_guard =
                EnvVarGuard::set("ANIMUS_CONFIG_DIR", Some(config_root.path().to_string_lossy().as_ref()));
            let _home_guard = EnvVarGuard::set("HOME", Some(home_root.path().to_string_lossy().as_ref()));
            let _legacy_guard = EnvVarGuard::set("AGENT_ORCHESTRATOR_CONFIG_DIR", None);

            let project = TempDir::new().expect("project temp dir");
            let project_root = project.path().to_string_lossy().to_string();
            let _config_source_seam =
                orchestrator_config::workflow_config::config_source_client::install_yaml_config_source_base(
                    project.path(),
                );
            let scoped_root = protocol::repository_scope::scoped_state_root(project.path()).expect("scoped root");

            let (hub, workflow_id, phase_id) =
                seed_workflow_and_safe_phase_checkpoint(&project_root, &scoped_root).await;

            let before = hub.workflows().get(&workflow_id).await.expect("workflow before");
            let before_phase = before.current_phase.clone();
            let before_index = before.current_phase_index;

            let checkpoint =
                read_checkpoint(&scoped_root, &workflow_id, &phase_id).expect("read").expect("checkpoint present");
            let result =
                apply_resumed_outcome_in_tree(&project_root, &scoped_root, hub.clone(), &checkpoint, Some("sess-x"))
                    .await;
            assert_eq!(result, ResumedOutcomeApplyResult::Advanced);

            let after = hub.workflows().get(&workflow_id).await.expect("workflow after");
            let advanced = after.current_phase_index > before_index
                || after.current_phase.as_deref() != before_phase.as_deref()
                || matches!(
                    after.status,
                    orchestrator_core::WorkflowStatus::Completed | orchestrator_core::WorkflowStatus::Failed
                );
            assert!(
                advanced,
                "workflow state should have advanced past phase '{phase_id}'; before idx={before_index} after idx={} status={:?}",
                after.current_phase_index, after.status
            );

            // Workflow must NOT be left in Running state without a runner —
            // either terminal (Completed/Failed) or Paused so the orphan
            // sweep skips it. research-workflow's research is the last
            // phase so completion is terminal; broader workflows would
            // land on Paused.
            assert!(
                matches!(
                    after.status,
                    orchestrator_core::WorkflowStatus::Completed
                        | orchestrator_core::WorkflowStatus::Cancelled
                        | orchestrator_core::WorkflowStatus::Failed
                        | orchestrator_core::WorkflowStatus::Escalated
                        | orchestrator_core::WorkflowStatus::Paused
                ),
                "workflow must not stay Running without an active runner after resume apply; status={:?}",
                after.status
            );
        }

        // Idempotency: if a previous restart already advanced the workflow
        // (durable marker carried it forward), re-applying must NOT
        // double-advance — it should report AlreadyAdvanced and still mark
        // the session checkpoint Completed so it stops showing up.
        #[tokio::test]
        async fn auto_resume_idempotent_when_workflow_already_advanced() {
            let _lock = lock_env();
            let config_root = TempDir::new().expect("config temp dir");
            let home_root = TempDir::new().expect("home temp dir");
            let _config_guard =
                EnvVarGuard::set("ANIMUS_CONFIG_DIR", Some(config_root.path().to_string_lossy().as_ref()));
            let _home_guard = EnvVarGuard::set("HOME", Some(home_root.path().to_string_lossy().as_ref()));
            let _legacy_guard = EnvVarGuard::set("AGENT_ORCHESTRATOR_CONFIG_DIR", None);

            let project = TempDir::new().expect("project temp dir");
            let project_root = project.path().to_string_lossy().to_string();
            let _config_source_seam =
                orchestrator_config::workflow_config::config_source_client::install_yaml_config_source_base(
                    project.path(),
                );
            let scoped_root = protocol::repository_scope::scoped_state_root(project.path()).expect("scoped root");

            let (hub, workflow_id, phase_id) =
                seed_workflow_and_safe_phase_checkpoint(&project_root, &scoped_root).await;

            let checkpoint =
                read_checkpoint(&scoped_root, &workflow_id, &phase_id).expect("read").expect("checkpoint present");

            // First apply advances the workflow normally.
            let first =
                apply_resumed_outcome_in_tree(&project_root, &scoped_root, hub.clone(), &checkpoint, Some("sess-1"))
                    .await;
            assert_eq!(first, ResumedOutcomeApplyResult::Advanced);

            // Re-read the checkpoint (it's now Completed) but pretend a
            // crash left the OLD Running checkpoint for the same phase.
            // We re-seed Running to simulate a corrupted re-discover.
            update_session_running(&scoped_root, &workflow_id, &phase_id).expect("force Running again");
            let stale_checkpoint =
                read_checkpoint(&scoped_root, &workflow_id, &phase_id).expect("read").expect("present");

            let second = apply_resumed_outcome_in_tree(
                &project_root,
                &scoped_root,
                hub.clone(),
                &stale_checkpoint,
                Some("sess-2"),
            )
            .await;
            assert_eq!(
                second,
                ResumedOutcomeApplyResult::AlreadyAdvanced,
                "second apply MUST NOT double-advance the workflow"
            );

            let after = read_checkpoint(&scoped_root, &workflow_id, &phase_id).expect("read").expect("present");
            assert_eq!(
                after.status,
                SessionCheckpointStatus::Completed,
                "AlreadyAdvanced still flips the stale checkpoint Completed"
            );
        }

        // Codex round-7 follow-up P1: decision-gated phases (review, QA,
        // testing) MUST NOT have an implicit Advance synthesized — the
        // resumed agent might have produced a Rework/Fail that we never
        // captured. Apply must mark the session Blocked with a clear,
        // --force-aware reason instead of advancing the workflow.
        #[tokio::test]
        async fn auto_resume_blocks_decision_gated_phase_instead_of_implicit_advance() {
            let _lock = lock_env();
            let config_root = TempDir::new().expect("config temp dir");
            let home_root = TempDir::new().expect("home temp dir");
            let _config_guard =
                EnvVarGuard::set("ANIMUS_CONFIG_DIR", Some(config_root.path().to_string_lossy().as_ref()));
            let _home_guard = EnvVarGuard::set("HOME", Some(home_root.path().to_string_lossy().as_ref()));
            let _legacy_guard = EnvVarGuard::set("AGENT_ORCHESTRATOR_CONFIG_DIR", None);

            let project = TempDir::new().expect("project temp dir");
            let project_root = project.path().to_string_lossy().to_string();
            let _config_source_seam =
                orchestrator_config::workflow_config::config_source_client::install_yaml_config_source_base(
                    project.path(),
                );
            let scoped_root = protocol::repository_scope::scoped_state_root(project.path()).expect("scoped root");

            let (hub, workflow_id, target_phase) = seed_workflow_and_checkpoint_on_phase(
                &project_root,
                &scoped_root,
                None, // standard pipeline
                &["code-review", "testing"],
            )
            .await;

            let checkpoint =
                read_checkpoint(&scoped_root, &workflow_id, &target_phase).expect("read").expect("checkpoint present");

            let before = hub.workflows().get(&workflow_id).await.expect("workflow before");

            let result = apply_resumed_outcome_in_tree(
                &project_root,
                &scoped_root,
                hub.clone(),
                &checkpoint,
                Some("sess-rotated"),
            )
            .await;
            assert_eq!(result, ResumedOutcomeApplyResult::BlockedAwaitingDecision);

            // Workflow MUST NOT have been advanced.
            let after = hub.workflows().get(&workflow_id).await.expect("workflow after");
            assert_eq!(
                after.current_phase, before.current_phase,
                "decision-gated phase MUST NOT auto-advance on resume"
            );
            assert_eq!(after.current_phase_index, before.current_phase_index);

            // Codex round-7 round-3 P1: workflow must be Paused (not
            // Running) so the orphan sweep does not cancel it before the
            // operator runs --force.
            assert_eq!(
                after.status,
                orchestrator_core::WorkflowStatus::Paused,
                "decision-gated resume must Pause the workflow to survive the orphan sweep"
            );

            // Session checkpoint marked Blocked with an actionable reason.
            let after_ck = read_checkpoint(&scoped_root, &workflow_id, &target_phase).expect("read").expect("present");
            assert_eq!(after_ck.status, SessionCheckpointStatus::Blocked);
            let reason = after_ck.blocked_reason.unwrap_or_default();
            assert!(
                reason.contains("decision-gated phase")
                    && reason.contains(target_phase.as_str())
                    && reason.contains("--force"),
                "expected decision-gated blocked reason w/ --force hint, got: {reason}"
            );
        }

        // Codex round-7 round-3 P1: when the resumed apply advances the
        // workflow to a NEW non-terminal phase (not the final one), the
        // workflow MUST be Paused — otherwise the next orphan sweep would
        // cancel it (no runner is driving the new phase from inside the
        // startup recovery path).
        #[tokio::test]
        async fn auto_resume_pauses_workflow_when_advance_lands_on_non_terminal_phase() {
            let _lock = lock_env();
            let config_root = TempDir::new().expect("config temp dir");
            let home_root = TempDir::new().expect("home temp dir");
            let _config_guard =
                EnvVarGuard::set("ANIMUS_CONFIG_DIR", Some(config_root.path().to_string_lossy().as_ref()));
            let _home_guard = EnvVarGuard::set("HOME", Some(home_root.path().to_string_lossy().as_ref()));
            let _legacy_guard = EnvVarGuard::set("AGENT_ORCHESTRATOR_CONFIG_DIR", None);

            let project = TempDir::new().expect("project temp dir");
            let project_root = project.path().to_string_lossy().to_string();
            let scoped_root = protocol::repository_scope::scoped_state_root(project.path()).expect("scoped root");

            // Author a custom workflow YAML with two safe-to-advance
            // phases so the apply path moves to a non-terminal phase. We
            // bootstrap the standard scaffold first (FileServiceHub::new
            // ensures defaults) then add our own file.
            let _ = FileServiceHub::new(&project_root).expect("bootstrap hub");
            let workflows_dir = project.path().join(".animus").join("workflows");
            std::fs::create_dir_all(&workflows_dir).expect("create dir");
            // v0.6 kernel-purification: the kernel bakes no phases. The scaffold
            // defines `research` but not `wireframe`, so define both here.
            let custom_yaml = r#"agents:
  default:
    description: Default
    system_prompt: Default agent
phases:
  research:
    mode: agent
    agent_id: default
  wireframe:
    mode: agent
    agent_id: default
workflows:
  - id: research-then-wireframe
    name: Research Then Wireframe
    description: Two safe-to-advance phases for resume-pause testing.
    phases:
      - research
      - wireframe
"#;
            std::fs::write(workflows_dir.join("research-then-wireframe.yaml"), custom_yaml).expect("write yaml");
            // v0.6: install the config_source seam AFTER the custom workflow YAML
            // is written so the compiled base includes `research-then-wireframe`.
            let _config_source_seam =
                orchestrator_config::workflow_config::config_source_client::install_yaml_config_source_base(
                    project.path(),
                );

            let (hub, workflow_id, phase_id) = seed_workflow_and_checkpoint_on_phase(
                &project_root,
                &scoped_root,
                Some("research-then-wireframe"),
                &["research"],
            )
            .await;

            let checkpoint =
                read_checkpoint(&scoped_root, &workflow_id, &phase_id).expect("read").expect("checkpoint present");
            let result =
                apply_resumed_outcome_in_tree(&project_root, &scoped_root, hub.clone(), &checkpoint, Some("sess-x"))
                    .await;
            assert_eq!(result, ResumedOutcomeApplyResult::Advanced);

            let after = hub.workflows().get(&workflow_id).await.expect("workflow after");
            assert_eq!(
                after.status,
                orchestrator_core::WorkflowStatus::Paused,
                "non-terminal advance must Pause workflow so orphan sweep does not cancel it"
            );
            // And the workflow actually moved forward.
            assert!(
                after.current_phase_index > 0 || after.current_phase.as_deref() == Some("wireframe"),
                "workflow should have advanced past 'research'; current_phase={:?} idx={}",
                after.current_phase,
                after.current_phase_index
            );
        }

        // Codex round-7 round-4 P2: when the operator already paused the
        // workflow before the daemon restart, `WorkflowLifecycleExecutor::pause`
        // panics because the state machine has no Paused -> PauseRequested
        // transition. Apply must skip the redundant pause call and not crash.
        #[tokio::test]
        async fn auto_resume_does_not_repause_already_paused_workflow() {
            let _lock = lock_env();
            let config_root = TempDir::new().expect("config temp dir");
            let home_root = TempDir::new().expect("home temp dir");
            let _config_guard =
                EnvVarGuard::set("ANIMUS_CONFIG_DIR", Some(config_root.path().to_string_lossy().as_ref()));
            let _home_guard = EnvVarGuard::set("HOME", Some(home_root.path().to_string_lossy().as_ref()));
            let _legacy_guard = EnvVarGuard::set("AGENT_ORCHESTRATOR_CONFIG_DIR", None);

            let project = TempDir::new().expect("project temp dir");
            let project_root = project.path().to_string_lossy().to_string();
            let _config_source_seam =
                orchestrator_config::workflow_config::config_source_client::install_yaml_config_source_base(
                    project.path(),
                );
            let scoped_root = protocol::repository_scope::scoped_state_root(project.path()).expect("scoped root");

            // Seed a decision-gated checkpoint (standard pipeline's
            // code-review/testing) then pause the workflow BEFORE running
            // apply, simulating the "operator paused while session was
            // still running, then daemon restarted" scenario codex called
            // out.
            let (hub, workflow_id, target_phase) =
                seed_workflow_and_checkpoint_on_phase(&project_root, &scoped_root, None, &["code-review", "testing"])
                    .await;
            hub.workflows().pause(&workflow_id).await.expect("pause workflow");
            let before = hub.workflows().get(&workflow_id).await.expect("workflow before");
            assert_eq!(before.status, orchestrator_core::WorkflowStatus::Paused);

            let checkpoint =
                read_checkpoint(&scoped_root, &workflow_id, &target_phase).expect("read").expect("checkpoint present");

            // Apply MUST NOT panic — must short-circuit through the
            // already-paused branch and return BlockedAwaitingDecision.
            let result =
                apply_resumed_outcome_in_tree(&project_root, &scoped_root, hub.clone(), &checkpoint, Some("sess-x"))
                    .await;
            assert_eq!(result, ResumedOutcomeApplyResult::BlockedAwaitingDecision);

            // Workflow stays Paused, not duplicated/cancelled.
            let after = hub.workflows().get(&workflow_id).await.expect("workflow after");
            assert_eq!(after.status, orchestrator_core::WorkflowStatus::Paused);
        }
    }

    mod decision_gate {
        use super::super::phase_requires_explicit_decision_for_resume;
        use protocol::test_utils::EnvVarGuard;
        use std::sync::MutexGuard;
        use tempfile::TempDir;

        // Shared crate-wide env lock (see the sibling module above) so HOME /
        // ANIMUS_CONFIG_DIR mutations serialize against every other env-aware
        // test in this crate, not just within this module.
        fn lock_env() -> MutexGuard<'static, ()> {
            crate::shared::test_env_lock().lock().unwrap_or_else(|p| p.into_inner())
        }

        // Use an isolated empty project for tests that only need built-in
        // defaults. Critical: do NOT use a path that happens to have an
        // overlay config that would mutate the answer.
        fn empty_project() -> TempDir {
            TempDir::new().expect("empty project")
        }

        #[test]
        fn review_and_testing_phases_require_explicit_decision() {
            let _lock = lock_env();
            let config_root = TempDir::new().expect("config");
            let home_root = TempDir::new().expect("home");
            let _config_guard =
                EnvVarGuard::set("ANIMUS_CONFIG_DIR", Some(config_root.path().to_string_lossy().as_ref()));
            let _home_guard = EnvVarGuard::set("HOME", Some(home_root.path().to_string_lossy().as_ref()));
            let _legacy_guard = EnvVarGuard::set("AGENT_ORCHESTRATOR_CONFIG_DIR", None);
            let project = empty_project();
            let root = project.path().to_string_lossy().to_string();

            assert!(phase_requires_explicit_decision_for_resume(&root, "code-review"));
            assert!(phase_requires_explicit_decision_for_resume(&root, "review"));
            assert!(phase_requires_explicit_decision_for_resume(&root, "architecture"));
            assert!(phase_requires_explicit_decision_for_resume(&root, "testing"));
            assert!(phase_requires_explicit_decision_for_resume(&root, "test"));
            assert!(phase_requires_explicit_decision_for_resume(&root, "qa"));
            assert!(phase_requires_explicit_decision_for_resume(&root, "design-review"));
        }

        // Codex round-7 round-2 P1: implementation phases require a
        // commit message; we cannot synthesize one, so they MUST block.
        #[test]
        fn implementation_phase_blocks_because_it_requires_commit_message() {
            let _lock = lock_env();
            let config_root = TempDir::new().expect("config");
            let home_root = TempDir::new().expect("home");
            let _config_guard =
                EnvVarGuard::set("ANIMUS_CONFIG_DIR", Some(config_root.path().to_string_lossy().as_ref()));
            let _home_guard = EnvVarGuard::set("HOME", Some(home_root.path().to_string_lossy().as_ref()));
            let _legacy_guard = EnvVarGuard::set("AGENT_ORCHESTRATOR_CONFIG_DIR", None);
            let project = empty_project();
            let root = project.path().to_string_lossy().to_string();
            assert!(phase_requires_explicit_decision_for_resume(&root, "implementation"));
        }

        #[test]
        fn research_design_phases_safe_to_implicit_advance() {
            let _lock = lock_env();
            let config_root = TempDir::new().expect("config");
            let home_root = TempDir::new().expect("home");
            let _config_guard =
                EnvVarGuard::set("ANIMUS_CONFIG_DIR", Some(config_root.path().to_string_lossy().as_ref()));
            let _home_guard = EnvVarGuard::set("HOME", Some(home_root.path().to_string_lossy().as_ref()));
            let _legacy_guard = EnvVarGuard::set("AGENT_ORCHESTRATOR_CONFIG_DIR", None);
            let project = empty_project();
            let root = project.path().to_string_lossy().to_string();

            // These phases default-to-advance under the normal no-decision
            // execution path (see phase_output::persist_phase_output's
            // None branch), so the resume path mirrors that behavior.
            assert!(!phase_requires_explicit_decision_for_resume(&root, "research"));
            assert!(!phase_requires_explicit_decision_for_resume(&root, "design"));
            assert!(!phase_requires_explicit_decision_for_resume(&root, "ux-research"));
            // Unknown / custom phases with no overlay fall through to
            // default capabilities (no review/testing/commit flag) and
            // are safe to auto-advance.
            assert!(!phase_requires_explicit_decision_for_resume(&root, "custom-phase"));
        }

        // Codex round-7 round-2 P2: built-in `requirements` phase carries
        // a `decision_contract` in the bundled default config, so the
        // recovery path MUST honor it and refuse to auto-advance — even
        // though the phase's built-in capabilities don't set is_review.
        #[test]
        fn requirements_phase_blocks_via_default_decision_contract() {
            let _lock = lock_env();
            let config_root = TempDir::new().expect("config");
            let home_root = TempDir::new().expect("home");
            let _config_guard =
                EnvVarGuard::set("ANIMUS_CONFIG_DIR", Some(config_root.path().to_string_lossy().as_ref()));
            let _home_guard = EnvVarGuard::set("HOME", Some(home_root.path().to_string_lossy().as_ref()));
            let _legacy_guard = EnvVarGuard::set("AGENT_ORCHESTRATOR_CONFIG_DIR", None);
            let project = empty_project();
            let root = project.path().to_string_lossy().to_string();

            // v0.6 kernel-purification: the kernel no longer bakes a
            // decision_contract onto `requirements`. Packs / config_source
            // supply it. Seed a config that declares `requirements` WITH a
            // decision_contract, then assert the resume safety predicate honors
            // it (the merge-into-runtime-config behavior under test).
            std::fs::create_dir_all(project.path().join(".animus")).expect("mkdir .animus");
            std::fs::write(
                project.path().join(".animus").join("workflows.yaml"),
                r#"
tools_allowlist:
  - cargo
agents:
  po:
    description: Product Owner
    system_prompt: Refine requirements.
phases:
  requirements:
    mode: agent
    agent_id: po
    decision_contract:
      min_confidence: 0.6
      max_risk: medium
      allow_missing_decision: true
workflows:
  - id: standard
    name: Standard
    phases:
      - requirements
"#,
            )
            .expect("write workflows.yaml");
            let _config_source_seam =
                orchestrator_config::workflow_config::config_source_client::install_yaml_config_source_base(
                    project.path(),
                );

            assert!(
                phase_requires_explicit_decision_for_resume(&root, "requirements"),
                "a config-source-supplied decision_contract on `requirements` MUST be honored by the resume predicate"
            );
        }

        // Codex round-7 round-5 P1: workflow YAML `phases:` definitions
        // that declare `decision_contract` or `capabilities.is_review` /
        // `is_testing` / `requires_commit` MUST be honored by the resume
        // safety predicate — the YAML phase_definitions are merged into
        // the AgentRuntimeConfig.phases map by
        // `merge_workflow_runtime_overlay`, so the predicate should pick
        // them up via `phase_execution(phase_id)`.
        #[tokio::test]
        async fn workflow_yaml_phase_definition_with_decision_contract_blocks() {
            let _lock = lock_env();
            let config_root = TempDir::new().expect("config");
            let home_root = TempDir::new().expect("home");
            let _config_guard =
                EnvVarGuard::set("ANIMUS_CONFIG_DIR", Some(config_root.path().to_string_lossy().as_ref()));
            let _home_guard = EnvVarGuard::set("HOME", Some(home_root.path().to_string_lossy().as_ref()));
            let _legacy_guard = EnvVarGuard::set("AGENT_ORCHESTRATOR_CONFIG_DIR", None);
            let project = empty_project();
            let root = project.path().to_string_lossy().to_string();

            let _ = orchestrator_core::FileServiceHub::new(&root).expect("hub bootstrap");
            let workflows_dir = project.path().join(".animus").join("workflows");
            std::fs::create_dir_all(&workflows_dir).expect("mkdir");
            // YAML-only custom phase with decision_contract. No
            // companion entry in agent-runtime-config.v2.json — the
            // overlay merge is the ONLY surface that injects it.
            let yaml = r#"tools_allowlist:
  - cargo
agents:
  default:
    description: default agent
    role: software_engineer
    tool: claude
workflows:
  - id: audit-workflow
    name: Audit Workflow
    description: workflow with custom yaml-only phase
    phases:
      - security-audit
phases:
  security-audit:
    mode: agent
    agent_id: default
    directive: Audit the change for security regressions.
    decision_contract:
      required_evidence: []
      min_confidence: 0.6
      max_risk: medium
      allow_missing_decision: false
"#;
            std::fs::write(workflows_dir.join("audit-workflow.yaml"), yaml).expect("write yaml");
            let _config_source_seam =
                orchestrator_config::workflow_config::config_source_client::install_yaml_config_source_base(
                    project.path(),
                );

            assert!(
                phase_requires_explicit_decision_for_resume(&root, "security-audit"),
                "YAML phase_definitions with decision_contract MUST be honored by the resume guard"
            );
        }

        // Codex round-7 round-4 P2: workflow YAML-level on_verdict routing
        // is a decision gate even when the phase itself has no
        // decision_contract or review/testing capabilities. The
        // workflow-aware variant of the predicate must honor it.
        #[tokio::test]
        async fn workflow_on_verdict_routing_blocks_resume_advance() {
            use super::super::phase_requires_explicit_decision_for_resume_in_workflow;
            let _lock = lock_env();
            let config_root = TempDir::new().expect("config");
            let home_root = TempDir::new().expect("home");
            let _config_guard =
                EnvVarGuard::set("ANIMUS_CONFIG_DIR", Some(config_root.path().to_string_lossy().as_ref()));
            let _home_guard = EnvVarGuard::set("HOME", Some(home_root.path().to_string_lossy().as_ref()));
            let _legacy_guard = EnvVarGuard::set("AGENT_ORCHESTRATOR_CONFIG_DIR", None);
            let project = empty_project();
            let root = project.path().to_string_lossy().to_string();

            // Bootstrap default scaffolding then write a custom workflow
            // YAML where `research` has on_verdict routing (without any
            // decision_contract or review/testing capabilities on the
            // phase definition itself).
            let _ = orchestrator_core::FileServiceHub::new(&root).expect("hub");
            let workflows_dir = project.path().join(".animus").join("workflows");
            std::fs::create_dir_all(&workflows_dir).expect("mkdir");
            let yaml = r#"tools_allowlist:
  - cargo
agents:
  default:
    description: default agent
    role: software_engineer
    tool: claude
phases:
  research:
    mode: agent
    agent_id: default
    directive: Research the topic.
workflows:
  - id: gated-research
    name: Gated Research
    description: research phase with on_verdict routing
    phases:
      - research:
          on_verdict:
            rework:
              target: research
"#;
            std::fs::write(workflows_dir.join("gated-research.yaml"), yaml).expect("write yaml");
            let _config_source_seam =
                orchestrator_config::workflow_config::config_source_client::install_yaml_config_source_base(
                    project.path(),
                );

            // Without the workflow_ref, the phase appears safe to advance.
            assert!(
                !phase_requires_explicit_decision_for_resume(&root, "research"),
                "bare `research` phase has no built-in gates"
            );

            // With the workflow_ref, on_verdict gating must engage.
            assert!(
                phase_requires_explicit_decision_for_resume_in_workflow(&root, "research", Some("gated-research")),
                "on_verdict routing MUST block resume auto-advance for the configured workflow"
            );
        }
    }
}
