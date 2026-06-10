//! Plugin lockfile (`.animus/plugins.lock`).
//!
//! Records `(name, version, artifact_sha256, signature_bundle_sha256,
//! installed_at)` for every plugin install so a later upgrade can refuse to
//! silently overwrite a binary whose hash no longer matches what the operator
//! originally approved. The lockfile is the user-visible audit + rollback
//! anchor for plugin installs; the install pipeline writes it on success and
//! removes the entry on uninstall.
//!
//! Format (TOML, matches the rest of the manifest surface):
//!
//! ```toml
//! schema_version = "1.0"
//! generated_at = "2026-05-26T..."
//!
//! [[plugins]]
//! name = "launchapp-dev/animus-provider-claude"
//! version = "v0.2.2"
//! artifact_sha256 = "abc123..."
//! signature_bundle_sha256 = "def456..."
//! installed_at = "2026-05-26T..."
//! ```
//!
//! Resolution order (`PluginLockfile::default_path`):
//! 1. Project-local `<project_root>/.animus/plugins.lock` when `project_root`
//!    is supplied AND the file exists OR the parent `.animus/` exists.
//! 2. Global `~/.animus/plugins.lock` fallback.

use std::collections::BTreeSet;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::Utc;
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Current lockfile schema version. Bump when the on-disk shape changes in
/// a non-backward-compatible way.
pub const LOCKFILE_SCHEMA_VERSION: &str = "1.0";

/// One install entry inside the lockfile. The tuple
/// `(name, version, artifact_sha256)` is the integrity claim the install
/// pipeline checks on every subsequent upgrade.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockEntry {
    /// Canonical install identifier — typically `owner/repo` for release-source
    /// installs (e.g. `launchapp-dev/animus-provider-claude`). For `--path`
    /// and `--url` installs callers should pass the logical plugin name so
    /// uninstall/upgrade keep working.
    pub name: String,
    /// Release tag or local version string (e.g. `v0.2.2`). Empty string for
    /// sources without a tag is allowed but discouraged.
    #[serde(default)]
    pub version: String,
    /// Lowercase hex sha256 of the installed binary (the on-disk artifact).
    pub artifact_sha256: String,
    /// Lowercase hex sha256 of the cosign signature bundle that accompanied
    /// this install, if any. `None` when the install proceeded under
    /// `warn`/`disabled` without a bundle present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature_bundle_sha256: Option<String>,
    /// RFC 3339 timestamp captured at install time.
    pub installed_at: String,
    /// User-facing kind assigned at install time. For subject_backend
    /// plugins this is the kind the SubjectRouter dispatches against
    /// (e.g. `task`, `task-2`, `archive`); for provider plugins it is the
    /// `provider_tool` users invoke (e.g. `claude`, `claude-2`). When
    /// `None` the entry is from a pre-v0.5.7 lockfile and the consumer
    /// treats it as `installed_kind == native_kind`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub installed_kind: Option<String>,
    /// Plugin-native kind declared in the binary's manifest. The
    /// SubjectRouter translates `<installed_kind>/<verb>` to
    /// `<native_kind>/<verb>` at the wire boundary so plugins stay
    /// non-parametric. When `None` the entry is from a pre-v0.5.7
    /// lockfile and the consumer treats it as `installed_kind == native_kind`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_kind: Option<String>,
}

impl LockEntry {
    /// Resolve the effective installed kind, falling back to `native_kind`
    /// for pre-v0.5.7 entries where the rename surface did not yet exist.
    pub fn effective_installed_kind(&self) -> Option<&str> {
        self.installed_kind.as_deref().or(self.native_kind.as_deref())
    }

    /// Resolve the effective native kind, falling back to `installed_kind`
    /// for pre-v0.5.7 entries where the rename surface did not yet exist.
    pub fn effective_native_kind(&self) -> Option<&str> {
        self.native_kind.as_deref().or(self.installed_kind.as_deref())
    }
}

/// The full on-disk lockfile shape.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginLockfile {
    /// Schema version of the on-disk format. Readers MUST refuse to parse a
    /// lockfile whose major version differs from
    /// [`LOCKFILE_SCHEMA_VERSION`].
    #[serde(default = "default_schema_version")]
    pub schema_version: String,
    /// RFC 3339 timestamp of the most recent write.
    #[serde(default)]
    pub generated_at: String,
    /// Lock entries, in insertion order.
    #[serde(default)]
    pub plugins: Vec<LockEntry>,
    /// Resolved on-disk path the lockfile was loaded from / will be saved
    /// to. Skipped from the serialized form.
    #[serde(skip)]
    path: PathBuf,
    /// Names explicitly removed from this in-memory snapshot since it was
    /// loaded. Consulted by [`Self::save`]'s locked merge so an entry a
    /// concurrent process installed after this snapshot was loaded
    /// survives the save, while an explicit [`Self::remove`] still deletes
    /// its entry. Skipped from the serialized form.
    #[serde(skip)]
    removed: BTreeSet<String>,
}

fn default_schema_version() -> String {
    LOCKFILE_SCHEMA_VERSION.to_string()
}

/// Outcome of [`PluginLockfile::verify_entry`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LockVerifyResult {
    /// No entry for this name in the lockfile. Caller decides whether that's
    /// fine (fresh install) or an error (upgrade without prior install).
    Missing,
    /// The stored hash matches the supplied artifact.
    Match,
    /// The stored hash disagrees with the supplied artifact. The caller MUST
    /// refuse the upgrade unless `--force` was passed.
    Mismatch { expected: String, actual: String },
}

impl PluginLockfile {
    /// Resolve the canonical lockfile path.
    ///
    /// When `project_root` is supplied and either the project-local
    /// `<project_root>/.animus/plugins.lock` already exists OR the
    /// `<project_root>/.animus/` directory exists (i.e. this project has
    /// opted into Animus), prefer the project-local path. Otherwise fall
    /// back to `~/.animus/plugins.lock`.
    pub fn default_path(project_root: Option<&Path>) -> PathBuf {
        if let Some(root) = project_root {
            let local = root.join(".animus").join("plugins.lock");
            if local.exists() {
                return local;
            }
            let parent = root.join(".animus");
            if parent.exists() {
                return local;
            }
        }
        global_lockfile_path()
    }

    /// Load the lockfile at `path`, creating an empty in-memory lockfile
    /// when the file does not exist. Schema-version mismatches are reported
    /// as errors so a future v2 file can't be silently truncated by a
    /// v1-only writer.
    pub fn load_or_empty(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::empty_at(path));
        }
        let raw = fs::read_to_string(path).with_context(|| format!("failed to read lockfile {}", path.display()))?;
        let mut parsed: Self =
            toml::from_str(&raw).with_context(|| format!("failed to parse lockfile {}", path.display()))?;
        if !schema_compatible(&parsed.schema_version) {
            anyhow::bail!(
                "lockfile {} has incompatible schema_version '{}' (this build supports '{}')",
                path.display(),
                parsed.schema_version,
                LOCKFILE_SCHEMA_VERSION
            );
        }
        parsed.path = path.to_path_buf();
        Ok(parsed)
    }

    /// Convenience: load (or initialize) the lockfile at
    /// [`Self::default_path`].
    pub fn load_default(project_root: Option<&Path>) -> Result<Self> {
        Self::load_or_empty(&Self::default_path(project_root))
    }

    /// In-memory empty lockfile bound to `path`.
    pub fn empty_at(path: &Path) -> Self {
        Self {
            schema_version: LOCKFILE_SCHEMA_VERSION.to_string(),
            generated_at: Utc::now().to_rfc3339(),
            plugins: Vec::new(),
            path: path.to_path_buf(),
            removed: BTreeSet::new(),
        }
    }

    /// Resolved on-disk path the lockfile was loaded from.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Look up an entry by name.
    pub fn find(&self, name: &str) -> Option<&LockEntry> {
        self.plugins.iter().find(|e| e.name == name)
    }

    /// Insert or replace an entry. Returns the prior entry when one existed.
    pub fn upsert(&mut self, entry: LockEntry) -> Option<LockEntry> {
        self.removed.remove(&entry.name);
        if let Some(idx) = self.plugins.iter().position(|e| e.name == entry.name) {
            let prior = std::mem::replace(&mut self.plugins[idx], entry);
            return Some(prior);
        }
        self.plugins.push(entry);
        None
    }

    /// Remove an entry by name. Returns the removed entry, or `None` if no
    /// such entry existed. The name is recorded as removed either way so a
    /// subsequent [`Self::save`] drops it even when a concurrent process
    /// re-wrote the on-disk file after this snapshot was loaded.
    pub fn remove(&mut self, name: &str) -> Option<LockEntry> {
        self.removed.insert(name.to_string());
        let idx = self.plugins.iter().position(|e| e.name == name)?;
        Some(self.plugins.remove(idx))
    }

    /// Re-check whether the supplied `artifact_sha256` matches the recorded
    /// hash for `name`. Returns [`LockVerifyResult::Missing`] when no entry
    /// is present.
    pub fn verify_entry(&self, name: &str, artifact_sha256: &str) -> LockVerifyResult {
        let Some(entry) = self.find(name) else {
            return LockVerifyResult::Missing;
        };
        if entry.artifact_sha256.eq_ignore_ascii_case(artifact_sha256) {
            LockVerifyResult::Match
        } else {
            LockVerifyResult::Mismatch { expected: entry.artifact_sha256.clone(), actual: artifact_sha256.to_string() }
        }
    }

    /// Hash the file at `path` against the entry recorded for `name`.
    /// Combines [`sha256_of_file`] + [`Self::verify_entry`] into one call so
    /// CLI subcommands can implement `plugin lock verify` directly.
    pub fn verify_installed(&self, name: &str, installed_path: &Path) -> Result<LockVerifyResult> {
        let actual = sha256_of_file(installed_path)?;
        Ok(self.verify_entry(name, &actual))
    }

    /// Persist the lockfile to its bound path. Creates parent dirs as
    /// needed and writes atomically (write to a unique pid+uuid temp file,
    /// then rename — mirrors `manifest_cache::insert`).
    ///
    /// Concurrent writers (e.g. two `animus plugin install` runs) are
    /// serialized behind an exclusive [`fs2`] lock on a `<path>.lock`
    /// sidecar held for the full read-merge-write. Under that lock the
    /// on-disk file is re-read and entries a concurrent process added
    /// since this snapshot was loaded are merged back in, so neither run
    /// erases the other's entry; names passed to [`Self::remove`] stay
    /// removed.
    pub fn save(&mut self) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create lockfile parent dir {}", parent.display()))?;
        }
        let _guard = acquire_save_lock(&self.path)?;

        // Merge-on-save: keep on-disk entries this snapshot never saw. A
        // file that no longer parses is treated as absent so the
        // documented `--force-rewrite-lockfile` remediation (save an empty
        // snapshot over corrupt bytes) keeps working.
        if let Ok(on_disk) = Self::load_or_empty(&self.path) {
            for entry in on_disk.plugins {
                if self.removed.contains(&entry.name) || self.find(&entry.name).is_some() {
                    continue;
                }
                self.plugins.push(entry);
            }
        }

        self.generated_at = Utc::now().to_rfc3339();
        let serialized = toml::to_string_pretty(self).context("failed to serialize plugin lockfile")?;
        let file_name = self.path.file_name().and_then(|value| value.to_str()).unwrap_or("plugins.lock");
        let tmp = self.path.with_file_name(format!(
            "{file_name}.tmp-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        {
            let mut handle = fs::File::create(&tmp).with_context(|| format!("failed to open {}", tmp.display()))?;
            handle.write_all(serialized.as_bytes()).with_context(|| format!("failed to write {}", tmp.display()))?;
            handle.sync_all().ok();
        }
        if let Err(err) = fs::rename(&tmp, &self.path) {
            let _ = fs::remove_file(&tmp);
            return Err(anyhow::Error::new(err).context(format!(
                "failed to install lockfile {} (from {})",
                self.path.display(),
                tmp.display()
            )));
        }
        self.removed.clear();
        Ok(())
    }
}

/// Acquire an exclusive cross-process lock on the `<path>.lock` sidecar
/// guarding lockfile writes. Hold the returned `File` for the full
/// read-merge-write inside [`PluginLockfile::save`]; the lock releases on
/// drop. Two concurrent `animus plugin install` runs otherwise interleave
/// their read-modify-write cycles and one silently erases the other's
/// entry.
fn acquire_save_lock(path: &Path) -> Result<fs::File> {
    let file_name = path.file_name().and_then(|value| value.to_str()).unwrap_or("plugins.lock");
    let lock_path = path.with_file_name(format!("{file_name}.lock"));
    let file = fs::File::create(&lock_path)
        .with_context(|| format!("failed to create lockfile write guard at {}", lock_path.display()))?;
    file.lock_exclusive().with_context(|| format!("failed to acquire exclusive lock on {}", lock_path.display()))?;
    Ok(file)
}

fn schema_compatible(version: &str) -> bool {
    let major = |s: &str| s.split('.').next().unwrap_or("").to_string();
    major(version) == major(LOCKFILE_SCHEMA_VERSION)
}

/// Returns `~/.animus/plugins.lock`. Falls back to `.animus/plugins.lock`
/// in the cwd when `$HOME` is unset.
pub fn global_lockfile_path() -> PathBuf {
    if let Some(home) = dirs::home_dir() {
        return home.join(".animus").join("plugins.lock");
    }
    PathBuf::from(".animus").join("plugins.lock")
}

/// Compute the lowercase hex sha256 of a file on disk.
pub fn sha256_of_file(path: &Path) -> Result<String> {
    let bytes = fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    Ok(format!("{:x}", hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn now_str() -> String {
        Utc::now().to_rfc3339()
    }

    #[test]
    fn load_or_empty_returns_empty_when_missing() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("plugins.lock");
        let lock = PluginLockfile::load_or_empty(&path).unwrap();
        assert!(lock.plugins.is_empty());
        assert_eq!(lock.schema_version, LOCKFILE_SCHEMA_VERSION);
        assert_eq!(lock.path(), &path);
    }

    #[test]
    fn upsert_then_save_then_reload_roundtrips() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("plugins.lock");
        let mut lock = PluginLockfile::empty_at(&path);
        lock.upsert(LockEntry {
            name: "launchapp-dev/animus-provider-claude".to_string(),
            version: "v0.2.2".to_string(),
            artifact_sha256: "a".repeat(64),
            signature_bundle_sha256: Some("b".repeat(64)),
            installed_at: now_str(),
            installed_kind: None,
            native_kind: None,
        });
        lock.save().unwrap();

        let reloaded = PluginLockfile::load_or_empty(&path).unwrap();
        assert_eq!(reloaded.plugins.len(), 1);
        let entry = &reloaded.plugins[0];
        assert_eq!(entry.name, "launchapp-dev/animus-provider-claude");
        assert_eq!(entry.version, "v0.2.2");
        assert_eq!(entry.artifact_sha256, "a".repeat(64));
        assert_eq!(entry.signature_bundle_sha256.as_deref(), Some("b".repeat(64).as_str()));
    }

    #[test]
    fn upsert_replaces_existing_entry() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("plugins.lock");
        let mut lock = PluginLockfile::empty_at(&path);
        lock.upsert(LockEntry {
            name: "x".into(),
            version: "v1".into(),
            artifact_sha256: "1".repeat(64),
            signature_bundle_sha256: None,
            installed_at: now_str(),
            installed_kind: None,
            native_kind: None,
        });
        let prior = lock.upsert(LockEntry {
            name: "x".into(),
            version: "v2".into(),
            artifact_sha256: "2".repeat(64),
            signature_bundle_sha256: None,
            installed_at: now_str(),
            installed_kind: None,
            native_kind: None,
        });
        assert!(prior.is_some());
        assert_eq!(lock.plugins.len(), 1);
        assert_eq!(lock.plugins[0].version, "v2");
    }

    #[test]
    fn verify_entry_reports_match_missing_and_mismatch() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("plugins.lock");
        let mut lock = PluginLockfile::empty_at(&path);
        let sha = "f".repeat(64);
        lock.upsert(LockEntry {
            name: "x".into(),
            version: "v1".into(),
            artifact_sha256: sha.clone(),
            signature_bundle_sha256: None,
            installed_at: now_str(),
            installed_kind: None,
            native_kind: None,
        });
        assert_eq!(lock.verify_entry("x", &sha), LockVerifyResult::Match);
        assert_eq!(lock.verify_entry("x", &sha.to_ascii_uppercase()), LockVerifyResult::Match);
        match lock.verify_entry("x", &"0".repeat(64)) {
            LockVerifyResult::Mismatch { expected, actual } => {
                assert_eq!(expected, sha);
                assert_eq!(actual, "0".repeat(64));
            }
            other => panic!("expected Mismatch, got {other:?}"),
        }
        assert_eq!(lock.verify_entry("missing", &sha), LockVerifyResult::Missing);
    }

    #[test]
    fn verify_installed_detects_tampered_binary() {
        let dir = tempdir().unwrap();
        let bin_path = dir.path().join("plugin-bin");
        fs::write(&bin_path, b"original-binary-content").unwrap();
        let original_sha = sha256_of_file(&bin_path).unwrap();

        let lock_path = dir.path().join("plugins.lock");
        let mut lock = PluginLockfile::empty_at(&lock_path);
        lock.upsert(LockEntry {
            name: "myplugin".into(),
            version: "v1".into(),
            artifact_sha256: original_sha.clone(),
            signature_bundle_sha256: None,
            installed_at: now_str(),
            installed_kind: None,
            native_kind: None,
        });

        // No tamper — should match.
        assert_eq!(lock.verify_installed("myplugin", &bin_path).unwrap(), LockVerifyResult::Match);

        // Mutate the binary on disk and re-check.
        fs::write(&bin_path, b"tampered-binary-content").unwrap();
        match lock.verify_installed("myplugin", &bin_path).unwrap() {
            LockVerifyResult::Mismatch { expected, actual } => {
                assert_eq!(expected, original_sha);
                assert_ne!(actual, original_sha);
            }
            other => panic!("expected Mismatch after tamper, got {other:?}"),
        }
    }

    #[test]
    fn remove_returns_entry_and_drops_it() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("plugins.lock");
        let mut lock = PluginLockfile::empty_at(&path);
        lock.upsert(LockEntry {
            name: "drop-me".into(),
            version: String::new(),
            artifact_sha256: "9".repeat(64),
            signature_bundle_sha256: None,
            installed_at: now_str(),
            installed_kind: None,
            native_kind: None,
        });
        let removed = lock.remove("drop-me").expect("expected entry");
        assert_eq!(removed.name, "drop-me");
        assert!(lock.find("drop-me").is_none());
        assert!(lock.remove("drop-me").is_none());
    }

    fn entry(name: &str, sha_char: char) -> LockEntry {
        LockEntry {
            name: name.to_string(),
            version: "v1".to_string(),
            artifact_sha256: sha_char.to_string().repeat(64),
            signature_bundle_sha256: None,
            installed_at: now_str(),
            installed_kind: None,
            native_kind: None,
        }
    }

    /// Two concurrent read-modify-write cycles (two `animus plugin install`
    /// runs) must both land: the sidecar lock + merge-on-save keeps the file
    /// valid TOML and preserves both entries regardless of interleaving.
    #[test]
    fn concurrent_saves_preserve_both_entries() {
        for _ in 0..10 {
            let dir = tempdir().unwrap();
            let path = dir.path().join("plugins.lock");

            let spawn_installer = |name: &'static str, sha_char: char| {
                let path = path.clone();
                std::thread::spawn(move || {
                    let mut lock = PluginLockfile::load_or_empty(&path).expect("load");
                    lock.upsert(entry(name, sha_char));
                    lock.save().expect("save");
                })
            };
            let a = spawn_installer("plugin-a", 'a');
            let b = spawn_installer("plugin-b", 'b');
            a.join().expect("thread a");
            b.join().expect("thread b");

            let reloaded = PluginLockfile::load_or_empty(&path).expect("concurrent saves must yield valid TOML");
            let names: Vec<&str> = reloaded.plugins.iter().map(|e| e.name.as_str()).collect();
            assert!(names.contains(&"plugin-a"), "entry from thread a must survive; got {names:?}");
            assert!(names.contains(&"plugin-b"), "entry from thread b must survive; got {names:?}");
        }
    }

    /// The merge-on-save must not resurrect an explicitly removed entry,
    /// while still preserving an entry a concurrent process added after this
    /// snapshot was loaded.
    #[test]
    fn save_merges_concurrent_entries_but_honors_removals() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("plugins.lock");

        let mut seed = PluginLockfile::empty_at(&path);
        seed.upsert(entry("keep-me", 'a'));
        seed.upsert(entry("drop-me", 'b'));
        seed.save().unwrap();

        // Snapshot loaded before the concurrent write below.
        let mut snapshot = PluginLockfile::load_or_empty(&path).unwrap();

        // Concurrent process installs another plugin.
        let mut concurrent = PluginLockfile::load_or_empty(&path).unwrap();
        concurrent.upsert(entry("added-concurrently", 'c'));
        concurrent.save().unwrap();

        snapshot.remove("drop-me");
        snapshot.save().unwrap();

        let reloaded = PluginLockfile::load_or_empty(&path).unwrap();
        let names: Vec<&str> = reloaded.plugins.iter().map(|e| e.name.as_str()).collect();
        assert!(names.contains(&"keep-me"), "untouched entry must survive; got {names:?}");
        assert!(names.contains(&"added-concurrently"), "concurrent entry must survive; got {names:?}");
        assert!(!names.contains(&"drop-me"), "removed entry must stay removed; got {names:?}");
    }

    #[test]
    fn schema_mismatch_is_rejected() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("plugins.lock");
        fs::write(&path, "schema_version = \"99.0\"\ngenerated_at = \"2026-05-26T00:00:00Z\"\nplugins = []\n").unwrap();
        let err = PluginLockfile::load_or_empty(&path).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("schema_version"));
    }

    #[test]
    fn default_path_prefers_project_local_when_animus_dir_exists() {
        let dir = tempdir().unwrap();
        let animus = dir.path().join(".animus");
        fs::create_dir_all(&animus).unwrap();
        let resolved = PluginLockfile::default_path(Some(dir.path()));
        assert_eq!(resolved, animus.join("plugins.lock"));
    }

    #[test]
    fn default_path_falls_back_to_global_when_no_project_root() {
        let resolved = PluginLockfile::default_path(None);
        assert_eq!(resolved, global_lockfile_path());
    }

    #[test]
    fn legacy_lockfile_without_kind_fields_parses_with_defaults() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("plugins.lock");
        let legacy = "schema_version = \"1.0\"\ngenerated_at = \"2026-05-26T00:00:00Z\"\n\n[[plugins]]\nname = \"launchapp-dev/animus-subject-default\"\nversion = \"v0.1.1\"\nartifact_sha256 = \"a\"\ninstalled_at = \"2026-05-26T00:00:00Z\"\n";
        fs::write(&path, legacy).unwrap();
        let lock = PluginLockfile::load_or_empty(&path).expect("legacy lockfile parses");
        assert_eq!(lock.plugins.len(), 1);
        let entry = &lock.plugins[0];
        assert_eq!(entry.installed_kind, None);
        assert_eq!(entry.native_kind, None);
        assert_eq!(entry.effective_installed_kind(), None);
        assert_eq!(entry.effective_native_kind(), None);
    }

    #[test]
    fn lockfile_roundtrips_installed_and_native_kind() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("plugins.lock");
        let mut lock = PluginLockfile::empty_at(&path);
        lock.upsert(LockEntry {
            name: "launchapp-dev/animus-subject-default-archive".to_string(),
            version: "v0.1.0".to_string(),
            artifact_sha256: "c".repeat(64),
            signature_bundle_sha256: None,
            installed_at: now_str(),
            installed_kind: Some("archive".to_string()),
            native_kind: Some("task".to_string()),
        });
        lock.save().unwrap();
        let reloaded = PluginLockfile::load_or_empty(&path).unwrap();
        let entry = &reloaded.plugins[0];
        assert_eq!(entry.installed_kind.as_deref(), Some("archive"));
        assert_eq!(entry.native_kind.as_deref(), Some("task"));
        assert_eq!(entry.effective_installed_kind(), Some("archive"));
        assert_eq!(entry.effective_native_kind(), Some("task"));
    }
}
