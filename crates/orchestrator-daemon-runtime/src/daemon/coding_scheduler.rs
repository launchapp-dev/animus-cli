//! Durable admission control for daemon-owned coding runs.
//!
//! This is deliberately only the ownership authority.  Runner reattachment and
//! retained-environment probing remain in `agent_record` and the CLI daemon
//! reconciler (TASK-793/TASK-933); callers feed their result to
//! [`CodingScheduler::reconcile`].

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Duration, Utc};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const CODING_SCHEDULER_CAPACITY: usize = 5;
const STATE_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TaskGeneration {
    pub task_id: String,
    /// Stable producer-owned generation identity. Queue-backed work uses the
    /// durable queue entry id; this must never be synthesized by the daemon.
    pub generation: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodingLease {
    pub task: TaskGeneration,
    /// Monotonically increasing fencing token.  Every mutation must present it.
    pub lease_generation: u64,
    /// Unforgeable ownership token, persisted so a restarted daemon can adopt
    /// the exact lease rather than preparing another node.
    pub owner: String,
    pub resources: CodingRunResources,
    pub acquired_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    #[serde(default)]
    pub recovered: bool,
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
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ReservationOutcome {
    Reserved { lease: CodingLease },
    Rejected { reason: CollisionReason },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodingSchedulerStatus {
    pub capacity: usize,
    pub available: usize,
    pub reservations: Vec<CodingLease>,
    /// Leases which may no longer be renewed and require liveness
    /// reconciliation before their resources can be admitted again.
    #[serde(default)]
    pub recovery_needed: Vec<CodingLease>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_collision: Option<CollisionReason>,
}

/// The liveness classification is supplied by the existing runner/node restart
/// reconciliation.  In particular, `RetainedLiveNode` preserves the lease and
/// prevents preparation of a duplicate environment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryObservation {
    LiveRunner,
    RetainedLiveNode,
    DeadNode,
    Terminal,
    /// State could not be proven live or dead. Preserve the fence without
    /// extending its expiry so typed status reports recovery is required.
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum RecoveryOutcome {
    Preserved { lease: CodingLease },
    Recovered { lease: CodingLease },
    Released { task: TaskGeneration, lease_generation: u64 },
    Missing,
    Fenced { current_generation: u64 },
}

#[derive(Debug, Serialize, Deserialize)]
struct SchedulerState {
    version: u32,
    next_lease_generation: u64,
    #[serde(default)]
    leases: Vec<CodingLease>,
    #[serde(default)]
    last_collision: Option<CollisionReason>,
}

impl Default for SchedulerState {
    fn default() -> Self {
        Self { version: STATE_VERSION, next_lease_generation: 1, leases: Vec::new(), last_collision: None }
    }
}

/// File-backed, process-safe coding scheduler.
#[derive(Debug, Clone)]
pub struct CodingScheduler {
    state_path: PathBuf,
    lease_ttl: Duration,
}

/// Typed service surface used by daemon admission, health and recovery.
/// Keeping callers on this contract prevents them from interpreting the
/// durable JSON file or bypassing generation fencing.
pub trait CodingSchedulerService: Send + Sync {
    fn reserve(&self, task: TaskGeneration, resources: CodingRunResources) -> Result<ReservationOutcome>;
    fn renew(&self, task: &TaskGeneration, lease_generation: u64, owner: &str) -> Result<Option<CodingLease>>;
    fn release(&self, task: &TaskGeneration, lease_generation: u64, owner: &str) -> Result<bool>;
    fn bind_resources(
        &self,
        task: &TaskGeneration,
        lease_generation: u64,
        owner: &str,
        resources: CodingRunResources,
    ) -> Result<ReservationOutcome>;
    fn reconcile(
        &self,
        task: &TaskGeneration,
        lease_generation: u64,
        observation: RecoveryObservation,
    ) -> Result<RecoveryOutcome>;
    fn status(&self) -> Result<CodingSchedulerStatus>;
}

impl CodingScheduler {
    pub fn for_project(project_root: &Path) -> Result<Self> {
        let root = protocol::scoped_state_root(project_root)
            .context("a home directory is required for the scoped daemon state")?;
        Ok(Self::with_state_path(root.join("daemon").join("coding-leases.json")))
    }

    pub fn with_state_path(state_path: PathBuf) -> Self {
        Self { state_path, lease_ttl: Duration::minutes(5) }
    }

    pub fn with_lease_ttl(mut self, lease_ttl: Duration) -> Self {
        self.lease_ttl = lease_ttl;
        self
    }

    pub fn reserve(&self, task: TaskGeneration, resources: CodingRunResources) -> Result<ReservationOutcome> {
        validate(&task, &resources)?;
        self.mutate(|state| {
            let collision = collision(state, &task, &resources);
            if let Some(reason) = collision {
                state.last_collision = Some(reason.clone());
                return ReservationOutcome::Rejected { reason };
            }
            let lease_generation = state.next_lease_generation;
            state.next_lease_generation = state.next_lease_generation.saturating_add(1);
            let now = Utc::now();
            let lease = CodingLease {
                task,
                lease_generation,
                owner: Uuid::new_v4().to_string(),
                resources,
                acquired_at: now,
                expires_at: now + self.lease_ttl,
                recovered: false,
            };
            state.leases.push(lease.clone());
            state.last_collision = None;
            ReservationOutcome::Reserved { lease }
        })
    }

    /// Renew only if both fencing credentials still identify the current owner.
    pub fn renew(&self, task: &TaskGeneration, lease_generation: u64, owner: &str) -> Result<Option<CodingLease>> {
        self.mutate(|state| {
            let now = Utc::now();
            let Some(lease) = state.leases.iter_mut().find(|lease| &lease.task == task) else {
                return None;
            };
            if lease.lease_generation != lease_generation || lease.owner != owner {
                return None;
            }
            // Expiry is a fencing boundary.  Only reconcile(), after consulting
            // the existing runner/node liveness machinery, may recover it.
            if lease.expires_at <= now {
                return None;
            }
            lease.expires_at = now + self.lease_ttl;
            Some(lease.clone())
        })
    }

    pub fn release(&self, task: &TaskGeneration, lease_generation: u64, owner: &str) -> Result<bool> {
        self.mutate(|state| {
            let before = state.leases.len();
            state.leases.retain(|lease| {
                !(&lease.task == task && lease.lease_generation == lease_generation && lease.owner == owner)
            });
            state.leases.len() != before
        })
    }

    /// Attach identities allocated after admission (workspace, environment,
    /// branch or PR) to the exact lease. This is fenced and collision checked,
    /// so late phase preparation cannot steal another run's resource.
    pub fn bind_resources(
        &self,
        task: &TaskGeneration,
        lease_generation: u64,
        owner: &str,
        resources: CodingRunResources,
    ) -> Result<ReservationOutcome> {
        validate(task, &resources)?;
        self.mutate(|state| {
            let Some(index) = state.leases.iter().position(|lease| &lease.task == task) else {
                return ReservationOutcome::Rejected {
                    reason: CollisionReason::DuplicateTaskGeneration { task: task.clone() },
                };
            };
            if state.leases[index].lease_generation != lease_generation || state.leases[index].owner != owner {
                return ReservationOutcome::Rejected {
                    reason: CollisionReason::TaskAlreadyActive {
                        task_id: task.task_id.clone(),
                        generation: state.leases[index].task.generation.clone(),
                    },
                };
            }
            // Expiry fences every allocation mutation, not only heartbeats.
            // Otherwise a daemon holding stale persisted credentials could
            // prepare and bind a second node while restart reconciliation is
            // still deciding whether the original runner/node survived.
            if state.leases[index].expires_at <= Utc::now() {
                return ReservationOutcome::Rejected {
                    reason: CollisionReason::LeaseExpired {
                        task: task.clone(),
                        lease_generation,
                    },
                };
            }
            let lease = state.leases.remove(index);
            if let Some(reason) = collision(state, task, &resources) {
                state.leases.insert(index, lease);
                state.last_collision = Some(reason.clone());
                return ReservationOutcome::Rejected { reason };
            }
            let mut lease = lease;
            lease.resources = resources;
            state.leases.insert(index, lease.clone());
            state.last_collision = None;
            ReservationOutcome::Reserved { lease }
        })
    }

    /// Apply the TASK-793/TASK-933 liveness result to an exact fenced lease.
    pub fn reconcile(
        &self,
        task: &TaskGeneration,
        lease_generation: u64,
        observation: RecoveryObservation,
    ) -> Result<RecoveryOutcome> {
        self.mutate(|state| {
            let Some(index) = state.leases.iter().position(|lease| &lease.task == task) else {
                return RecoveryOutcome::Missing;
            };
            if state.leases[index].lease_generation != lease_generation {
                return RecoveryOutcome::Fenced { current_generation: state.leases[index].lease_generation };
            }
            match observation {
                RecoveryObservation::LiveRunner => {
                    state.leases[index].expires_at = Utc::now() + self.lease_ttl;
                    RecoveryOutcome::Preserved { lease: state.leases[index].clone() }
                }
                RecoveryObservation::RetainedLiveNode => {
                    state.leases[index].recovered = true;
                    state.leases[index].expires_at = Utc::now() + self.lease_ttl;
                    RecoveryOutcome::Recovered { lease: state.leases[index].clone() }
                }
                RecoveryObservation::DeadNode => {
                    // The journal/workflow still owns this generation and will
                    // redispatch it. Drop only identities belonging to the dead
                    // allocation so the replacement can be bound to the same
                    // fence; retaining the lease prevents queue admission from
                    // preparing a second replacement concurrently.
                    state.leases[index].resources.workspace_id = None;
                    state.leases[index].resources.environment_id = None;
                    state.leases[index].recovered = true;
                    state.leases[index].expires_at = Utc::now() + self.lease_ttl;
                    RecoveryOutcome::Recovered { lease: state.leases[index].clone() }
                }
                RecoveryObservation::Terminal => {
                    let lease = state.leases.remove(index);
                    RecoveryOutcome::Released {
                        task: lease.task,
                        lease_generation: lease.lease_generation,
                    }
                }
                RecoveryObservation::Unknown => RecoveryOutcome::Preserved { lease: state.leases[index].clone() },
            }
        })
    }

    pub fn status(&self) -> Result<CodingSchedulerStatus> {
        self.read(|state| {
            let now = Utc::now();
            CodingSchedulerStatus {
                capacity: CODING_SCHEDULER_CAPACITY,
                available: CODING_SCHEDULER_CAPACITY.saturating_sub(state.leases.len()),
                reservations: state.leases.clone(),
                recovery_needed: state.leases.iter().filter(|lease| lease.expires_at <= now).cloned().collect(),
                last_collision: state.last_collision.clone(),
            }
        })
    }

    fn read<T>(&self, f: impl FnOnce(&SchedulerState) -> T) -> Result<T> {
        self.with_lock(false, |state| Ok(f(state)))
    }

    fn mutate<T>(&self, f: impl FnOnce(&mut SchedulerState) -> T) -> Result<T> {
        self.with_lock(true, |state| {
            let value = f(state);
            persist(&self.state_path, state)?;
            Ok(value)
        })
    }

    fn with_lock<T>(&self, exclusive: bool, f: impl FnOnce(&mut SchedulerState) -> Result<T>) -> Result<T> {
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
        f(&mut state)
    }
}

impl CodingSchedulerService for CodingScheduler {
    fn reserve(&self, task: TaskGeneration, resources: CodingRunResources) -> Result<ReservationOutcome> {
        CodingScheduler::reserve(self, task, resources)
    }

    fn renew(&self, task: &TaskGeneration, lease_generation: u64, owner: &str) -> Result<Option<CodingLease>> {
        CodingScheduler::renew(self, task, lease_generation, owner)
    }

    fn release(&self, task: &TaskGeneration, lease_generation: u64, owner: &str) -> Result<bool> {
        CodingScheduler::release(self, task, lease_generation, owner)
    }

    fn bind_resources(
        &self,
        task: &TaskGeneration,
        lease_generation: u64,
        owner: &str,
        resources: CodingRunResources,
    ) -> Result<ReservationOutcome> {
        CodingScheduler::bind_resources(self, task, lease_generation, owner, resources)
    }

    fn reconcile(
        &self,
        task: &TaskGeneration,
        lease_generation: u64,
        observation: RecoveryObservation,
    ) -> Result<RecoveryOutcome> {
        CodingScheduler::reconcile(self, task, lease_generation, observation)
    }

    fn status(&self) -> Result<CodingSchedulerStatus> {
        CodingScheduler::status(self)
    }
}

fn collision(
    state: &SchedulerState,
    task: &TaskGeneration,
    resources: &CodingRunResources,
) -> Option<CollisionReason> {
    if let Some(existing) = state.leases.iter().find(|lease| lease.task == *task) {
        return Some(CollisionReason::DuplicateTaskGeneration { task: existing.task.clone() });
    }
    if let Some(existing) = state.leases.iter().find(|lease| lease.task.task_id == task.task_id) {
        return Some(CollisionReason::TaskAlreadyActive {
            task_id: existing.task.task_id.clone(),
            generation: existing.task.generation.clone(),
        });
    }
    for existing in &state.leases {
        if same_repository(&existing.resources.repository, &resources.repository) {
            if let (Some(held), Some(wanted)) = (
                existing.resources.git_ref.as_deref().and_then(normalized_git_ref),
                resources.git_ref.as_deref().and_then(normalized_git_ref),
            ) {
                if held == wanted {
                    return Some(CollisionReason::RepositoryRef {
                        repository: resources.repository.clone(),
                        git_ref: resources.git_ref.clone().unwrap_or_default(),
                        task: existing.task.clone(),
                    });
                }
            }
        }
        for (name, wanted, held) in [
            ("queue_item", &resources.queue_item_id, &existing.resources.queue_item_id),
            ("workflow", &resources.workflow_id, &existing.resources.workflow_id),
        ] {
            if !wanted.is_empty() && wanted == held {
                return Some(CollisionReason::ResourceOwned {
                    resource: name.to_string(),
                    value: wanted.clone(),
                    task: existing.task.clone(),
                });
            }
        }
        for (name, wanted, held) in [
            ("workspace", &resources.workspace_id, &existing.resources.workspace_id),
            ("environment", &resources.environment_id, &existing.resources.environment_id),
        ] {
            if let (Some(wanted), Some(held)) = (wanted, held) {
                if wanted == held {
                    return Some(CollisionReason::ResourceOwned {
                        resource: name.to_string(),
                        value: wanted.clone(),
                        task: existing.task.clone(),
                    });
                }
            }
        }
        if same_repository(&existing.resources.repository, &resources.repository)
            && resources.branch.is_some()
            && existing.resources.branch == resources.branch
        {
            return Some(CollisionReason::ResourceOwned {
                resource: "branch".to_string(),
                value: resources.branch.clone().unwrap_or_default(),
                task: existing.task.clone(),
            });
        }
        if same_repository(&existing.resources.repository, &resources.repository) {
            if let (Some(wanted), Some(held)) = (&resources.pull_request, &existing.resources.pull_request) {
                if wanted == held {
                    return Some(CollisionReason::ResourceOwned {
                        resource: "pull_request".to_string(),
                        value: wanted.clone(),
                        task: existing.task.clone(),
                    });
                }
            }
        }
    }
    (state.leases.len() >= CODING_SCHEDULER_CAPACITY)
        .then_some(CollisionReason::Capacity { capacity: CODING_SCHEDULER_CAPACITY })
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

    // Git's SCP-like syntax has no scheme: `git@host:owner/repository.git`.
    // Require a slash after the colon so shorthand names and Windows paths do
    // not accidentally become remote repository URLs.
    let first_slash = value.find('/');
    if let Some(colon) = value.find(':') {
        if first_slash.is_some_and(|slash| colon < slash) {
            let host = value[..colon].rsplit_once('@').map_or(&value[..colon], |(_, host)| host);
            return repository_host_path(host, &value[colon + 1..]);
        }
    }

    trim_repository_path(value).to_string()
}

fn repository_host_path(host: &str, path: &str) -> String {
    let host = host.trim().trim_end_matches('/').to_ascii_lowercase();
    let path = trim_repository_path(path);
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

fn validate(task: &TaskGeneration, resources: &CodingRunResources) -> Result<()> {
    for (name, value) in [
        ("task id", task.task_id.as_str()),
        ("task generation", task.generation.as_str()),
        ("repository", resources.repository.as_str()),
        ("queue item id", resources.queue_item_id.as_str()),
        ("workflow id", resources.workflow_id.as_str()),
    ] {
        anyhow::ensure!(!value.trim().is_empty(), "{name} must not be empty");
    }
    Ok(())
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
    let tmp = path.with_extension(format!("json.{}.tmp", std::process::id()));
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

    fn resources(n: usize) -> CodingRunResources {
        CodingRunResources {
            repository: "launchapp/animus".into(),
            git_ref: Some(format!("refs/heads/task-{n}")),
            queue_item_id: format!("queue-{n}"),
            workflow_id: format!("workflow-{n}"),
            workspace_id: Some(format!("workspace-{n}")),
            environment_id: Some(format!("environment-{n}")),
            branch: Some(format!("task-{n}")),
            pull_request: None,
        }
    }

    fn task(n: usize) -> TaskGeneration {
        TaskGeneration { task_id: format!("TASK-{n}"), generation: format!("queue-{n}") }
    }

    #[test]
    fn admits_five_independent_runs_and_persists_them() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("leases.json");
        let scheduler = CodingScheduler::with_state_path(path.clone());
        for n in 0..CODING_SCHEDULER_CAPACITY {
            assert!(matches!(scheduler.reserve(task(n), resources(n)).unwrap(), ReservationOutcome::Reserved { .. }));
        }
        assert_eq!(CodingScheduler::with_state_path(path).status().unwrap().available, 0);
        assert!(matches!(
            scheduler.reserve(task(9), resources(9)).unwrap(),
            ReservationOutcome::Rejected { reason: CollisionReason::Capacity { capacity: 5 } }
        ));
    }

    #[test]
    fn rejects_duplicate_generation_and_repo_ref_collision() {
        let temp = tempfile::tempdir().unwrap();
        let scheduler = CodingScheduler::with_state_path(temp.path().join("leases.json"));
        scheduler.reserve(task(1), resources(1)).unwrap();
        assert!(matches!(
            scheduler.reserve(task(1), resources(2)).unwrap(),
            ReservationOutcome::Rejected { reason: CollisionReason::DuplicateTaskGeneration { .. } }
        ));
        let mut colliding = resources(3);
        colliding.git_ref = resources(1).git_ref;
        assert!(matches!(
            scheduler.reserve(task(3), colliding).unwrap(),
            ReservationOutcome::Rejected { reason: CollisionReason::RepositoryRef { .. } }
        ));
    }

    #[test]
    fn rejects_equivalent_repository_and_ref_spellings() {
        let temp = tempfile::tempdir().unwrap();
        let scheduler = CodingScheduler::with_state_path(temp.path().join("leases.json"));
        let mut held = resources(1);
        held.repository = "https://GitHub.COM/launchapp/animus.git".into();
        held.git_ref = Some("refs/heads/main".into());
        held.branch = None;
        scheduler.reserve(task(1), held).unwrap();

        for (n, repository) in [
            (2, "https://github.com/launchapp/animus/"),
            (3, "ssh://git@github.com/launchapp/animus.git"),
            (4, "git@github.com:launchapp/animus.git"),
        ] {
            let mut wanted = resources(n);
            wanted.repository = repository.into();
            wanted.git_ref = Some("origin/main".into());
            wanted.branch = None;
            assert!(matches!(
                scheduler.reserve(task(n), wanted).unwrap(),
                ReservationOutcome::Rejected { reason: CollisionReason::RepositoryRef { .. } }
            ));
        }
    }

    #[test]
    fn pull_request_identity_is_scoped_to_its_repository() {
        let temp = tempfile::tempdir().unwrap();
        let scheduler = CodingScheduler::with_state_path(temp.path().join("leases.json"));
        let mut held = resources(1);
        held.pull_request = Some("42".into());
        scheduler.reserve(task(1), held).unwrap();

        let mut independent = resources(2);
        independent.repository = "launchapp/another-repository".into();
        independent.pull_request = Some("42".into());
        assert!(matches!(
            scheduler.reserve(task(2), independent).unwrap(),
            ReservationOutcome::Reserved { .. }
        ));

        let mut colliding = resources(3);
        colliding.pull_request = Some("42".into());
        assert!(matches!(
            scheduler.reserve(task(3), colliding).unwrap(),
            ReservationOutcome::Rejected {
                reason: CollisionReason::ResourceOwned { resource, value, .. }
            } if resource == "pull_request" && value == "42"
        ));
    }

    #[test]
    fn retained_node_is_adopted_and_stale_owner_is_fenced() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("leases.json");
        let scheduler = CodingScheduler::with_state_path(path.clone());
        let ReservationOutcome::Reserved { lease } = scheduler.reserve(task(1), resources(1)).unwrap() else {
            panic!("reservation rejected");
        };
        let restarted = CodingScheduler::with_state_path(path);
        let recovered = restarted
            .reconcile(&lease.task, lease.lease_generation, RecoveryObservation::RetainedLiveNode)
            .unwrap();
        assert!(matches!(recovered, RecoveryOutcome::Recovered { lease: CodingLease { recovered: true, .. } }));
        assert!(!restarted.release(&lease.task, lease.lease_generation + 1, &lease.owner).unwrap());
    }

    #[test]
    fn late_resource_binding_is_generation_fenced_and_persisted() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("leases.json");
        let scheduler = CodingScheduler::with_state_path(path.clone());
        let ReservationOutcome::Reserved { lease } = scheduler.reserve(task(1), resources(1)).unwrap() else {
            panic!("reservation rejected");
        };
        let mut bound = lease.resources.clone();
        bound.workspace_id = Some("/workspace/allocated".into());
        bound.environment_id = Some("railway:node-123".into());
        assert!(matches!(
            scheduler
                .bind_resources(&lease.task, lease.lease_generation, &lease.owner, bound.clone())
                .unwrap(),
            ReservationOutcome::Reserved { .. }
        ));
        assert!(matches!(
            scheduler
                .bind_resources(&lease.task, lease.lease_generation + 1, &lease.owner, bound.clone())
                .unwrap(),
            ReservationOutcome::Rejected { .. }
        ));
        assert_eq!(CodingScheduler::with_state_path(path).status().unwrap().reservations[0].resources, bound);
    }

    #[test]
    fn dead_node_restart_preserves_fence_for_exactly_one_replacement() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("leases.json");
        let scheduler = CodingScheduler::with_state_path(path.clone());
        let ReservationOutcome::Reserved { lease } = scheduler.reserve(task(1), resources(1)).unwrap() else {
            panic!("reservation rejected");
        };

        let restarted = CodingScheduler::with_state_path(path.clone());
        let RecoveryOutcome::Recovered { lease: recovered } = restarted
            .reconcile(&lease.task, lease.lease_generation, RecoveryObservation::DeadNode)
            .unwrap()
        else {
            panic!("dead node was not recovered");
        };
        assert_eq!(recovered.lease_generation, lease.lease_generation);
        assert_eq!(recovered.owner, lease.owner);
        assert_eq!(recovered.resources.workflow_id, lease.resources.workflow_id);
        assert!(recovered.resources.workspace_id.is_none());
        assert!(recovered.resources.environment_id.is_none());
        assert!(matches!(
            restarted.reserve(lease.task.clone(), lease.resources.clone()).unwrap(),
            ReservationOutcome::Rejected { reason: CollisionReason::DuplicateTaskGeneration { .. } }
        ));

        let mut replacement = recovered.resources.clone();
        replacement.workspace_id = Some("replacement-workspace".into());
        replacement.environment_id = Some("replacement-environment".into());
        assert!(matches!(
            restarted
                .bind_resources(
                    &recovered.task,
                    recovered.lease_generation,
                    &recovered.owner,
                    replacement.clone(),
                )
                .unwrap(),
            ReservationOutcome::Reserved { .. }
        ));

        let persisted = CodingScheduler::with_state_path(path).status().unwrap();
        assert_eq!(persisted.reservations.len(), 1);
        assert_eq!(persisted.reservations[0].lease_generation, lease.lease_generation);
        assert_eq!(persisted.reservations[0].resources, replacement);
    }

    #[test]
    fn terminal_observation_releases_capacity() {
        let temp = tempfile::tempdir().unwrap();
        let scheduler = CodingScheduler::with_state_path(temp.path().join("leases.json"));
        let ReservationOutcome::Reserved { lease } = scheduler.reserve(task(1), resources(1)).unwrap() else {
            panic!("reservation rejected");
        };
        assert!(matches!(
            scheduler.reconcile(&lease.task, lease.lease_generation, RecoveryObservation::Terminal).unwrap(),
            RecoveryOutcome::Released { .. }
        ));
        assert_eq!(scheduler.status().unwrap().available, CODING_SCHEDULER_CAPACITY);
    }

    #[test]
    fn expired_lease_cannot_be_revived_by_renewal_and_requires_reconciliation() {
        let temp = tempfile::tempdir().unwrap();
        let scheduler = CodingScheduler::with_state_path(temp.path().join("leases.json"))
            .with_lease_ttl(Duration::milliseconds(-1));
        let ReservationOutcome::Reserved { lease } = scheduler.reserve(task(1), resources(1)).unwrap() else {
            panic!("reservation rejected");
        };
        assert!(scheduler.renew(&lease.task, lease.lease_generation, &lease.owner).unwrap().is_none());
        assert_eq!(scheduler.status().unwrap().recovery_needed.len(), 1);
        assert!(matches!(
            scheduler.reconcile(&lease.task, lease.lease_generation, RecoveryObservation::RetainedLiveNode).unwrap(),
            RecoveryOutcome::Recovered { .. }
        ));
        assert!(scheduler.status().unwrap().recovery_needed.is_empty());
    }

    #[test]
    fn expired_lease_cannot_bind_a_replacement_before_reconciliation() {
        let temp = tempfile::tempdir().unwrap();
        let scheduler = CodingScheduler::with_state_path(temp.path().join("leases.json"))
            .with_lease_ttl(Duration::milliseconds(-1));
        let ReservationOutcome::Reserved { lease } = scheduler.reserve(task(1), resources(1)).unwrap() else {
            panic!("reservation rejected");
        };
        let mut replacement = lease.resources.clone();
        replacement.workspace_id = Some("replacement-workspace".into());
        replacement.environment_id = Some("replacement-environment".into());

        assert!(matches!(
            scheduler
                .bind_resources(&lease.task, lease.lease_generation, &lease.owner, replacement)
                .unwrap(),
            ReservationOutcome::Rejected {
                reason: CollisionReason::LeaseExpired { lease_generation, .. }
            } if lease_generation == lease.lease_generation
        ));
        assert_eq!(scheduler.status().unwrap().recovery_needed.len(), 1);
    }
}
