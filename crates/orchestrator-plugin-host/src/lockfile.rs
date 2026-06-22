//! Plugin lockfile (`.animus/plugins.lock`).
//!
//! `plugins.lock` IS the animus plugin lockfile: it records, per plugin
//! install, the source provenance plus a PER-TARGET integrity claim so a
//! later upgrade can refuse to silently overwrite a binary whose hash no
//! longer matches what the operator originally approved, and so
//! `animus plugin install --locked` can reproduce the exact pinned set on a
//! fresh machine — including a DIFFERENT platform than the one the lock was
//! generated on. The lockfile is the user-visible audit + rollback anchor for
//! plugin installs; the install pipeline writes it on success and removes the
//! entry on uninstall.
//!
//! ## Platform-aware integrity (schema 2.0)
//!
//! Each entry's integrity claim is keyed by target triple in
//! [`LockEntry::targets`]. The portable claim is
//! [`TargetIntegrity::archive_sha256`] — the sha256 of the release TARBALL
//! (`ao-<ver>-<target>.tar.gz`), exactly what a GitHub release's
//! `SHA256SUMS.txt` lists. A release install fetches `SHA256SUMS.txt` and
//! records the tarball sha for EVERY published platform, so a lock generated
//! on macOS carries the linux tarball sha too and `--locked` can verify it on
//! a linux container BEFORE extracting/executing anything.
//!
//! [`TargetIntegrity::installed_binary_sha256`] is a SECONDARY, host-only
//! field: the sha256 of the extracted binary on the machine that performed
//! this install. It is only meaningful for the install platform, so it is
//! recorded only for the current target and used solely for fast on-disk
//! tamper detection by `plugin lock verify` and the daemon lock-drift probe.
//! Cross-platform verification (`--locked`) always uses `archive_sha256`.
//!
//! Tool-managed: do NOT hand-edit. Use `animus plugin install` /
//! `plugin update` / `plugin uninstall` to mutate it; gitignore keeps the
//! installed binaries out of version control but the lockfile itself is meant
//! to be committed so the pinned set travels with the repo.
//!
//! Format (TOML, matches the rest of the manifest surface):
//!
//! ```toml
//! schema_version = "2.0"
//! generated_at = "2026-05-26T..."
//!
//! [[plugins]]
//! name = "launchapp-dev/animus-provider-claude"
//! version = "v0.2.2"
//! installed_at = "2026-05-26T..."
//! source_repo = "launchapp-dev/animus-provider-claude"
//! resolved_commit = "0123abc...40-hex..."
//!
//! [plugins.targets.x86_64-unknown-linux-gnu]
//! archive_sha256 = "abc123..."
//! signature_bundle_sha256 = "def456..."
//!
//! [plugins.targets.aarch64-apple-darwin]
//! archive_sha256 = "789fed..."
//! installed_binary_sha256 = "0011..."
//! ```
//!
//! `source_repo` records where the plugin came from — an `owner/repo` slug for
//! release installs, the URL for `--url` installs, or `path:<...>` for
//! `--path` installs. `resolved_commit` is the exact 40-hex commit sha when
//! the GitHub release resolved to one (releases tagged at a branch have no sha
//! and leave it unset). Both are optional and back-compat.
//!
//! ## 1.0 → 2.0 migration
//!
//! A 1.0 entry's flat `artifact_sha256` was the installed BINARY hash on the
//! generating machine, NOT a tarball sha — so it cannot be reused as a 2.0
//! `archive_sha256`. A 1.0 entry therefore migrates to a 2.0 entry with an
//! EMPTY `targets` map: it parses (no hard-fail), but carries no usable
//! integrity claim until it is re-`install`ed. Consumers treat such an entry
//! as "no recorded target" and `--locked` reports it as a missing target with
//! an actionable reinstall hint. On the next write the entry serializes as
//! 2.0.
//!
//! Resolution order (`PluginLockfile::default_path`):
//! 1. Project-local `<project_root>/.animus/plugins.lock` when `project_root`
//!    is supplied AND the file exists OR the parent `.animus/` exists.
//! 2. Global `~/.animus/plugins.lock` fallback.

use std::collections::{BTreeMap, BTreeSet};
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
pub const LOCKFILE_SCHEMA_VERSION: &str = "2.0";

/// Per-target integrity claim for a single platform of a lock entry, keyed in
/// [`LockEntry::targets`] by target triple (e.g. `x86_64-unknown-linux-gnu`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TargetIntegrity {
    /// Lowercase hex sha256 of the release TARBALL for this target
    /// (`<plugin>-<target>.tar.gz`) — exactly what the release
    /// `SHA256SUMS.txt` lists. This is the PORTABLE claim: it can be verified
    /// on any machine before extracting, so a lock generated on one platform
    /// drives a verified `--locked` install on another. For `--path`/`--url`
    /// sources (no tarball / no SHA256SUMS) this is the sha of the fetched
    /// artifact itself; such entries carry only the install platform's target.
    pub archive_sha256: String,
    /// Lowercase hex sha256 of the cosign signature bundle published alongside
    /// this target's tarball, if any. `None` when no bundle was present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature_bundle_sha256: Option<String>,
    /// Lowercase hex sha256 of the EXTRACTED binary as installed on disk.
    /// SECONDARY + host-only: only meaningful for the platform that performed
    /// the install, so it is populated solely for the current target and used
    /// only for fast on-disk tamper detection (`plugin lock verify`, daemon
    /// lock-drift probe). Cross-platform verification uses
    /// [`Self::archive_sha256`]. `None` for targets recorded from a foreign
    /// platform's SHA256SUMS (where no binary was extracted locally).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub installed_binary_sha256: Option<String>,
}

/// One install entry inside the lockfile. `(name, version, targets)` is the
/// integrity claim the install pipeline checks on every subsequent upgrade and
/// reproduces under `--locked`.
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
    /// Per-target integrity claims, keyed by target triple. Populated for
    /// EVERY published platform on a release install (from `SHA256SUMS.txt`),
    /// or only the install platform's target for `--path`/`--url` sources. An
    /// EMPTY map marks an entry migrated from a 1.0 lockfile (whose flat
    /// binary hash is unusable as a tarball sha): such an entry needs a
    /// re-`install` to gain a usable claim.
    #[serde(default)]
    pub targets: BTreeMap<String, TargetIntegrity>,
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
    /// Where this plugin was installed from. For release-source installs this
    /// is the `owner/repo` slug (e.g. `launchapp-dev/animus-provider-claude`)
    /// so `animus plugin install --locked` can reconstruct the pin. For
    /// `--url` installs it is the URL; for `--path` installs it is
    /// `path:<absolute-or-supplied-path>`. `None` for pre-source-provenance
    /// lockfile rows. The resolved tag for release installs lives in
    /// [`Self::version`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_repo: Option<String>,
    /// Exact commit sha the install resolved to, when the GitHub release
    /// pointed at a 40-char lowercase-hex sha (`target_commitish`). Releases
    /// tagged at a branch return a branch name, not a sha, and leave this
    /// `None`. `None` for non-release installs and pre-source-provenance rows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_commit: Option<String>,
    /// Legacy 1.0 flat binary hash, captured only to detect a 1.0 row on read.
    /// Never re-serialized (2.0 writes `targets` instead). A 1.0 row's
    /// `artifact_sha256` was the install machine's BINARY hash, which is not a
    /// tarball sha and cannot seed a portable `targets` claim, so it is read
    /// then dropped — the migrated entry has an empty `targets` map until a
    /// re-`install`. See module docs. Always `None` for in-memory-constructed
    /// entries; consumers should never set it.
    #[serde(default, skip_serializing, rename = "artifact_sha256")]
    pub legacy_artifact_sha256: Option<String>,
    /// Legacy 1.0 flat signature-bundle hash; captured only for read-back
    /// (never re-serialized). Dropped on migration for the same reason as
    /// [`Self::legacy_artifact_sha256`].
    #[serde(default, skip_serializing, rename = "signature_bundle_sha256")]
    pub legacy_signature_bundle_sha256: Option<String>,
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

    /// The integrity claim for `target` (target triple), if recorded.
    pub fn target(&self, target: &str) -> Option<&TargetIntegrity> {
        self.targets.get(target)
    }

    /// `true` when this entry carries no usable integrity claim — i.e. it was
    /// migrated from a 1.0 lockfile and needs a re-`install` before
    /// `--locked` / `lock verify` can validate it.
    pub fn is_legacy_unverifiable(&self) -> bool {
        self.targets.is_empty()
    }

    /// Sorted list of target triples this entry has a recorded claim for.
    pub fn target_triples(&self) -> Vec<&str> {
        self.targets.keys().map(String::as_str).collect()
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
    /// An entry exists but has no recorded integrity claim for the requested
    /// target triple. Distinct from [`Self::Missing`]: the plugin IS pinned,
    /// but the lock was generated without this platform (or migrated from a
    /// 1.0 lockfile). `--locked` must hard-fail with a "regenerate / reinstall
    /// on this platform" hint rather than verify against the wrong hash.
    MissingTarget { target: String },
    /// The stored hash matches the supplied artifact.
    Match,
    /// The stored hash disagrees with the supplied artifact. The caller MUST
    /// refuse the upgrade unless `--force` was passed.
    Mismatch { expected: String, actual: String },
}

/// The Rust target triple this binary was built for, used to key per-target
/// integrity claims. Built from `cfg!` so it matches the published asset
/// naming (`<plugin>-<triple>.tar.gz`). Returns `None` on a target with no
/// known triple mapping (mirrors the empty asset-selector case in the CLI).
///
// TODO(codex-p2): keyed on `target_os`/`target_arch` only, so a `*-linux-musl`
// build reports the `-gnu` triple (and a Windows `-gnu` build reports `-msvc`).
// Animus releases are gnu/msvc, so this is correct for them; a musl-built CLI
// would record/look up the wrong target. Distinguishing needs a `target_env`
// cfg arm; deferred until a musl build target is actually shipped.
pub fn current_target_triple() -> Option<&'static str> {
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        Some("aarch64-apple-darwin")
    }
    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    {
        Some("x86_64-apple-darwin")
    }
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        Some("x86_64-unknown-linux-gnu")
    }
    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    {
        Some("aarch64-unknown-linux-gnu")
    }
    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    {
        Some("x86_64-pc-windows-msvc")
    }
    #[cfg(not(any(
        all(target_os = "macos", target_arch = "aarch64"),
        all(target_os = "macos", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "aarch64"),
        all(target_os = "windows", target_arch = "x86_64"),
    )))]
    {
        None
    }
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
        if !schema_readable(&parsed.schema_version) {
            anyhow::bail!(
                "lockfile {} has incompatible schema_version '{}' (this build reads 1.x and {})",
                path.display(),
                parsed.schema_version,
                LOCKFILE_SCHEMA_VERSION
            );
        }
        // 1.0 → 2.0 migration: a 1.0 row's flat `artifact_sha256` was the
        // install machine's BINARY hash (not a tarball sha), so it cannot seed
        // a portable `targets` claim. Drop the legacy fields and leave
        // `targets` empty — the entry parses but is `is_legacy_unverifiable()`
        // until a re-`install`. The next write emits 2.0.
        if major_of(&parsed.schema_version) == "1" {
            let migrated_legacy = parsed.plugins.iter().filter(|e| e.legacy_artifact_sha256.is_some()).count();
            if migrated_legacy > 0 {
                tracing::warn!(
                    path = %path.display(),
                    count = migrated_legacy,
                    "migrating 1.0 plugin lockfile to 2.0: legacy entries carry no portable (per-target) integrity claim \
                     and must be reinstalled before `--locked` / `lock verify` can validate them"
                );
            }
            for entry in &mut parsed.plugins {
                entry.legacy_artifact_sha256 = None;
                entry.legacy_signature_bundle_sha256 = None;
            }
            parsed.schema_version = LOCKFILE_SCHEMA_VERSION.to_string();
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

    /// Verify the supplied EXTRACTED-BINARY sha256 for `name` against the
    /// recorded `installed_binary_sha256` for the current build target.
    ///
    /// Returns [`LockVerifyResult::Missing`] when no entry exists,
    /// [`LockVerifyResult::MissingTarget`] when the entry has no claim for the
    /// current target OR the current target's claim records no installed-binary
    /// hash (e.g. the target was populated from a foreign platform's
    /// SHA256SUMS, or the entry was migrated from a 1.0 lockfile). This is the
    /// HOST-ONLY tamper check used by `plugin lock verify` and the daemon
    /// drift probe; cross-platform reproducibility uses
    /// [`Self::verify_archive`].
    pub fn verify_entry(&self, name: &str, installed_binary_sha256: &str) -> LockVerifyResult {
        let Some(triple) = current_target_triple() else {
            return LockVerifyResult::MissingTarget { target: "<unknown>".to_string() };
        };
        self.verify_entry_for_target(name, triple, installed_binary_sha256)
    }

    /// Target-explicit form of [`Self::verify_entry`] — verifies the supplied
    /// extracted-binary sha against `targets[target].installed_binary_sha256`.
    pub fn verify_entry_for_target(&self, name: &str, target: &str, installed_binary_sha256: &str) -> LockVerifyResult {
        let Some(entry) = self.find(name) else {
            return LockVerifyResult::Missing;
        };
        let Some(integrity) = entry.target(target) else {
            return LockVerifyResult::MissingTarget { target: target.to_string() };
        };
        let Some(expected) = integrity.installed_binary_sha256.as_deref() else {
            return LockVerifyResult::MissingTarget { target: target.to_string() };
        };
        if expected.eq_ignore_ascii_case(installed_binary_sha256) {
            LockVerifyResult::Match
        } else {
            LockVerifyResult::Mismatch { expected: expected.to_string(), actual: installed_binary_sha256.to_string() }
        }
    }

    /// Verify a downloaded release TARBALL's sha256 for `name` against the
    /// recorded `archive_sha256` for `target`. This is the PORTABLE,
    /// cross-platform claim `animus plugin install --locked` checks before
    /// extracting/executing anything.
    pub fn verify_archive(&self, name: &str, target: &str, archive_sha256: &str) -> LockVerifyResult {
        let Some(entry) = self.find(name) else {
            return LockVerifyResult::Missing;
        };
        let Some(integrity) = entry.target(target) else {
            return LockVerifyResult::MissingTarget { target: target.to_string() };
        };
        if integrity.archive_sha256.eq_ignore_ascii_case(archive_sha256) {
            LockVerifyResult::Match
        } else {
            LockVerifyResult::Mismatch {
                expected: integrity.archive_sha256.clone(),
                actual: archive_sha256.to_string(),
            }
        }
    }

    /// Hash the file at `path` against the installed-binary claim recorded for
    /// `name` on the current target. Combines [`sha256_of_file`] +
    /// [`Self::verify_entry`] into one call so CLI subcommands can implement
    /// `plugin lock verify` directly.
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

fn major_of(version: &str) -> &str {
    version.split('.').next().unwrap_or("")
}

/// A lockfile is readable when its major version is the current one (2) — the
/// native shape — OR `1` (the legacy shape, migrated in-memory on load). A
/// future major (3+) is refused so a newer file is not silently truncated by
/// this writer.
fn schema_readable(version: &str) -> bool {
    let major = major_of(version);
    major == major_of(LOCKFILE_SCHEMA_VERSION) || major == "1"
}

/// Returns the project-local lockfile path
/// (`<project_root>/.animus/plugins.lock`). Used by `animus plugin install
/// --project` and the both-roots `plugin lock verify` sweep. Unlike
/// [`PluginLockfile::default_path`] this never falls back to the global
/// lockfile — the caller has explicitly chosen the project scope.
pub fn project_lockfile_path(project_root: &Path) -> PathBuf {
    project_root.join(".animus").join("plugins.lock")
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

    /// Build a 2.0 entry with a single target claim for the CURRENT build
    /// target whose `archive_sha256` and `installed_binary_sha256` are both the
    /// repeated `sha_char`. Mirrors how an install on this platform records its
    /// own target.
    fn entry_with_local_target(name: &str, version: &str, sha_char: char) -> LockEntry {
        let triple = current_target_triple().expect("test target has a known triple");
        let sha = sha_char.to_string().repeat(64);
        let mut targets = BTreeMap::new();
        targets.insert(
            triple.to_string(),
            TargetIntegrity {
                archive_sha256: sha.clone(),
                signature_bundle_sha256: None,
                installed_binary_sha256: Some(sha),
            },
        );
        LockEntry {
            name: name.to_string(),
            version: version.to_string(),
            targets,
            installed_at: now_str(),
            installed_kind: None,
            native_kind: None,
            source_repo: None,
            resolved_commit: None,
            legacy_artifact_sha256: None,
            legacy_signature_bundle_sha256: None,
        }
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
    fn upsert_then_save_then_reload_roundtrips_targets() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("plugins.lock");
        let mut lock = PluginLockfile::empty_at(&path);
        let mut targets = BTreeMap::new();
        targets.insert(
            "x86_64-unknown-linux-gnu".to_string(),
            TargetIntegrity {
                archive_sha256: "a".repeat(64),
                signature_bundle_sha256: Some("b".repeat(64)),
                installed_binary_sha256: None,
            },
        );
        targets.insert(
            "aarch64-apple-darwin".to_string(),
            TargetIntegrity {
                archive_sha256: "c".repeat(64),
                signature_bundle_sha256: None,
                installed_binary_sha256: Some("d".repeat(64)),
            },
        );
        lock.upsert(LockEntry {
            name: "launchapp-dev/animus-provider-claude".to_string(),
            version: "v0.2.2".to_string(),
            targets,
            installed_at: now_str(),
            installed_kind: None,
            native_kind: None,
            source_repo: None,
            resolved_commit: None,
            legacy_artifact_sha256: None,
            legacy_signature_bundle_sha256: None,
        });
        lock.save().unwrap();

        let reloaded = PluginLockfile::load_or_empty(&path).unwrap();
        assert_eq!(reloaded.plugins.len(), 1);
        let entry = &reloaded.plugins[0];
        assert_eq!(entry.name, "launchapp-dev/animus-provider-claude");
        assert_eq!(entry.version, "v0.2.2");
        assert_eq!(entry.target_triples(), vec!["aarch64-apple-darwin", "x86_64-unknown-linux-gnu"]);
        let linux = entry.target("x86_64-unknown-linux-gnu").unwrap();
        assert_eq!(linux.archive_sha256, "a".repeat(64));
        assert_eq!(linux.signature_bundle_sha256.as_deref(), Some("b".repeat(64).as_str()));
        let mac = entry.target("aarch64-apple-darwin").unwrap();
        assert_eq!(mac.archive_sha256, "c".repeat(64));
        assert_eq!(mac.installed_binary_sha256.as_deref(), Some("d".repeat(64).as_str()));
    }

    #[test]
    fn upsert_replaces_existing_entry() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("plugins.lock");
        let mut lock = PluginLockfile::empty_at(&path);
        lock.upsert(entry_with_local_target("x", "v1", '1'));
        let prior = lock.upsert(entry_with_local_target("x", "v2", '2'));
        assert!(prior.is_some());
        assert_eq!(lock.plugins.len(), 1);
        assert_eq!(lock.plugins[0].version, "v2");
    }

    #[test]
    fn verify_archive_reports_match_missing_mismatch_and_missing_target() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("plugins.lock");
        let mut lock = PluginLockfile::empty_at(&path);
        let sha = "f".repeat(64);
        let mut targets = BTreeMap::new();
        targets.insert(
            "x86_64-unknown-linux-gnu".to_string(),
            TargetIntegrity {
                archive_sha256: sha.clone(),
                signature_bundle_sha256: None,
                installed_binary_sha256: None,
            },
        );
        lock.upsert(LockEntry {
            name: "x".into(),
            version: "v1".into(),
            targets,
            installed_at: now_str(),
            installed_kind: None,
            native_kind: None,
            source_repo: None,
            resolved_commit: None,
            legacy_artifact_sha256: None,
            legacy_signature_bundle_sha256: None,
        });
        let tgt = "x86_64-unknown-linux-gnu";
        assert_eq!(lock.verify_archive("x", tgt, &sha), LockVerifyResult::Match);
        assert_eq!(lock.verify_archive("x", tgt, &sha.to_ascii_uppercase()), LockVerifyResult::Match);
        match lock.verify_archive("x", tgt, &"0".repeat(64)) {
            LockVerifyResult::Mismatch { expected, actual } => {
                assert_eq!(expected, sha);
                assert_eq!(actual, "0".repeat(64));
            }
            other => panic!("expected Mismatch, got {other:?}"),
        }
        // Entry exists but no claim for a foreign target → MissingTarget.
        match lock.verify_archive("x", "aarch64-apple-darwin", &sha) {
            LockVerifyResult::MissingTarget { target } => assert_eq!(target, "aarch64-apple-darwin"),
            other => panic!("expected MissingTarget, got {other:?}"),
        }
        assert_eq!(lock.verify_archive("missing", tgt, &sha), LockVerifyResult::Missing);
    }

    #[test]
    fn verify_installed_detects_tampered_binary() {
        let dir = tempdir().unwrap();
        let bin_path = dir.path().join("plugin-bin");
        fs::write(&bin_path, b"original-binary-content").unwrap();
        let original_sha = sha256_of_file(&bin_path).unwrap();
        let triple = current_target_triple().expect("test target has a known triple");

        let lock_path = dir.path().join("plugins.lock");
        let mut lock = PluginLockfile::empty_at(&lock_path);
        let mut targets = BTreeMap::new();
        targets.insert(
            triple.to_string(),
            TargetIntegrity {
                archive_sha256: "a".repeat(64),
                signature_bundle_sha256: None,
                installed_binary_sha256: Some(original_sha.clone()),
            },
        );
        lock.upsert(LockEntry {
            name: "myplugin".into(),
            version: "v1".into(),
            targets,
            installed_at: now_str(),
            installed_kind: None,
            native_kind: None,
            source_repo: None,
            resolved_commit: None,
            legacy_artifact_sha256: None,
            legacy_signature_bundle_sha256: None,
        });

        // No tamper — should match the recorded installed-binary hash.
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
    fn verify_entry_missing_target_when_no_installed_binary_hash() {
        // A target populated from a foreign platform's SHA256SUMS has an
        // archive sha but no installed_binary_sha256 — the host-only
        // verify_entry must report MissingTarget, not match against nothing.
        let dir = tempdir().unwrap();
        let path = dir.path().join("plugins.lock");
        let triple = current_target_triple().expect("known triple");
        let mut lock = PluginLockfile::empty_at(&path);
        let mut targets = BTreeMap::new();
        targets.insert(
            triple.to_string(),
            TargetIntegrity {
                archive_sha256: "a".repeat(64),
                signature_bundle_sha256: None,
                installed_binary_sha256: None,
            },
        );
        lock.upsert(LockEntry {
            name: "x".into(),
            version: "v1".into(),
            targets,
            installed_at: now_str(),
            installed_kind: None,
            native_kind: None,
            source_repo: None,
            resolved_commit: None,
            legacy_artifact_sha256: None,
            legacy_signature_bundle_sha256: None,
        });
        match lock.verify_entry("x", &"a".repeat(64)) {
            LockVerifyResult::MissingTarget { target } => assert_eq!(target, triple),
            other => panic!("expected MissingTarget, got {other:?}"),
        }
    }

    #[test]
    fn remove_returns_entry_and_drops_it() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("plugins.lock");
        let mut lock = PluginLockfile::empty_at(&path);
        lock.upsert(entry_with_local_target("drop-me", "", '9'));
        let removed = lock.remove("drop-me").expect("expected entry");
        assert_eq!(removed.name, "drop-me");
        assert!(lock.find("drop-me").is_none());
        assert!(lock.remove("drop-me").is_none());
    }

    fn entry(name: &str, sha_char: char) -> LockEntry {
        entry_with_local_target(name, "v1", sha_char)
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
    fn future_major_schema_is_rejected() {
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
    fn lockfile_roundtrips_installed_and_native_kind() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("plugins.lock");
        let mut lock = PluginLockfile::empty_at(&path);
        let mut e = entry_with_local_target("launchapp-dev/animus-subject-default-archive", "v0.1.0", 'c');
        e.installed_kind = Some("archive".to_string());
        e.native_kind = Some("task".to_string());
        lock.upsert(e);
        lock.save().unwrap();
        let reloaded = PluginLockfile::load_or_empty(&path).unwrap();
        let entry = &reloaded.plugins[0];
        assert_eq!(entry.installed_kind.as_deref(), Some("archive"));
        assert_eq!(entry.native_kind.as_deref(), Some("task"));
        assert_eq!(entry.effective_installed_kind(), Some("archive"));
        assert_eq!(entry.effective_native_kind(), Some("task"));
    }

    #[test]
    fn lockfile_roundtrips_source_repo_and_resolved_commit() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("plugins.lock");
        let mut lock = PluginLockfile::empty_at(&path);
        let mut e = entry_with_local_target("launchapp-dev/animus-provider-claude", "v0.2.2", 'a');
        e.source_repo = Some("launchapp-dev/animus-provider-claude".to_string());
        e.resolved_commit = Some("9".repeat(40));
        lock.upsert(e);
        lock.save().unwrap();
        let reloaded = PluginLockfile::load_or_empty(&path).unwrap();
        let entry = &reloaded.plugins[0];
        assert_eq!(entry.source_repo.as_deref(), Some("launchapp-dev/animus-provider-claude"));
        assert_eq!(entry.resolved_commit.as_deref(), Some("9".repeat(40).as_str()));
    }

    /// A 1.0 lockfile (flat `artifact_sha256`) must NOT hard-fail. It migrates
    /// to a 2.0 entry with an EMPTY `targets` map (the legacy binary hash is
    /// not a tarball sha) and is flagged `is_legacy_unverifiable()`. The next
    /// write emits 2.0.
    #[test]
    fn legacy_1_0_lockfile_migrates_to_empty_targets() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("plugins.lock");
        let legacy = "schema_version = \"1.0\"\ngenerated_at = \"2026-05-26T00:00:00Z\"\n\n[[plugins]]\nname = \"launchapp-dev/animus-subject-default\"\nversion = \"v0.1.1\"\nartifact_sha256 = \"abc123\"\nsignature_bundle_sha256 = \"def456\"\ninstalled_at = \"2026-05-26T00:00:00Z\"\nsource_repo = \"launchapp-dev/animus-subject-default\"\n";
        fs::write(&path, legacy).unwrap();
        let mut lock = PluginLockfile::load_or_empty(&path).expect("legacy 1.0 lockfile parses");
        assert_eq!(lock.schema_version, LOCKFILE_SCHEMA_VERSION, "migrated in-memory to 2.0");
        assert_eq!(lock.plugins.len(), 1);
        let entry = &lock.plugins[0];
        assert!(entry.targets.is_empty(), "legacy binary hash is not reusable as a tarball sha");
        assert!(entry.is_legacy_unverifiable());
        // Source provenance survives the migration (still needed to reinstall).
        assert_eq!(entry.source_repo.as_deref(), Some("launchapp-dev/animus-subject-default"));
        assert_eq!(entry.installed_kind, None);
        assert_eq!(entry.native_kind, None);

        // Next write emits 2.0 with no legacy fields.
        lock.save().unwrap();
        let raw = fs::read_to_string(&path).unwrap();
        assert!(raw.contains("schema_version = \"2.0\""));
        assert!(!raw.contains("artifact_sha256"), "2.0 write drops the flat legacy field: {raw}");
    }
}
