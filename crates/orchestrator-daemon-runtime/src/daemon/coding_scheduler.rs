//! Durable local projection of queue-owned fleet execution fences.
//!
//! The queue backend is the sole lease and generation authority. This module
//! never allocates an owner or generation and never extends an expiry. It
//! persists the exact [`ExecutionFence`] returned by `queue/v2/*`, attaches
//! daemon-local environment identities to it, and prevents a stale process from
//! preparing or releasing resources owned by a newer queue lease.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use animus_execution_protocol::{ExecutionFence, QueueLeaseFence};
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use fs2::FileExt;
use serde::{Deserialize, Serialize};

pub const CODING_SCHEDULER_CAPACITY: usize = 5;
const STATE_VERSION: u32 = 2;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TaskGeneration {
    pub task_id: String,
    pub generation: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodingRunResources {
    pub repository: String,
    pub git_ref: Option<String>,
    pub queue_item_id: String,
    pub workflow_id: String,
    pub workspace_id: Option<String>,
    pub environment_id: Option<String>,
    pub branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pull_request: Option<String>,
}

impl CodingRunResources {
    pub fn from_execution(execution: &ExecutionFence) -> Result<Self> {
        validate_fleet_execution(execution)?;
        let queue = execution.queue_lease.as_ref().context("fleet execution is missing queue lease")?;
        let (repository, git_ref, branch) = execution.repository.as_ref().map_or_else(
            || (String::new(), None, None),
            |repository| {
                let branch =
                    repository.head_ref.strip_prefix("refs/heads/").unwrap_or(&repository.head_ref).to_string();
                (repository.repository.clone(), Some(repository.head_ref.clone()), Some(branch))
            },
        );
        Ok(Self {
            repository,
            git_ref,
            queue_item_id: queue.entry_id.clone(),
            workflow_id: execution.workflow_id.clone(),
            workspace_id: None,
            environment_id: None,
            branch,
            pull_request: None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodingLease {
    /// Exact queue-owned execution authority. This is the canonical identity.
    pub execution: ExecutionFence,
    /// Display/index fields derived from `execution`; never independently minted.
    pub task: TaskGeneration,
    pub lease_generation: u64,
    pub owner: String,
    pub resources: CodingRunResources,
    pub acquired_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    #[serde(default)]
    pub recovered: bool,
    /// Live-process projection only. Queue authority remains in `execution`;
    /// startup reconciliation resets this from observed runner liveness.
    #[serde(default)]
    pub runner_active: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CollisionReason {
    Capacity { capacity: usize },
    DuplicateTaskGeneration { task: TaskGeneration },
    TaskAlreadyActive { task_id: String, generation: String },
    LeaseExpired { task: TaskGeneration, lease_generation: u64 },
    ResourceOwned { resource: String, value: String, task: TaskGeneration },
    RepositoryRef { repository: String, git_ref: String, task: TaskGeneration },
    StaleFence { task: TaskGeneration, current_generation: u64 },
    InvalidFence { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ReservationOutcome {
    Reserved { lease: Box<CodingLease> },
    Rejected { reason: CollisionReason },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodingSchedulerStatus {
    pub capacity: usize,
    pub available: usize,
    /// Stable identity of the daemon process currently renewing/recovering
    /// queue leases. This is descriptive only; the queue fence remains the
    /// authority for every mutation.
    pub owner_id: Option<String>,
    pub reservations: Vec<CodingLease>,
    #[serde(default)]
    pub recovery_needed: Vec<CodingLease>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_collision: Option<CollisionReason>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SchedulerState {
    version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    owner_id: Option<String>,
    #[serde(default)]
    leases: Vec<CodingLease>,
    #[serde(default)]
    last_collision: Option<CollisionReason>,
}

impl Default for SchedulerState {
    fn default() -> Self {
        Self { version: STATE_VERSION, owner_id: None, leases: Vec::new(), last_collision: None }
    }
}

#[derive(Debug, Clone)]
pub struct CodingScheduler {
    state_path: PathBuf,
}

pub trait CodingSchedulerService: Send + Sync {
    fn set_owner_id(&self, owner_id: &str) -> Result<()>;
    fn owns(&self, execution: &ExecutionFence) -> Result<bool>;
    fn mark_runner_active(&self, execution: &ExecutionFence, active: bool) -> Result<bool>;
    fn track(&self, execution: ExecutionFence, resources: CodingRunResources) -> Result<ReservationOutcome>;
    fn update_execution(&self, previous: &ExecutionFence, current: ExecutionFence) -> Result<ReservationOutcome>;
    fn release(&self, execution: &ExecutionFence) -> Result<bool>;
    fn bind_resources(&self, execution: &ExecutionFence, resources: CodingRunResources) -> Result<ReservationOutcome>;
    fn clear_environment(&self, execution: &ExecutionFence) -> Result<Option<CodingLease>>;
    fn status(&self) -> Result<CodingSchedulerStatus>;
}

impl CodingScheduler {
    pub fn for_project(project_root: &Path) -> Result<Self> {
        let root = protocol::scoped_state_root(project_root)
            .context("a home directory is required for the scoped daemon state")?;
        Ok(Self::with_state_path(root.join("daemon").join("coding-leases.json")))
    }

    pub fn with_state_path(state_path: PathBuf) -> Self {
        Self { state_path }
    }

    /// Record the daemon instance that owns future queue lease operations.
    /// This never grants authority; queue responses still carry and validate
    /// the exact owner/generation used by every mutation.
    pub fn set_owner_id(&self, owner_id: &str) -> Result<()> {
        let owner_id = owner_id.trim();
        anyhow::ensure!(!owner_id.is_empty(), "coding scheduler owner id must not be empty");
        self.mutate(|state| {
            state.owner_id = Some(owner_id.to_string());
        })
    }

    /// Whether the durable local projection still names this exact queue
    /// fence, including its latest backend-issued expiry. Used to keep stale
    /// runner cleanup from touching a recovered environment.
    pub fn owns(&self, execution: &ExecutionFence) -> Result<bool> {
        self.with_state(false, |state| state.leases.iter().any(|lease| lease.execution == *execution))
    }

    pub fn mark_runner_active(&self, execution: &ExecutionFence, active: bool) -> Result<bool> {
        self.mutate(|state| {
            let Some(lease) = state.leases.iter_mut().find(|lease| lease.execution == *execution) else {
                return false;
            };
            lease.runner_active = active;
            lease.updated_at = Utc::now();
            true
        })
    }

    /// Persist a fence already allocated by the queue. This method cannot mint
    /// or recover authority; it only projects an externally validated lease.
    pub fn track(&self, execution: ExecutionFence, resources: CodingRunResources) -> Result<ReservationOutcome> {
        let lease = lease_from_execution(execution, resources)?;
        self.mutate(|state| {
            if let Some(existing) =
                state.leases.iter_mut().find(|existing| existing.execution.same_execution_generation(&lease.execution))
            {
                if same_queue_authority(&existing.execution, &lease.execution) {
                    existing.execution = lease.execution.clone();
                    existing.lease_generation = lease.lease_generation;
                    existing.owner.clone_from(&lease.owner);
                    existing.expires_at = lease.expires_at;
                    existing.updated_at = Utc::now();
                    return ReservationOutcome::Reserved { lease: Box::new(existing.clone()) };
                }
                let reason = CollisionReason::StaleFence {
                    task: existing.task.clone(),
                    current_generation: existing.lease_generation,
                };
                state.last_collision = Some(reason.clone());
                return ReservationOutcome::Rejected { reason };
            }
            if let Some(reason) = collision(state, &lease) {
                state.last_collision = Some(reason.clone());
                return ReservationOutcome::Rejected { reason };
            }
            state.leases.push(lease.clone());
            state.last_collision = None;
            ReservationOutcome::Reserved { lease: Box::new(lease) }
        })
    }

    /// Replace only the queue-owned mutable lease portion after an applied
    /// renew or recover response. Workflow/subject/repository generations must
    /// remain identical and the queue ownership transition must be monotonic.
    pub fn update_execution(&self, previous: &ExecutionFence, current: ExecutionFence) -> Result<ReservationOutcome> {
        validate_fleet_execution(&current)?;
        anyhow::ensure!(
            previous.same_execution_generation(&current) && previous.repository == current.repository,
            "queue mutation changed immutable execution identity"
        );
        validate_queue_transition(previous, &current)?;
        self.mutate(|state| {
            let Some(existing) =
                state.leases.iter_mut().find(|lease| lease.execution.same_execution_generation(previous))
            else {
                let reason = invalid_fence_reason(previous, "execution is not tracked");
                state.last_collision = Some(reason.clone());
                return ReservationOutcome::Rejected { reason };
            };
            if !same_queue_authority(&existing.execution, previous) {
                let reason = CollisionReason::StaleFence {
                    task: existing.task.clone(),
                    current_generation: existing.lease_generation,
                };
                state.last_collision = Some(reason.clone());
                return ReservationOutcome::Rejected { reason };
            }
            let queue = current.queue_lease.as_ref().expect("validated fleet fence");
            let queue_generation = queue.generation;
            let queue_owner = queue.owner_id.clone();
            let queue_expires_at = queue.expires_at;
            existing.execution = current;
            existing.lease_generation = queue_generation;
            existing.owner = queue_owner;
            existing.expires_at = queue_expires_at;
            existing.recovered = existing.recovered || queue_generation > previous_queue(previous).generation;
            existing.updated_at = Utc::now();
            state.last_collision = None;
            ReservationOutcome::Reserved { lease: Box::new(existing.clone()) }
        })
    }

    pub fn release(&self, execution: &ExecutionFence) -> Result<bool> {
        self.mutate(|state| {
            let before = state.leases.len();
            state.leases.retain(|lease| {
                !(lease.execution.same_execution_generation(execution)
                    && same_queue_authority(&lease.execution, execution))
            });
            state.leases.len() != before
        })
    }

    pub fn bind_resources(
        &self,
        execution: &ExecutionFence,
        resources: CodingRunResources,
    ) -> Result<ReservationOutcome> {
        validate_projection(execution, &resources)?;
        self.mutate(|state| {
            let Some(index) =
                state.leases.iter().position(|lease| lease.execution.same_execution_generation(execution))
            else {
                let reason = invalid_fence_reason(execution, "execution is not tracked");
                state.last_collision = Some(reason.clone());
                return ReservationOutcome::Rejected { reason };
            };
            if !same_queue_authority(&state.leases[index].execution, execution) {
                let reason = CollisionReason::StaleFence {
                    task: state.leases[index].task.clone(),
                    current_generation: state.leases[index].lease_generation,
                };
                state.last_collision = Some(reason.clone());
                return ReservationOutcome::Rejected { reason };
            }
            if state.leases[index].expires_at <= Utc::now() {
                let reason = CollisionReason::LeaseExpired {
                    task: state.leases[index].task.clone(),
                    lease_generation: state.leases[index].lease_generation,
                };
                state.last_collision = Some(reason.clone());
                return ReservationOutcome::Rejected { reason };
            }
            let mut candidate = state.leases[index].clone();
            candidate.resources = resources;
            if let Some(reason) = resource_collision(state, index, &candidate) {
                state.last_collision = Some(reason.clone());
                return ReservationOutcome::Rejected { reason };
            }
            state.leases[index] = candidate.clone();
            state.leases[index].updated_at = Utc::now();
            candidate.updated_at = state.leases[index].updated_at;
            state.last_collision = None;
            ReservationOutcome::Reserved { lease: Box::new(candidate) }
        })
    }

    pub fn clear_environment(&self, execution: &ExecutionFence) -> Result<Option<CodingLease>> {
        self.mutate(|state| {
            let lease = state.leases.iter_mut().find(|lease| {
                lease.execution.same_execution_generation(execution)
                    && same_queue_authority(&lease.execution, execution)
            })?;
            lease.resources.workspace_id = None;
            lease.resources.environment_id = None;
            lease.updated_at = Utc::now();
            Some(lease.clone())
        })
    }

    pub fn status(&self) -> Result<CodingSchedulerStatus> {
        self.with_state(false, |state| {
            let now = Utc::now();
            let recovery_needed = state.leases.iter().filter(|lease| lease.expires_at <= now).cloned().collect();
            CodingSchedulerStatus {
                capacity: CODING_SCHEDULER_CAPACITY,
                // TASK-1332: only LIVE leases consume slots; expired entries are
                // recovery bookkeeping, not occupancy.
                available: CODING_SCHEDULER_CAPACITY
                    .saturating_sub(state.leases.iter().filter(|lease| lease.expires_at > now).count()),
                owner_id: state.owner_id.clone(),
                reservations: state.leases.clone(),
                recovery_needed,
                last_collision: state.last_collision.clone(),
            }
        })
    }

    fn mutate<T>(&self, f: impl FnOnce(&mut SchedulerState) -> T) -> Result<T> {
        if let Some(parent) = self.state_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let lock_path = self.state_path.with_extension("lock");
        let lock = OpenOptions::new().create(true).truncate(false).read(true).write(true).open(lock_path)?;
        lock.lock_exclusive()?;
        let mut state = load(&self.state_path)?;
        let value = f(&mut state);
        // Persist before the exclusive lock is dropped. Releasing the lock
        // between read/mutate and rename lets two daemon paths overwrite one
        // another from the same stale snapshot.
        persist(&self.state_path, &state)?;
        Ok(value)
    }

    fn with_state<T>(&self, exclusive: bool, f: impl FnOnce(&mut SchedulerState) -> T) -> Result<T> {
        if let Some(parent) = self.state_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let lock_path = self.state_path.with_extension("lock");
        let lock = OpenOptions::new().create(true).truncate(false).read(true).write(true).open(lock_path)?;
        if exclusive {
            lock.lock_exclusive()?;
        } else {
            FileExt::lock_shared(&lock)?;
        }
        let mut state = load(&self.state_path)?;
        Ok(f(&mut state))
    }
}

impl Clone for SchedulerState {
    fn clone(&self) -> Self {
        Self {
            version: self.version,
            owner_id: self.owner_id.clone(),
            leases: self.leases.clone(),
            last_collision: self.last_collision.clone(),
        }
    }
}

impl CodingSchedulerService for CodingScheduler {
    fn set_owner_id(&self, owner_id: &str) -> Result<()> {
        Self::set_owner_id(self, owner_id)
    }

    fn owns(&self, execution: &ExecutionFence) -> Result<bool> {
        Self::owns(self, execution)
    }

    fn mark_runner_active(&self, execution: &ExecutionFence, active: bool) -> Result<bool> {
        Self::mark_runner_active(self, execution, active)
    }

    fn track(&self, execution: ExecutionFence, resources: CodingRunResources) -> Result<ReservationOutcome> {
        Self::track(self, execution, resources)
    }

    fn update_execution(&self, previous: &ExecutionFence, current: ExecutionFence) -> Result<ReservationOutcome> {
        Self::update_execution(self, previous, current)
    }

    fn release(&self, execution: &ExecutionFence) -> Result<bool> {
        Self::release(self, execution)
    }

    fn bind_resources(&self, execution: &ExecutionFence, resources: CodingRunResources) -> Result<ReservationOutcome> {
        Self::bind_resources(self, execution, resources)
    }

    fn clear_environment(&self, execution: &ExecutionFence) -> Result<Option<CodingLease>> {
        Self::clear_environment(self, execution)
    }

    fn status(&self) -> Result<CodingSchedulerStatus> {
        Self::status(self)
    }
}

fn lease_from_execution(execution: ExecutionFence, resources: CodingRunResources) -> Result<CodingLease> {
    validate_projection(&execution, &resources)?;
    let subject = execution.subject.as_ref().expect("validated fleet fence");
    let queue = execution.queue_lease.as_ref().expect("validated fleet fence");
    let now = Utc::now();
    Ok(CodingLease {
        task: TaskGeneration { task_id: subject.qualified_id.clone(), generation: subject.generation.to_string() },
        lease_generation: queue.generation,
        owner: queue.owner_id.clone(),
        expires_at: queue.expires_at,
        execution,
        resources,
        acquired_at: now,
        updated_at: now,
        recovered: false,
        runner_active: false,
    })
}

fn validate_projection(execution: &ExecutionFence, resources: &CodingRunResources) -> Result<()> {
    validate_fleet_execution(execution)?;
    let queue = execution.queue_lease.as_ref().expect("validated fleet fence");
    anyhow::ensure!(resources.workflow_id == execution.workflow_id, "resource workflow id does not match fence");
    anyhow::ensure!(resources.queue_item_id == queue.entry_id, "resource queue item does not match fence");
    match execution.repository.as_ref() {
        Some(repository) => {
            anyhow::ensure!(
                normalized_repository(&resources.repository) == normalized_repository(&repository.repository),
                "resource repository does not match fence"
            );
            anyhow::ensure!(
                resources.git_ref.as_deref().and_then(normalized_git_ref) == normalized_git_ref(&repository.head_ref),
                "resource head ref does not match fence"
            );
        }
        None => {
            anyhow::ensure!(resources.repository.trim().is_empty(), "non-code fence cannot bind a repository");
            anyhow::ensure!(resources.git_ref.is_none(), "non-code fence cannot bind a git ref");
            anyhow::ensure!(resources.branch.is_none(), "non-code fence cannot bind a branch");
            anyhow::ensure!(resources.pull_request.is_none(), "non-code fence cannot bind a pull request");
        }
    }
    Ok(())
}

fn validate_fleet_execution(execution: &ExecutionFence) -> Result<()> {
    execution.validate_queue_backed().map_err(anyhow::Error::msg)?;
    let subject = execution.subject.as_ref().context("queue-backed fleet execution is missing subject generation")?;
    anyhow::ensure!(!subject.qualified_id.trim().is_empty(), "fleet subject id must not be empty");
    Ok(())
}

fn validate_queue_transition(previous: &ExecutionFence, current: &ExecutionFence) -> Result<()> {
    let old = previous_queue(previous);
    let new = previous_queue(current);
    anyhow::ensure!(old.entry_id == new.entry_id, "queue mutation changed entry id");
    if old.owner_id == new.owner_id {
        anyhow::ensure!(old.generation == new.generation, "renew changed lease generation");
        anyhow::ensure!(new.expires_at >= old.expires_at, "renew shortened lease expiry");
    } else {
        anyhow::ensure!(
            new.generation == old.generation.saturating_add(1),
            "recovery did not increment lease generation exactly once"
        );
    }
    Ok(())
}

fn previous_queue(execution: &ExecutionFence) -> &QueueLeaseFence {
    execution.queue_lease.as_ref().expect("validated queue-backed execution fence")
}

fn same_queue_authority(left: &ExecutionFence, right: &ExecutionFence) -> bool {
    match (left.queue_lease.as_ref(), right.queue_lease.as_ref()) {
        (Some(left), Some(right)) => {
            left.entry_id == right.entry_id && left.owner_id == right.owner_id && left.generation == right.generation
        }
        _ => false,
    }
}

fn collision(state: &SchedulerState, wanted: &CodingLease) -> Option<CollisionReason> {
    let now = Utc::now();
    // TASK-1332: an EXPIRED lease must never block a freshly queue-leased
    // dispatch. The queue backend is the lease authority — it issued the new
    // lease because the old one lapsed — so expired local bookkeeping only
    // wedges the fleet (the 2026-08-26 outage: a recovered lease for a
    // cancelled run bounced every later entry for its subject with
    // LeaseExpired/TaskAlreadyActive forever). Live leases keep every
    // protection below unchanged.
    let live = |lease: &&CodingLease| lease.expires_at > now;
    if let Some(existing) = state.leases.iter().filter(live).find(|lease| lease.task == wanted.task) {
        return Some(CollisionReason::DuplicateTaskGeneration { task: existing.task.clone() });
    }
    if let Some(existing) = state.leases.iter().filter(live).find(|lease| lease.task.task_id == wanted.task.task_id) {
        return Some(CollisionReason::TaskAlreadyActive {
            task_id: existing.task.task_id.clone(),
            generation: existing.task.generation.clone(),
        });
    }
    if let Some(reason) = resource_collision(state, usize::MAX, wanted) {
        return Some(reason);
    }
    (state.leases.iter().filter(|lease| lease.expires_at > now).count() >= CODING_SCHEDULER_CAPACITY)
        .then_some(CollisionReason::Capacity { capacity: CODING_SCHEDULER_CAPACITY })
}

fn resource_collision(state: &SchedulerState, skip: usize, wanted: &CodingLease) -> Option<CollisionReason> {
    let now = Utc::now();
    for (index, existing) in state.leases.iter().enumerate() {
        if index == skip {
            continue;
        }
        // TASK-1332: expired leases never collide — see collision().
        if existing.expires_at <= now {
            continue;
        }
        if let (Some(held), Some(wanted_ref)) =
            (existing.execution.repository.as_ref(), wanted.execution.repository.as_ref())
        {
            if held.collision_key() == wanted_ref.collision_key() {
                return Some(CollisionReason::RepositoryRef {
                    repository: wanted_ref.repository.clone(),
                    git_ref: wanted_ref.head_ref.clone(),
                    task: existing.task.clone(),
                });
            }
        }
        for (name, wanted_value, held) in [
            ("queue_item", &wanted.resources.queue_item_id, &existing.resources.queue_item_id),
            ("workflow", &wanted.resources.workflow_id, &existing.resources.workflow_id),
        ] {
            if !wanted_value.is_empty() && wanted_value == held {
                return Some(CollisionReason::ResourceOwned {
                    resource: name.to_string(),
                    value: wanted_value.clone(),
                    task: existing.task.clone(),
                });
            }
        }
        for (name, wanted_value, held) in [
            ("workspace", &wanted.resources.workspace_id, &existing.resources.workspace_id),
            ("environment", &wanted.resources.environment_id, &existing.resources.environment_id),
        ] {
            if let (Some(wanted_value), Some(held)) = (wanted_value, held) {
                if wanted_value == held {
                    return Some(CollisionReason::ResourceOwned {
                        resource: name.to_string(),
                        value: wanted_value.clone(),
                        task: existing.task.clone(),
                    });
                }
            }
        }
        if !wanted.resources.repository.trim().is_empty()
            && same_repository(&existing.resources.repository, &wanted.resources.repository)
            && wanted.resources.branch.is_some()
            && existing.resources.branch == wanted.resources.branch
        {
            return Some(CollisionReason::ResourceOwned {
                resource: "branch".to_string(),
                value: wanted.resources.branch.clone().unwrap_or_default(),
                task: existing.task.clone(),
            });
        }
        if !wanted.resources.repository.trim().is_empty()
            && same_repository(&existing.resources.repository, &wanted.resources.repository)
        {
            if let (Some(wanted_pr), Some(held)) = (&wanted.resources.pull_request, &existing.resources.pull_request) {
                if wanted_pr == held {
                    return Some(CollisionReason::ResourceOwned {
                        resource: "pull_request".to_string(),
                        value: wanted_pr.clone(),
                        task: existing.task.clone(),
                    });
                }
            }
        }
    }
    None
}

fn invalid_fence_reason(execution: &ExecutionFence, reason: &str) -> CollisionReason {
    let task = execution
        .subject
        .as_ref()
        .map_or(TaskGeneration { task_id: "unknown".to_string(), generation: "0".to_string() }, |subject| {
            TaskGeneration { task_id: subject.qualified_id.clone(), generation: subject.generation.to_string() }
        });
    CollisionReason::InvalidFence { reason: format!("{}: {reason}", task.task_id) }
}

fn same_repository(left: &str, right: &str) -> bool {
    normalized_repository(left) == normalized_repository(right)
}

fn normalized_repository(value: &str) -> String {
    let value = value.trim();
    if let Some((_, remainder)) = value.split_once("://") {
        let (authority, path) = remainder.split_once('/').unwrap_or((remainder, ""));
        let host = authority.rsplit_once('@').map_or(authority, |(_, host)| host);
        return repository_host_path(host, path);
    }
    let first_slash = value.find('/');
    if let Some(colon) = value.find(':') {
        if first_slash.is_some_and(|slash| colon < slash) {
            let host = value[..colon].rsplit_once('@').map_or(&value[..colon], |(_, host)| host);
            return repository_host_path(host, &value[colon + 1..]);
        }
    }
    trim_repository_path(value).to_ascii_lowercase()
}

fn repository_host_path(host: &str, path: &str) -> String {
    let host = host.trim().trim_end_matches('/').to_ascii_lowercase();
    let path = trim_repository_path(path).to_ascii_lowercase();
    if path.is_empty() {
        host
    } else {
        format!("{host}/{path}")
    }
}

fn trim_repository_path(value: &str) -> &str {
    value.trim().trim_matches('/').trim_end_matches(".git").trim_end_matches('/')
}

fn normalized_git_ref(value: &str) -> Option<&str> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    Some(
        value
            .strip_prefix("refs/heads/")
            .or_else(|| value.strip_prefix("refs/remotes/origin/"))
            .or_else(|| value.strip_prefix("origin/"))
            .unwrap_or(value),
    )
}

fn load(path: &Path) -> Result<SchedulerState> {
    match fs::read(path) {
        Ok(bytes) => {
            let state: SchedulerState = serde_json::from_slice(&bytes).context("parse durable coding lease state")?;
            anyhow::ensure!(state.version == STATE_VERSION, "unsupported coding lease state version {}", state.version);
            Ok(state)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(SchedulerState::default()),
        Err(error) => Err(error.into()),
    }
}

fn persist(path: &Path, state: &SchedulerState) -> Result<()> {
    let tmp = path.with_extension(format!("json.{}.{}.tmp", std::process::id(), uuid::Uuid::new_v4().simple()));
    let bytes = serde_json::to_vec_pretty(state)?;
    let mut file = File::create(&tmp)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    fs::rename(&tmp, path)?;
    if let Some(parent) = path.parent() {
        if let Ok(dir) = File::open(parent) {
            let _ = dir.sync_all();
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use animus_execution_protocol::{
        QueueLeaseFence, RepositoryReservation, SubjectGeneration, EXECUTION_FENCE_SCHEMA_ID, EXECUTION_FENCE_VERSION,
    };
    use chrono::Duration;

    fn execution(n: usize) -> ExecutionFence {
        ExecutionFence {
            schema: EXECUTION_FENCE_SCHEMA_ID.to_string(),
            version: EXECUTION_FENCE_VERSION,
            workflow_id: format!("workflow-{n}"),
            workflow_generation: 1,
            subject: Some(SubjectGeneration { qualified_id: format!("task:TASK-{n}"), generation: n as u64 + 1 }),
            queue_lease: Some(QueueLeaseFence {
                entry_id: format!("queue-{n}"),
                owner_id: "daemon-a".to_string(),
                generation: 1,
                expires_at: Utc::now() + Duration::minutes(5),
            }),
            repository: Some(RepositoryReservation {
                repository: "https://github.com/launchapp/animus.git".to_string(),
                base_ref: "refs/heads/main".to_string(),
                head_ref: format!("refs/heads/animus/TASK-{n}"),
            }),
        }
    }

    fn resources(n: usize) -> CodingRunResources {
        CodingRunResources::from_execution(&execution(n)).unwrap()
    }

    fn expired_execution(n: usize) -> ExecutionFence {
        let mut execution = execution(n);
        execution.queue_lease.as_mut().unwrap().expires_at = Utc::now() - Duration::minutes(1);
        execution
    }

    fn non_code_execution(n: usize) -> ExecutionFence {
        let mut execution = execution(n);
        execution.repository = None;
        execution
    }

    #[test]
    fn admits_five_independent_queue_fences_without_minting_authority() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("leases.json");
        let scheduler = CodingScheduler::with_state_path(path.clone());
        let mut expected = Vec::new();
        for n in 0..CODING_SCHEDULER_CAPACITY {
            let execution = execution(n);
            let resources = CodingRunResources::from_execution(&execution).unwrap();
            assert!(matches!(
                scheduler.track(execution.clone(), resources).unwrap(),
                ReservationOutcome::Reserved { .. }
            ));
            expected.push(execution);
        }
        let status = CodingScheduler::with_state_path(path).status().unwrap();
        assert_eq!(status.available, 0);
        for (n, lease) in status.reservations.iter().enumerate() {
            assert_eq!(lease.lease_generation, 1);
            assert_eq!(lease.owner, "daemon-a");
            assert_eq!(lease.execution, expected[n]);
        }
        assert!(matches!(
            scheduler.track(execution(9), resources(9)).unwrap(),
            ReservationOutcome::Rejected { reason: CollisionReason::Capacity { capacity: 5 } }
        ));
    }

    #[test]
    fn expired_leases_never_block_a_fresh_queue_lease_for_the_same_task() {
        // TASK-1332 regression: a recovered, expired lease for a cancelled run
        // used to bounce every later queue entry for the same subject with
        // TaskAlreadyActive forever (production wedge, 2026-08-26).
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("leases.json");
        let scheduler = CodingScheduler::with_state_path(path.clone());

        let stale = expired_execution(1);
        assert!(matches!(
            scheduler.track(stale.clone(), CodingRunResources::from_execution(&stale).unwrap()).unwrap(),
            ReservationOutcome::Reserved { .. }
        ));

        // A different execution (new workflow + new queue entry) for the SAME
        // task must reserve cleanly even though the stale lease is still on disk.
        let mut fresh = execution(2);
        fresh.subject = stale.subject.clone();
        assert!(matches!(
            scheduler.track(fresh.clone(), CodingRunResources::from_execution(&fresh).unwrap()).unwrap(),
            ReservationOutcome::Reserved { .. }
        ));

        // Only live leases consume capacity: 1 live of 5, not 2 of 5.
        let status = CodingScheduler::with_state_path(path).status().unwrap();
        assert_eq!(status.available, CODING_SCHEDULER_CAPACITY - 1);
        assert_eq!(status.recovery_needed.len(), 1, "the expired lease remains listed for queue recovery");
    }

    #[test]
    fn tracks_non_code_queue_execution_without_inventing_repository_ownership() {
        let temp = tempfile::tempdir().unwrap();
        let scheduler = CodingScheduler::with_state_path(temp.path().join("leases.json"));
        let execution = non_code_execution(1);
        let resources = CodingRunResources::from_execution(&execution).unwrap();
        assert!(resources.repository.is_empty());
        assert!(resources.git_ref.is_none());
        assert!(resources.branch.is_none());
        assert!(matches!(scheduler.track(execution.clone(), resources).unwrap(), ReservationOutcome::Reserved { .. }));

        let mut invalid = scheduler.status().unwrap().reservations[0].resources.clone();
        invalid.repository = "https://github.com/launchapp-dev/animus-cli.git".to_string();
        assert!(scheduler.bind_resources(&execution, invalid).is_err());
    }

    #[test]
    fn rejects_duplicate_generation_and_repository_head_collision() {
        let temp = tempfile::tempdir().unwrap();
        let scheduler = CodingScheduler::with_state_path(temp.path().join("leases.json"));
        scheduler.track(execution(1), resources(1)).unwrap();
        assert!(matches!(scheduler.track(execution(1), resources(1)).unwrap(), ReservationOutcome::Reserved { .. }));

        let mut colliding = execution(2);
        colliding.repository.as_mut().unwrap().head_ref = "refs/heads/animus/TASK-1".to_string();
        let colliding_resources = CodingRunResources::from_execution(&colliding).unwrap();
        assert!(matches!(
            scheduler.track(colliding, colliding_resources).unwrap(),
            ReservationOutcome::Rejected { reason: CollisionReason::RepositoryRef { .. } }
        ));
    }

    #[test]
    fn renew_and_recovery_only_replace_queue_authority_from_backend_response() {
        let temp = tempfile::tempdir().unwrap();
        let scheduler = CodingScheduler::with_state_path(temp.path().join("leases.json"));
        let first = execution(1);
        scheduler.track(first.clone(), resources(1)).unwrap();

        let mut renewed = first.clone();
        renewed.queue_lease.as_mut().unwrap().expires_at += Duration::minutes(5);
        scheduler.update_execution(&first, renewed.clone()).unwrap();

        let mut recovered = renewed.clone();
        let queue = recovered.queue_lease.as_mut().unwrap();
        queue.owner_id = "daemon-b".to_string();
        queue.generation += 1;
        queue.expires_at += Duration::minutes(5);
        scheduler.update_execution(&renewed, recovered.clone()).unwrap();

        let lease = scheduler.status().unwrap().reservations.pop().unwrap();
        assert_eq!(lease.execution, recovered);
        assert_eq!(lease.owner, "daemon-b");
        assert_eq!(lease.lease_generation, 2);
        assert!(lease.recovered);
        assert!(!scheduler.release(&first).unwrap(), "stale owner must not release recovered lease");
        assert!(scheduler.release(&lease.execution).unwrap());
    }

    #[test]
    fn expired_fence_cannot_bind_environment_before_queue_recovery() {
        let temp = tempfile::tempdir().unwrap();
        let scheduler = CodingScheduler::with_state_path(temp.path().join("leases.json"));
        let mut expired = execution(1);
        expired.queue_lease.as_mut().unwrap().expires_at = Utc::now() - Duration::seconds(1);
        let resources = CodingRunResources::from_execution(&expired).unwrap();
        scheduler.track(expired.clone(), resources.clone()).unwrap();
        let mut allocated = resources;
        allocated.environment_id = Some("railway:node-1".to_string());
        allocated.workspace_id = Some("railway:node-1:/workspace".to_string());
        assert!(matches!(
            scheduler.bind_resources(&expired, allocated).unwrap(),
            ReservationOutcome::Rejected { reason: CollisionReason::LeaseExpired { .. } }
        ));
        assert_eq!(scheduler.status().unwrap().recovery_needed.len(), 1);
    }

    #[test]
    fn dead_node_cleanup_preserves_execution_and_branch_identity() {
        let temp = tempfile::tempdir().unwrap();
        let scheduler = CodingScheduler::with_state_path(temp.path().join("leases.json"));
        let execution = execution(1);
        let mut resources = resources(1);
        resources.environment_id = Some("railway:node-1".to_string());
        resources.workspace_id = Some("railway:node-1:/workspace".to_string());
        resources.pull_request = Some("https://github.com/launchapp/animus/pull/1".to_string());
        scheduler.track(execution.clone(), resources).unwrap();
        let cleared = scheduler.clear_environment(&execution).unwrap().unwrap();
        assert!(cleared.resources.environment_id.is_none());
        assert!(cleared.resources.workspace_id.is_none());
        assert_eq!(cleared.resources.branch.as_deref(), Some("animus/TASK-1"));
        assert!(cleared.resources.pull_request.is_some());
    }
}
