//! Cross-role, process-global registry of resident plugin hosts.
//!
//! Animus 0.7 Pillar 1 Layer B ("cross-role host-sharing"): a single plugin
//! process can serve MULTIPLE kinds/roles at once (a plugin whose manifest
//! declares `plugin_kinds: ["subject_backend", "config_source", "queue"]` runs
//! as ONE process, not one-per-role). Before this registry each resident-style
//! role (`subject_backend`, `config_source`, `workflow_journal`) kept its own
//! host cache keyed differently (subject: by plugin name; config/journal: by
//! project root), so a multi-role plugin was spawned once per role.
//!
//! The registry unifies those caches under ONE key: the plugin's canonical
//! binary path plus its mtime-nanos. Every role that resolves the same binary
//! therefore shares the same live process — spawned once, handshaked once,
//! reused across roles. An in-place plugin upgrade (new bytes at the same path)
//! changes the mtime, so the stale entry is bypassed and a fresh host spawns.
//!
//! ## Eviction safety
//!
//! Hosts are LRU-bounded by a soft cap (`max_live`). Every handed-out host is
//! pinned by a [`ResidentHostLease`] whose liveness token (`active`) is cloned
//! into the cache entry; eviction only reaps entries whose token
//! `Arc::strong_count == 1` (the cache's own reference), so an in-flight RPC or
//! a long-lived subscription is never torn down out from under its user. When
//! every cached host is leased the live set may briefly exceed the soft cap; it
//! is still bounded by the host's process-slot cap, and shrinks back as leases
//! drop (the next `get_or_spawn` — even a fast-path cache hit — opportunistically
//! drains the overshoot).
//!
//! ## Anti-deadlock contract
//!
//! - The live-host map lives behind a single [`std::sync::Mutex`] that is NEVER
//!   held across an `.await` (only in-memory map mutation happens under it).
//! - The slow `spawn().await` runs under a *per-binary* [`tokio::sync::Mutex`]
//!   (`spawn_locks`) so only one task spawns a given binary while every other
//!   task — for that binary or any other — keeps making progress. Two concurrent
//!   resolves of the same not-yet-live binary spawn it exactly once (the loser
//!   observes the cache populated when it re-checks under the spawn lock).

use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Instant;

use anyhow::Result;

use crate::PluginHost;

/// Default soft cap on concurrently-live resident hosts. Matches the historical
/// `subject_router` per-router cap so behaviour is unchanged for the common
/// single-role case. Override with [`RESIDENT_HOST_CACHE_MAX_ENV`].
const DEFAULT_MAX_LIVE: usize = 8;

/// Env override for [`DEFAULT_MAX_LIVE`] (a non-zero `usize`; unset / unparseable
/// / zero falls back to the default). Read once when the process-global registry
/// is first initialised.
pub const RESIDENT_HOST_CACHE_MAX_ENV: &str = "ANIMUS_RESIDENT_HOST_CACHE_MAX";

/// Legacy per-router subject-host cap env var, honored as a fallback so
/// deployments that set it (to stay under a tight process/thread budget) keep
/// their cap now that subject lazy hosts share this registry. The new
/// [`RESIDENT_HOST_CACHE_MAX_ENV`] wins when both are set.
pub const LEGACY_SUBJECT_HOST_CACHE_MAX_ENV: &str = "ANIMUS_SUBJECT_HOST_CACHE_MAX";

fn env_max_live() -> usize {
    for key in [RESIDENT_HOST_CACHE_MAX_ENV, LEGACY_SUBJECT_HOST_CACHE_MAX_ENV] {
        if let Ok(value) = std::env::var(key) {
            match value.trim().parse::<usize>() {
                Ok(0) | Err(_) => {}
                Ok(n) => return n,
            }
        }
    }
    DEFAULT_MAX_LIVE
}

/// Best-effort binary mtime (nanos since epoch); `0` when unavailable. Used only
/// as a cache-invalidation signal: an unreadable mtime collides to `0`, and a
/// later successful read differs and forces a re-spawn.
pub fn binary_mtime_nanos(path: &Path) -> u128 {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

/// Canonicalize a binary path so two spellings of the same file (symlinks,
/// `/var` vs `/private/var` on macOS) collapse to one cache key. Falls back to
/// the raw path when canonicalization fails (e.g. the binary was removed).
fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Cache key: canonical binary path, its mtime-nanos, and a spawn-context
/// fingerprint.
///
/// The spawn-context fingerprint (see [`spawn_context_fingerprint`]) is
/// load-bearing for correctness: two roles may resolve the SAME binary yet need
/// DIFFERENT spawn options — e.g. `config_source` / `workflow_journal` forward
/// the full parent env (they replace the kernel's env-reading YAML interpolator)
/// while `subject_backend` forwards only manifest-declared env (the v0.4.x
/// secret trust boundary) and pins a project-root working directory. Reusing one
/// process across incompatible spawn contexts would run config loads without the
/// env they need, or subject calls in the wrong cwd. Keying on the fingerprint
/// means only roles with an IDENTICAL effective spawn context share a process
/// (config_source ⟷ workflow_journal do; subject_backend stays separate), which
/// is exactly the safe subset of cross-role sharing.
type HostKey = (PathBuf, u128, String);

/// Fingerprint the role-varying parts of a plugin spawn: the forwarded-env
/// allowlist, the working directory, and the notification-buffer hint. Manifest
/// `env_required` is intentionally excluded — it is constant for a given binary
/// (path + mtime already key that), so only the parts that differ BETWEEN roles
/// of the same binary belong here. Two spawns share a resident process iff this
/// fingerprint matches.
pub fn spawn_context_fingerprint(
    forwarded_env: &[String],
    working_dir: Option<&Path>,
    notification_buffer: Option<usize>,
) -> String {
    use std::hash::{Hash, Hasher};
    let mut names: Vec<&str> = forwarded_env.iter().map(String::as_str).collect();
    names.sort_unstable();
    names.dedup();
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for name in &names {
        name.hash(&mut hasher);
    }
    let env_hash = hasher.finish();
    let cwd = working_dir.map(|p| p.to_string_lossy().into_owned());
    format!("cwd={cwd:?};notif={notification_buffer:?};env={env_hash:016x}")
}

/// Source of monotonic [`CachedHostEntry::generation`] ids. Lets a death-like
/// retry reap ONLY the exact host that failed: a concurrent caller may have
/// already replaced the dead host with a fresh one, and we must not shut down
/// that healthy replacement.
fn next_generation() -> u64 {
    static GEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    GEN.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

/// One cached, resident plugin host plus its liveness token and generation.
struct CachedHostEntry {
    host: PluginHost,
    /// Last time a lease was minted for this host; LRU eviction reaps the
    /// smallest `last_used` first.
    last_used: Instant,
    /// Liveness token cloned into every [`ResidentHostLease`]. `strong_count`
    /// reveals whether any caller is currently using the host; eviction skips
    /// entries with a count above 1 (the cache's own reference).
    active: Arc<()>,
    /// Monotonic id assigned at insert; keys generation-scoped invalidation.
    generation: u64,
}

/// A borrow of a cached resident host that pins it against LRU eviction for its
/// lifetime. Hold it across the RPC (or park it inside a long-lived
/// subscription); drop it to let the host become evictable again.
pub struct ResidentHostLease {
    host: PluginHost,
    generation: u64,
    /// Kept alive purely for its `Drop`: while held, the cache entry's `active`
    /// `strong_count` is above 1, so eviction skips it. Never read.
    _active: Arc<()>,
}

impl ResidentHostLease {
    /// The leased host. The returned reference is valid for as long as the lease
    /// is held; the host stays pinned against eviction meanwhile.
    pub fn host(&self) -> &PluginHost {
        &self.host
    }

    /// The generation id of the backing cache entry. Pass to
    /// [`ResidentHostRegistry::invalidate_generation`] to reap ONLY this exact
    /// host on a death-like failure.
    pub fn generation(&self) -> u64 {
        self.generation
    }
}

/// Cross-role, LRU-bounded cache of resident plugin hosts keyed by
/// `(canonical binary path, mtime)`. See the module docs for the sharing model,
/// eviction safety, and anti-deadlock contract.
pub struct ResidentHostRegistry {
    hosts: Mutex<HashMap<HostKey, CachedHostEntry>>,
    spawn_locks: Mutex<HashMap<HostKey, Arc<tokio::sync::Mutex<()>>>>,
    max_live: usize,
}

impl ResidentHostRegistry {
    /// Construct a registry with the given soft cap on concurrently-live hosts.
    pub fn new(max_live: usize) -> Self {
        Self { hosts: Mutex::new(HashMap::new()), spawn_locks: Mutex::new(HashMap::new()), max_live: max_live.max(1) }
    }

    /// Resolve a leased resident host for `(path, mtime, spawn_context)`,
    /// spawning + handshaking it on first use via `spawn`.
    ///
    /// `spawn_context` MUST be a [`spawn_context_fingerprint`] of the exact spawn
    /// options `spawn` will use. Two callers share a process ONLY when their
    /// fingerprints match, so a role must never hand off a `spawn` closure whose
    /// effective env / cwd / notification hint differs from `spawn_context`.
    ///
    /// `spawn` MUST return a fully-initialized host (spawned AND handshaked) so
    /// the cached process is ready for every role that later shares it — the
    /// handshake happens exactly once per process, not once per role.
    ///
    /// The returned [`ResidentHostLease`] pins the host against LRU eviction for
    /// as long as it is held.
    pub async fn get_or_spawn<F, Fut>(
        &self,
        path: &Path,
        mtime: u128,
        spawn_context: &str,
        spawn: F,
    ) -> Result<ResidentHostLease>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<PluginHost>>,
    {
        let key: HostKey = (canonical(path), mtime, spawn_context.to_string());

        // Fast path: already live. Mint a lease + bump recency, and
        // opportunistically drain any now-idle hosts left over the soft cap by a
        // prior overshoot. Victim shutdown runs after the map lock is released.
        {
            let (lease, evicted) = {
                let mut hosts = self.hosts.lock().unwrap_or_else(|p| p.into_inner());
                match Self::lease_locked(&mut hosts, &key) {
                    Some(lease) => (Some(lease), Self::drain_over_cap_locked(&mut hosts, self.max_live)),
                    None => (None, Vec::new()),
                }
            };
            Self::shutdown_hosts(evicted).await;
            if let Some(lease) = lease {
                return Ok(lease);
            }
        }

        // Per-key spawn serialization: clone-out the key's spawn lock (creating it
        // on first sight) so the tiny `spawn_locks` map lock is held only
        // momentarily, then await on the per-key lock.
        let spawn_lock = {
            let mut locks = self.spawn_locks.lock().unwrap_or_else(|p| p.into_inner());
            locks.entry(key.clone()).or_insert_with(|| Arc::new(tokio::sync::Mutex::new(()))).clone()
        };
        let _spawn_guard = spawn_lock.lock().await;

        // Re-check under the spawn lock: a racing task may have spawned this
        // binary while we waited. If so, lease its host instead of double-spawning.
        {
            let mut hosts = self.hosts.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(lease) = Self::lease_locked(&mut hosts, &key) {
                return Ok(lease);
            }
        }

        // Spawn + handshake OUTSIDE every map lock so a slow spawn never blocks
        // routing for other binaries.
        let host = spawn().await?;

        // Insert + mint a lease (which bumps recency), then drain UNLEASED LRU
        // hosts back toward the soft cap. Eviction shuts victims down OUTSIDE the
        // map lock. The just-inserted host is leased, so it can never be a victim.
        let (lease, evicted) = {
            let mut hosts = self.hosts.lock().unwrap_or_else(|p| p.into_inner());
            // An in-place binary upgrade changes the mtime, so the fresh host is
            // stored under a NEW key. Reap any stale-mtime entries for the same
            // canonical path + spawn context first, so their processes (and plugin
            // slots / DB connections) don't leak until later LRU pressure. Skip
            // leased stale entries — an in-flight call still owns them; they become
            // idle and are reaped by a later drain once the lease drops.
            let stale: Vec<PluginHost> = {
                let stale_keys: Vec<HostKey> = hosts
                    .iter()
                    .filter(|(k, e)| k.0 == key.0 && k.2 == key.2 && k.1 != key.1 && Arc::strong_count(&e.active) == 1)
                    .map(|(k, _)| k.clone())
                    .collect();
                stale_keys.into_iter().filter_map(|k| hosts.remove(&k)).map(|e| e.host).collect()
            };
            let generation = next_generation();
            hosts.insert(
                key.clone(),
                CachedHostEntry { host, last_used: Instant::now(), active: Arc::new(()), generation },
            );
            let lease = Self::lease_locked(&mut hosts, &key).expect("host just inserted");
            let mut evicted = stale;
            evicted.extend(Self::drain_over_cap_locked(&mut hosts, self.max_live));
            (lease, evicted)
        };
        Self::shutdown_hosts(evicted).await;

        Ok(lease)
    }

    /// Drop + reap the cached host for `(path, mtime, spawn_context)` ONLY if its
    /// generation is still `generation`. A concurrent caller may already have
    /// reaped the dead host and installed a fresh replacement; reaping by
    /// generation guarantees we never shut down that healthy replacement.
    /// Best-effort: shuts the evicted process down (a no-op if it is already
    /// dead).
    pub async fn invalidate_generation(&self, path: &Path, mtime: u128, spawn_context: &str, generation: u64) {
        let key: HostKey = (canonical(path), mtime, spawn_context.to_string());
        let victim = {
            let mut hosts = self.hosts.lock().unwrap_or_else(|p| p.into_inner());
            match hosts.get(&key) {
                Some(entry) if entry.generation == generation => hosts.remove(&key).map(|e| e.host),
                _ => None,
            }
        };
        if let Some(host) = victim {
            let _ = host.shutdown().await;
        }
    }

    /// Shut down every resident host in the registry and clear the map. Wired
    /// into daemon graceful-shutdown teardown. Idempotent.
    pub async fn shutdown_all(&self) {
        let hosts: Vec<PluginHost> = {
            let mut hosts = self.hosts.lock().unwrap_or_else(|p| p.into_inner());
            hosts.drain().map(|(_, e)| e.host).collect()
        };
        Self::shutdown_hosts_bare(hosts).await;
    }

    /// Mint a lease for the cached host at `key`, bumping its recency. `None`
    /// when no host is cached for the key.
    fn lease_locked(hosts: &mut HashMap<HostKey, CachedHostEntry>, key: &HostKey) -> Option<ResidentHostLease> {
        let entry = hosts.get_mut(key)?;
        entry.last_used = Instant::now();
        Some(ResidentHostLease {
            host: entry.host.clone(),
            generation: entry.generation,
            _active: entry.active.clone(),
        })
    }

    /// Evict UNLEASED least-recently-used hosts until the live count is back to
    /// `max_live` (or no more unleased hosts remain). Returns the evicted hosts
    /// so the caller can shut them down outside the map lock.
    fn drain_over_cap_locked(hosts: &mut HashMap<HostKey, CachedHostEntry>, max_live: usize) -> Vec<PluginHost> {
        let mut evicted = Vec::new();
        while hosts.len() > max_live {
            // Least-recently-used UNLEASED entry (token count 1 = cache-only).
            let victim = hosts
                .iter()
                .filter(|(_, e)| Arc::strong_count(&e.active) == 1)
                .min_by_key(|(_, e)| e.last_used)
                .map(|(k, _)| k.clone());
            match victim {
                Some(key) => {
                    if let Some(entry) = hosts.remove(&key) {
                        evicted.push(entry.host);
                    }
                }
                None => break,
            }
        }
        evicted
    }

    /// Best-effort shutdown of a batch of evicted hosts, outside any map lock.
    async fn shutdown_hosts(evicted: Vec<PluginHost>) {
        if evicted.is_empty() {
            return;
        }
        tracing::debug!(
            count = evicted.len(),
            "evicting least-recently-used idle resident host(s) to stay under the soft cap"
        );
        Self::shutdown_hosts_bare(evicted).await;
    }

    async fn shutdown_hosts_bare(hosts: Vec<PluginHost>) {
        for host in hosts {
            let _ = host.shutdown().await;
        }
    }

    /// Test helper: number of currently-live cached hosts.
    #[cfg(any(test, feature = "test-support"))]
    pub fn live_len(&self) -> usize {
        self.hosts.lock().unwrap_or_else(|p| p.into_inner()).len()
    }

    /// Test helper: whether a host is cached for `(path, mtime, spawn_context)`.
    #[cfg(any(test, feature = "test-support"))]
    pub fn contains(&self, path: &Path, mtime: u128, spawn_context: &str) -> bool {
        let key: HostKey = (canonical(path), mtime, spawn_context.to_string());
        self.hosts.lock().unwrap_or_else(|p| p.into_inner()).contains_key(&key)
    }
}

/// Process-global registry slot. Behind an `RwLock<Option<..>>` (not a bare
/// `OnceLock`) so lifecycle tests can swap in a fresh registry per run without
/// leaving the previous one wired to new resolves — mirroring the
/// `status::GLOBAL_STATUS_REGISTRY` idiom.
static GLOBAL: RwLock<Option<Arc<ResidentHostRegistry>>> = RwLock::new(None);

/// The process-global resident-host registry, initialising it (from the
/// [`RESIDENT_HOST_CACHE_MAX_ENV`] cap) on first use. Every resident-style role
/// resolves through this ONE instance so a multi-role plugin binary is shared.
pub fn global_resident_host_registry() -> Arc<ResidentHostRegistry> {
    {
        let guard = GLOBAL.read().unwrap_or_else(|p| p.into_inner());
        if let Some(registry) = guard.as_ref() {
            return registry.clone();
        }
    }
    let mut guard = GLOBAL.write().unwrap_or_else(|p| p.into_inner());
    if let Some(registry) = guard.as_ref() {
        return registry.clone();
    }
    let registry = Arc::new(ResidentHostRegistry::new(env_max_live()));
    *guard = Some(registry.clone());
    registry
}

/// Test helper: install a fresh process-global registry with the given soft cap,
/// replacing any previous one. Returns the installed registry so a test can also
/// inspect it directly. Serialize callers via `TEST_SLOT_FACTORY_GUARD` — the
/// slot is process-global.
#[cfg(any(test, feature = "test-support"))]
pub fn install_resident_host_registry_for_test(max_live: usize) -> Arc<ResidentHostRegistry> {
    let registry = Arc::new(ResidentHostRegistry::new(max_live));
    *GLOBAL.write().unwrap_or_else(|p| p.into_inner()) = Some(registry.clone());
    registry
}

#[cfg(test)]
mod tests {
    use animus_plugin_protocol::{InitializeResult, PluginCapabilities, PluginInfo, RpcRequest, RpcResponse};
    use tokio::io::{duplex, AsyncBufReadExt, AsyncWriteExt, BufReader};

    use super::*;

    /// An in-memory fake plugin host that answers the `initialize` handshake.
    /// Increments `spawn_count` once per construction so a test can assert how
    /// many processes were "spawned".
    async fn fake_host(name: &str, spawn_count: Arc<std::sync::atomic::AtomicUsize>) -> PluginHost {
        spawn_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let (host_reader, mut plugin_writer) = duplex(8192);
        let (plugin_reader, host_writer) = duplex(8192);
        let name_for_task = name.to_string();
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
                                name: name_for_task.clone(),
                                version: "0.1.0".to_string(),
                                plugin_kind: "subject_backend".to_string(),
                                plugin_kinds: vec!["subject_backend".to_string(), "config_source".to_string(),],
                                description: None,
                            },
                            capabilities: PluginCapabilities::default(),
                            kind_capabilities: std::collections::HashMap::new(),
                        }),
                    ),
                    "initialized" => continue,
                    method => RpcResponse::ok(request.id, serde_json::json!({ "method": method })),
                };
                let mut encoded = serde_json::to_string(&response).expect("encode");
                encoded.push('\n');
                plugin_writer.write_all(encoded.as_bytes()).await.expect("write");
            }
        });
        PluginHost::from_streams(name, host_reader, host_writer)
    }

    /// A multi-kind plugin binary resolved from two DIFFERENT role paths yields
    /// the SAME underlying process: spawned once, handshaked once, shared.
    #[tokio::test]
    async fn cross_role_resolution_shares_one_process() {
        let registry = ResidentHostRegistry::new(8);
        // A single binary on disk, standing in for a plugin whose manifest
        // declares plugin_kinds = ["subject_backend", "config_source"].
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("multi-role-plugin");
        std::fs::write(&path, b"binary").unwrap();
        let mtime = binary_mtime_nanos(&path);
        let spawns = Arc::new(std::sync::atomic::AtomicUsize::new(0));

        // Both roles use the SAME spawn context (same env / cwd / notif), so they
        // must share one process — the flagship cross-role case.
        let ctx = spawn_context_fingerprint(&[], None, None);

        // Role A (e.g. subject_backend) resolves the binary → one spawn.
        let spawns_a = spawns.clone();
        let lease_a = registry
            .get_or_spawn(&path, mtime, &ctx, move || {
                let spawns_a = spawns_a.clone();
                async move {
                    let host = fake_host("multi", spawns_a).await;
                    host.handshake().await?;
                    Ok(host)
                }
            })
            .await
            .expect("role A resolves");

        // Role B (e.g. config_source) resolves the SAME binary → cache hit, no
        // second spawn, and the very same process (same PID / stdin channel).
        let spawns_b = spawns.clone();
        let lease_b = registry
            .get_or_spawn(&path, mtime, &ctx, move || {
                let spawns_b = spawns_b.clone();
                async move {
                    let host = fake_host("multi", spawns_b).await;
                    host.handshake().await?;
                    Ok(host)
                }
            })
            .await
            .expect("role B resolves");

        assert_eq!(spawns.load(std::sync::atomic::Ordering::SeqCst), 1, "multi-role binary spawned exactly once");
        assert_eq!(registry.live_len(), 1, "one shared process cached, not one-per-role");
        // Both leases point at the same host handle (clones of one process).
        assert!(std::ptr::eq(lease_a.host().inner_ptr(), lease_b.host().inner_ptr()), "both roles share one host");

        // A DIFFERENT spawn context (e.g. subject_backend's project-root cwd vs
        // config_source's full-env forward) must NOT reuse the process — this is
        // the trust-boundary / cwd safety the spawn-context key guarantees.
        let other_ctx = spawn_context_fingerprint(&["DATABASE_URL".to_string()], Some(dir.path()), Some(64));
        assert_ne!(ctx, other_ctx, "distinct spawn contexts fingerprint differently");
        let spawns_c = spawns.clone();
        let _lease_c = registry
            .get_or_spawn(&path, mtime, &other_ctx, move || {
                let spawns_c = spawns_c.clone();
                async move {
                    let host = fake_host("multi", spawns_c).await;
                    host.handshake().await?;
                    Ok(host)
                }
            })
            .await
            .expect("distinct-context role resolves");
        assert_eq!(
            spawns.load(std::sync::atomic::Ordering::SeqCst),
            2,
            "a distinct spawn context spawns its own process (no unsafe reuse)"
        );
    }

    /// LRU eviction skips hosts with an active lease and reaps only idle ones.
    #[tokio::test]
    async fn lru_eviction_skips_leased_hosts() {
        // Cap of 1: any second live host would trigger eviction of the LRU
        // UNLEASED host. We hold a lease on the first, so it must survive.
        let registry = ResidentHostRegistry::new(1);
        let dir = tempfile::tempdir().unwrap();
        let spawns = Arc::new(std::sync::atomic::AtomicUsize::new(0));

        let path_a = dir.path().join("a");
        std::fs::write(&path_a, b"a").unwrap();
        let mtime_a = binary_mtime_nanos(&path_a);
        let path_b = dir.path().join("b");
        std::fs::write(&path_b, b"b").unwrap();
        let mtime_b = binary_mtime_nanos(&path_b);
        let ctx = spawn_context_fingerprint(&[], None, None);

        // Lease A and KEEP it held.
        let spawns_a = spawns.clone();
        let lease_a = registry
            .get_or_spawn(&path_a, mtime_a, &ctx, move || {
                let spawns_a = spawns_a.clone();
                async move {
                    let host = fake_host("a", spawns_a).await;
                    host.handshake().await?;
                    Ok(host)
                }
            })
            .await
            .expect("lease a");

        // Resolve B while A is still leased. This pushes the live count to 2,
        // over the cap of 1 — but A is leased, so eviction must skip it. B is
        // just-inserted and also leased, so neither is evicted here.
        let spawns_b = spawns.clone();
        let lease_b = registry
            .get_or_spawn(&path_b, mtime_b, &ctx, move || {
                let spawns_b = spawns_b.clone();
                async move {
                    let host = fake_host("b", spawns_b).await;
                    host.handshake().await?;
                    Ok(host)
                }
            })
            .await
            .expect("lease b");

        assert!(registry.contains(&path_a, mtime_a, &ctx), "leased host A must not be evicted under cap pressure");
        assert!(registry.contains(&path_b, mtime_b, &ctx), "host B present");
        assert_eq!(registry.live_len(), 2, "both leased hosts live, temporarily over the soft cap");

        // Drop B's lease so it becomes idle; A is still leased. A subsequent
        // resolve of A (fast-path hit) drains the overshoot: the idle B is
        // reaped, the leased A survives, back to the cap of 1.
        drop(lease_b);
        let never_called = registry
            .get_or_spawn(&path_a, mtime_a, &ctx, || async { panic!("must not re-spawn A: it is cached") })
            .await
            .expect("re-lease a");
        assert!(registry.contains(&path_a, mtime_a, &ctx), "leased A survives the drain");
        assert!(!registry.contains(&path_b, mtime_b, &ctx), "idle B reaped by the over-cap drain");
        assert_eq!(registry.live_len(), 1, "drained back to the soft cap");

        drop((lease_a, never_called));
    }

    /// An in-place binary upgrade (same path + context, new mtime) reaps the
    /// stale-mtime process instead of leaking it under a distinct key.
    #[tokio::test]
    async fn stale_mtime_entry_is_reaped_on_upgrade() {
        let registry = ResidentHostRegistry::new(8);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("upgradable");
        std::fs::write(&path, b"v1").unwrap();
        let ctx = spawn_context_fingerprint(&[], None, None);
        let spawns = Arc::new(std::sync::atomic::AtomicUsize::new(0));

        // Resolve at mtime v1, then drop the lease so the entry is idle.
        let old_mtime = 111u128;
        {
            let spawns_v1 = spawns.clone();
            let _lease = registry
                .get_or_spawn(&path, old_mtime, &ctx, move || {
                    let spawns_v1 = spawns_v1.clone();
                    async move {
                        let host = fake_host("v1", spawns_v1).await;
                        host.handshake().await?;
                        Ok(host)
                    }
                })
                .await
                .expect("v1 resolves");
        }
        assert!(registry.contains(&path, old_mtime, &ctx), "v1 host cached");

        // Resolve the SAME path + context at a NEW mtime (simulating an in-place
        // upgrade). The stale v1 entry must be reaped, not left leaking.
        let new_mtime = 222u128;
        let spawns_v2 = spawns.clone();
        let _lease_v2 = registry
            .get_or_spawn(&path, new_mtime, &ctx, move || {
                let spawns_v2 = spawns_v2.clone();
                async move {
                    let host = fake_host("v2", spawns_v2).await;
                    host.handshake().await?;
                    Ok(host)
                }
            })
            .await
            .expect("v2 resolves");

        assert!(!registry.contains(&path, old_mtime, &ctx), "stale v1 mtime entry reaped on upgrade");
        assert!(registry.contains(&path, new_mtime, &ctx), "fresh v2 mtime entry present");
        assert_eq!(registry.live_len(), 1, "only the fresh host remains after an in-place upgrade");
    }
}
