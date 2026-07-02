//! Plugin manifest cache.
//!
//! `--manifest` probes are static metadata: a sha256-stable binary always
//! emits the same manifest. Discovery used to spawn 30 subprocesses on every
//! `animus daemon status` / `animus plugin list`, costing ~3s wall time.
//! This cache stores serialized [`PluginManifest`]s keyed by the binary's
//! sha256 under `~/.animus/cache/manifests/<sha256>.json` so the cache-hit
//! path is just `stat + read JSON`.
//!
//! The cache is best-effort: any I/O error falls back to a fresh `--manifest`
//! probe. Corrupted entries are silently re-probed, not deserialized into
//! garbage. Honors `ANIMUS_CACHE_DIR` for hermetic tests and
//! `ANIMUS_DISABLE_MANIFEST_CACHE=1` as a kill-switch.
//!
//! Invalidation is automatic for lockfile-tracked plugins because every
//! install/upgrade rewrites the artifact sha256. As a belt-and-suspenders
//! safety net for hand-replaced binaries, [`ManifestCache::lookup_for_path`]
//! also rejects cached entries whose mtime predates the binary's mtime.

use std::fs;
use std::io::{BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use animus_plugin_protocol::PluginManifest;
use anyhow::{Context, Result};
use sha2::{Digest, Sha256};

const SHA256_HEX_LEN: usize = 64;

pub struct ManifestCache {
    root: PathBuf,
    enabled: bool,
}

impl ManifestCache {
    pub fn from_default() -> Self {
        Self::from_root(default_cache_root())
    }

    pub fn from_root(root: PathBuf) -> Self {
        let enabled = !cache_disabled_via_env();
        Self { root, enabled }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    pub fn entry_path(&self, sha256: &str) -> PathBuf {
        self.root.join(format!("{}.json", sha256))
    }

    /// Look up a cached manifest by sha256. Returns `None` on miss, parse
    /// error, or any I/O failure — never an `Err`.
    pub fn lookup(&self, sha256: &str) -> Option<PluginManifest> {
        if !self.enabled || !is_valid_sha256(sha256) {
            return None;
        }
        let path = self.entry_path(sha256);
        let bytes = fs::read(&path).ok()?;
        serde_json::from_slice::<PluginManifest>(&bytes).ok()
    }

    /// Same as [`Self::lookup`], but also rejects the cached entry when
    /// `binary_path`'s mtime is newer than the cache file's mtime. Covers
    /// the hand-replaced-binary edge case where sha256 reuse can't be
    /// trusted because the operator may have dropped a different binary at
    /// the same install location without rewriting the lockfile.
    pub fn lookup_for_path(&self, sha256: &str, binary_path: &Path) -> Option<PluginManifest> {
        if !self.enabled || !is_valid_sha256(sha256) {
            return None;
        }
        let path = self.entry_path(sha256);
        let cache_meta = fs::metadata(&path).ok()?;
        let bin_meta = fs::metadata(binary_path).ok()?;
        if let (Ok(bin_mtime), Ok(cache_mtime)) = (bin_meta.modified(), cache_meta.modified()) {
            if bin_mtime > cache_mtime {
                return None;
            }
        }
        let bytes = fs::read(&path).ok()?;
        serde_json::from_slice::<PluginManifest>(&bytes).ok()
    }

    /// Insert (or overwrite) a cache entry for `sha256`. Writes to a
    /// temp file and renames so concurrent readers never observe a
    /// half-written JSON blob. Returns `Ok(())` on success; cache I/O
    /// errors are surfaced so callers can log them, but callers MUST
    /// treat the cache as best-effort and never fail discovery on a
    /// cache write failure.
    pub fn insert(&self, sha256: &str, manifest: &PluginManifest) -> Result<()> {
        if !self.enabled {
            return Ok(());
        }
        if !is_valid_sha256(sha256) {
            anyhow::bail!("refusing to cache manifest under invalid sha256 key");
        }
        ensure_cache_root(&self.root)?;
        let serialized = serde_json::to_vec(manifest).context("failed to serialize manifest for cache")?;
        let final_path = self.entry_path(sha256);
        let mut tmp_path = final_path.clone();
        let tmp_name = format!("{}.tmp-{}.{}", sha256, std::process::id(), uuid::Uuid::new_v4().simple());
        tmp_path.set_file_name(tmp_name);
        {
            let mut handle =
                fs::File::create(&tmp_path).with_context(|| format!("failed to open {}", tmp_path.display()))?;
            handle.write_all(&serialized).with_context(|| format!("failed to write {}", tmp_path.display()))?;
            handle.sync_all().ok();
        }
        if let Err(err) = fs::rename(&tmp_path, &final_path) {
            let _ = fs::remove_file(&tmp_path);
            return Err(
                anyhow::Error::new(err).context(format!("failed to install cache entry at {}", final_path.display()))
            );
        }
        Ok(())
    }

    /// Remove every cached manifest entry under the cache root. Returns the
    /// number of files removed. Missing root counts as zero. Used by
    /// `animus plugin cache clear`.
    pub fn clear(&self) -> Result<usize> {
        if !self.root.exists() {
            return Ok(0);
        }
        let mut removed = 0;
        let entries =
            fs::read_dir(&self.root).with_context(|| format!("failed to read cache dir {}", self.root.display()))?;
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() && fs::remove_file(&path).is_ok() {
                removed += 1;
            }
        }
        Ok(removed)
    }

    /// Enumerate cached manifest entries. Each tuple is
    /// `(sha256, mtime, size_bytes)`. Used by `animus plugin cache list`.
    pub fn list(&self) -> Result<Vec<CachedEntry>> {
        let mut out = Vec::new();
        if !self.root.exists() {
            return Ok(out);
        }
        let entries =
            fs::read_dir(&self.root).with_context(|| format!("failed to read cache dir {}", self.root.display()))?;
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(file_name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            let Some(stem) = file_name.strip_suffix(".json") else {
                continue;
            };
            if !is_valid_sha256(stem) {
                continue;
            }
            let meta = match fs::metadata(&path) {
                Ok(m) => m,
                Err(_) => continue,
            };
            out.push(CachedEntry {
                sha256: stem.to_string(),
                size_bytes: meta.len(),
                mtime: meta.modified().ok(),
                path,
            });
        }
        out.sort_by(|a, b| a.sha256.cmp(&b.sha256));
        Ok(out)
    }

    /// Compute the lowercase hex sha256 of a binary on disk. Streams the
    /// file through a buffered reader rather than slurping it whole so
    /// discovery memory does not scale with plugin binary size — a
    /// 200 MB build-artifact accidentally dropped into the plugin dir
    /// won't blow up `animus plugin list`. Used by callers when a
    /// lockfile-recorded hash is not available (path/url installs,
    /// hand-dropped binaries). Codex round 7 P2.
    pub fn hash_binary(path: &Path) -> Result<String> {
        let file = fs::File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
        let mut reader = BufReader::with_capacity(64 * 1024, file);
        let mut hasher = Sha256::new();
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let n = reader.read(&mut buf).with_context(|| format!("failed to read {}", path.display()))?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
        }
        Ok(format!("{:x}", hasher.finalize()))
    }
}

#[derive(Debug, Clone)]
pub struct CachedEntry {
    pub sha256: String,
    pub size_bytes: u64,
    pub mtime: Option<SystemTime>,
    pub path: PathBuf,
}

fn cache_disabled_via_env() -> bool {
    matches!(std::env::var("ANIMUS_DISABLE_MANIFEST_CACHE").ok().as_deref(), Some("1" | "true" | "yes"))
}

fn default_cache_root() -> PathBuf {
    if let Ok(value) = std::env::var("ANIMUS_CACHE_DIR") {
        let trimmed = value.trim();
        if !trimmed.is_empty() {
            return PathBuf::from(trimmed).join("manifests");
        }
    }
    animus_home_cache().join("manifests")
}

fn animus_home_cache() -> PathBuf {
    if let Ok(value) = std::env::var("ANIMUS_CONFIG_DIR") {
        let trimmed = value.trim();
        if !trimmed.is_empty() {
            return PathBuf::from(trimmed).join("cache");
        }
    }
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home).join(".animus").join("cache");
    }
    PathBuf::from(".animus").join("cache")
}

fn ensure_cache_root(root: &Path) -> Result<()> {
    if root.exists() {
        return Ok(());
    }
    fs::create_dir_all(root).with_context(|| format!("failed to create cache dir {}", root.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(root, fs::Permissions::from_mode(0o700));
    }
    Ok(())
}

fn is_valid_sha256(value: &str) -> bool {
    value.len() == SHA256_HEX_LEN && value.bytes().all(|b| b.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;
    use animus_plugin_protocol::PluginManifest;
    use tempfile::tempdir;

    /// Cargo runs tests in the same process on multiple threads. Any test
    /// that mutates the process-wide env (e.g. toggles
    /// `ANIMUS_DISABLE_MANIFEST_CACHE`) MUST hold the CRATE-WIDE mutex to
    /// avoid poisoning concurrent cache constructions in other tests
    /// (notably the discovery integration tests). Codex round 4 P2.
    use crate::TEST_ENV_GUARD as ENV_GUARD;

    fn sample_manifest(name: &str) -> PluginManifest {
        PluginManifest {
            name: name.to_string(),
            version: "0.1.0".into(),
            plugin_kind: "custom".into(),
            plugin_kinds: vec![],
            description: "test".into(),
            protocol_version: "1.0.0".into(),
            capabilities: vec![],
            env_required: vec![],
            notification_buffer_size: None,
        }
    }

    #[test]
    fn insert_then_lookup_round_trips() {
        let dir = tempdir().unwrap();
        let cache = ManifestCache::from_root(dir.path().to_path_buf());
        let sha = "a".repeat(64);
        let manifest = sample_manifest("rt");
        cache.insert(&sha, &manifest).expect("insert");
        let got = cache.lookup(&sha).expect("hit");
        assert_eq!(got, manifest);
    }

    #[test]
    fn lookup_returns_none_on_miss() {
        let dir = tempdir().unwrap();
        let cache = ManifestCache::from_root(dir.path().to_path_buf());
        assert!(cache.lookup(&"b".repeat(64)).is_none());
    }

    #[test]
    fn lookup_rejects_invalid_sha_keys() {
        let dir = tempdir().unwrap();
        let cache = ManifestCache::from_root(dir.path().to_path_buf());
        assert!(cache.lookup("not-a-sha").is_none());
        assert!(cache.lookup(&"z".repeat(64)).is_none());
    }

    #[test]
    fn corrupt_cache_entry_misses_silently() {
        let dir = tempdir().unwrap();
        let cache = ManifestCache::from_root(dir.path().to_path_buf());
        let sha = "c".repeat(64);
        ensure_cache_root(&cache.root).unwrap();
        fs::write(cache.entry_path(&sha), b"{not json}").unwrap();
        assert!(cache.lookup(&sha).is_none(), "corrupt entry must not deserialize");
    }

    #[test]
    fn kill_switch_env_disables_cache() {
        let _guard = ENV_GUARD.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let dir = tempdir().unwrap();
        let prior = std::env::var_os("ANIMUS_DISABLE_MANIFEST_CACHE");
        std::env::set_var("ANIMUS_DISABLE_MANIFEST_CACHE", "1");
        let cache = ManifestCache::from_root(dir.path().to_path_buf());
        let sha = "d".repeat(64);
        let manifest = sample_manifest("off");
        cache.insert(&sha, &manifest).expect("insert is no-op when disabled");
        assert!(cache.lookup(&sha).is_none(), "disabled cache must always miss");
        match prior {
            Some(prev) => std::env::set_var("ANIMUS_DISABLE_MANIFEST_CACHE", prev),
            None => std::env::remove_var("ANIMUS_DISABLE_MANIFEST_CACHE"),
        }
    }

    #[test]
    fn lookup_for_path_rejects_when_binary_is_newer_than_cache_entry() {
        let dir = tempdir().unwrap();
        let cache = ManifestCache::from_root(dir.path().join("cache"));
        let sha = "e".repeat(64);
        let manifest = sample_manifest("stale");
        let binary = dir.path().join("binary");
        fs::write(&binary, b"v1").unwrap();
        cache.insert(&sha, &manifest).expect("insert");
        std::thread::sleep(std::time::Duration::from_millis(1100));
        fs::write(&binary, b"v2-with-newer-mtime").unwrap();
        assert!(cache.lookup_for_path(&sha, &binary).is_none(), "newer binary mtime must invalidate cache");
        assert!(cache.lookup(&sha).is_some(), "raw lookup still returns the cached entry");
    }

    #[test]
    fn lookup_for_path_serves_when_binary_unchanged() {
        let dir = tempdir().unwrap();
        let cache = ManifestCache::from_root(dir.path().join("cache"));
        let sha = "1".repeat(64);
        let manifest = sample_manifest("fresh");
        let binary = dir.path().join("binary");
        fs::write(&binary, b"contents").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(50));
        cache.insert(&sha, &manifest).expect("insert");
        let got = cache.lookup_for_path(&sha, &binary).expect("hit");
        assert_eq!(got, manifest);
    }

    #[test]
    fn clear_removes_every_cached_entry() {
        let dir = tempdir().unwrap();
        let cache = ManifestCache::from_root(dir.path().to_path_buf());
        for byte in 0..3u8 {
            let sha = format!("{:0>64x}", byte);
            cache.insert(&sha, &sample_manifest("c")).unwrap();
        }
        assert_eq!(cache.list().unwrap().len(), 3);
        let removed = cache.clear().unwrap();
        assert_eq!(removed, 3);
        assert!(cache.list().unwrap().is_empty());
    }

    #[test]
    fn list_skips_non_sha_files() {
        let dir = tempdir().unwrap();
        let cache = ManifestCache::from_root(dir.path().to_path_buf());
        ensure_cache_root(&cache.root).unwrap();
        fs::write(cache.root.join("README.txt"), b"hello").unwrap();
        let sha = "2".repeat(64);
        cache.insert(&sha, &sample_manifest("ok")).unwrap();
        let listed = cache.list().unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].sha256, sha);
    }

    #[test]
    fn hash_binary_matches_known_vector() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("blob");
        fs::write(&path, b"animus").unwrap();
        let sha = ManifestCache::hash_binary(&path).unwrap();
        let mut hasher = Sha256::new();
        hasher.update(b"animus");
        assert_eq!(sha, format!("{:x}", hasher.finalize()));
    }

    #[cfg(unix)]
    #[test]
    fn cache_root_is_created_with_0700_perms() {
        use std::os::unix::fs::PermissionsExt;
        let _guard = ENV_GUARD.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let dir = tempdir().unwrap();
        let cache_root = dir.path().join("manifests");
        let cache = ManifestCache::from_root(cache_root.clone());
        cache.insert(&"3".repeat(64), &sample_manifest("p")).unwrap();
        let mode = fs::metadata(&cache_root).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "cache root must be 0700, got {mode:o}");
    }
}
