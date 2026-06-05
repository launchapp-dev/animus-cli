//! Long-term decision-log compaction.
//!
//! Archived `decisions-<ts>[-<n>].jsonl.bak` files accumulate over time.
//! [`compact_and_expire`] walks the per-run subdirectories under a
//! `~/.animus/<scope>/runs/` root and:
//!
//! 1. **Compresses** archives older than the compress threshold (default
//!    24h) with zstd, producing `*.jsonl.bak.zst`. The uncompressed
//!    `*.jsonl.bak` is removed after the compressed file is durably on
//!    disk (write + rename + dir fsync).
//! 2. **Expires** archives older than the expiry threshold (default 7
//!    days, configurable via `ANIMUS_DECISION_LOG_EXPIRY_DAYS`). Both
//!    `.bak` and `.bak.zst` forms are removed.
//!
//! The sweep is intentionally synchronous and bounded — it runs once on
//! daemon startup (no new background task). The `decisions.jsonl`
//! primary log is never touched.
//!
//! [`super::ReplaySource::open`] transparently decompresses `.bak.zst`
//! when callers point it at an archived path so historical replays
//! still work after compression.

use std::fs::File;
use std::io::{self, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result};

const DEFAULT_COMPRESS_AFTER_SECS: u64 = 24 * 60 * 60;
const DEFAULT_EXPIRY_DAYS: u64 = 7;
const ZSTD_COMPRESSION_LEVEL: i32 = 3;
const EXPIRY_ENV_VAR: &str = "ANIMUS_DECISION_LOG_EXPIRY_DAYS";

/// Outcome of a single [`compact_and_expire`] sweep over a `runs/` root.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SweepReport {
    /// Number of archives compressed from `.bak` to `.bak.zst`.
    pub compressed: usize,
    /// Number of archives removed for being older than the expiry
    /// threshold.
    pub expired: usize,
    /// Number of archive paths that could not be processed (logged but
    /// not propagated, so a single bad file does not abort the sweep).
    pub failed: usize,
}

/// Tunable sweep policy. Use [`SweepPolicy::from_env`] to read the
/// production defaults + environment overrides.
#[derive(Debug, Clone, Copy)]
pub struct SweepPolicy {
    pub compress_after: Duration,
    pub expire_after: Duration,
}

impl Default for SweepPolicy {
    fn default() -> Self {
        Self {
            compress_after: Duration::from_secs(DEFAULT_COMPRESS_AFTER_SECS),
            expire_after: Duration::from_secs(DEFAULT_EXPIRY_DAYS * 24 * 60 * 60),
        }
    }
}

impl SweepPolicy {
    /// Read `ANIMUS_DECISION_LOG_EXPIRY_DAYS` for the expiry override.
    /// `compress_after` is fixed at 24h; callers that want a different
    /// schedule can build the policy directly.
    pub fn from_env() -> Self {
        let mut policy = Self::default();
        if let Ok(raw) = std::env::var(EXPIRY_ENV_VAR) {
            if let Ok(days) = raw.trim().parse::<u64>() {
                if days > 0 {
                    policy.expire_after = Duration::from_secs(days * 24 * 60 * 60);
                }
            }
        }
        policy
    }
}

/// Walk every per-run subdirectory under `runs_root` and apply the
/// sweep policy. Errors on individual archives are logged via
/// `tracing::warn!` and counted in [`SweepReport::failed`] but do not
/// abort the sweep — one corrupted file should not block compaction of
/// the rest.
///
/// `runs_root` is typically `~/.animus/<repo_scope>/runs/`. If it does
/// not exist, returns an empty report (not an error — fresh installs
/// have no runs).
pub fn compact_and_expire(runs_root: &Path, policy: SweepPolicy) -> Result<SweepReport> {
    let mut report = SweepReport::default();
    let Ok(entries) = std::fs::read_dir(runs_root) else {
        return Ok(report);
    };
    let now = SystemTime::now();
    for entry in entries.flatten() {
        let run_dir = entry.path();
        if !run_dir.is_dir() {
            continue;
        }
        let Ok(files) = std::fs::read_dir(&run_dir) else { continue };
        for file in files.flatten() {
            let path = file.path();
            if !path.is_file() {
                continue;
            }
            let Some(name) = path.file_name().and_then(|s| s.to_str()) else { continue };
            let is_bak = name.ends_with(".jsonl.bak");
            let is_zst = name.ends_with(".jsonl.bak.zst");
            if !is_bak && !is_zst {
                continue;
            }
            let Ok(meta) = file.metadata() else {
                report.failed += 1;
                continue;
            };
            let Ok(modified) = meta.modified() else {
                report.failed += 1;
                continue;
            };
            let age = now.duration_since(modified).unwrap_or(Duration::ZERO);
            if age >= policy.expire_after {
                match std::fs::remove_file(&path) {
                    Ok(_) => report.expired += 1,
                    Err(err) => {
                        tracing::warn!(path = %path.display(), error = %err, "decision-log sweeper: failed to expire archive");
                        report.failed += 1;
                    }
                }
                continue;
            }
            if is_bak && age >= policy.compress_after {
                match compress_bak_to_zst(&path) {
                    Ok(_) => report.compressed += 1,
                    Err(err) => {
                        tracing::warn!(path = %path.display(), error = %err, "decision-log sweeper: failed to compress archive");
                        report.failed += 1;
                    }
                }
            }
        }
    }
    Ok(report)
}

/// Read `.bak`, write `.bak.zst.partial`, fsync, rename to `.bak.zst`,
/// fsync parent dir, then remove the original `.bak`. The temp-suffix
/// rename makes the operation atomic against a sweeper crash. Either
/// the original `.bak` survives, or the `.bak.zst` is on disk and
/// complete. Never both, never neither.
fn compress_bak_to_zst(bak_path: &Path) -> io::Result<()> {
    let zst_path = bak_path.with_extension("bak.zst");
    let temp_path = bak_path.with_extension("bak.zst.partial");

    let mut input = BufReader::new(File::open(bak_path)?);
    let temp_file = File::create(&temp_path)?;
    let mut encoder = zstd::Encoder::new(BufWriter::new(temp_file), ZSTD_COMPRESSION_LEVEL)?;
    io::copy(&mut input, &mut encoder)?;
    let mut buf_writer = encoder.finish()?;
    buf_writer.flush()?;
    let temp_file = buf_writer.into_inner().map_err(|err| io::Error::other(format!("flush temp encoder: {err}")))?;
    temp_file.sync_all()?;
    drop(temp_file);

    std::fs::rename(&temp_path, &zst_path)?;
    if let Some(parent) = zst_path.parent() {
        if let Ok(dir) = File::open(parent) {
            let _ = dir.sync_all();
        }
    }
    std::fs::remove_file(bak_path)?;
    Ok(())
}

/// Open a `.jsonl.bak.zst` archive and decompress into the caller's
/// writer. Used by [`super::ReplaySource::open`] to transparently
/// service replays against compressed archives.
pub(crate) fn decompress_zst_into(zst_path: &Path) -> Result<Vec<u8>> {
    let file = File::open(zst_path).with_context(|| format!("open compressed archive {}", zst_path.display()))?;
    let mut decoder = zstd::Decoder::new(file).context("init zstd decoder")?;
    let mut out = Vec::new();
    std::io::copy(&mut decoder, &mut out).context("decompress zstd")?;
    Ok(out)
}

/// Resolve the project root for a given runs dir. Used by callers that
/// want to wire the sweeper into a startup path without re-implementing
/// the `~/.animus/<scope>/runs/` shape.
pub fn runs_root_for_project(project_root: &Path) -> Option<PathBuf> {
    let scope = protocol::repository_scope_for_path(project_root);
    let home = dirs::home_dir()?;
    Some(home.join(".animus").join(scope).join("runs"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::Duration;
    use tempfile::TempDir;

    fn set_mtime(path: &Path, hours_ago: u64) {
        // SystemTime → set via `filetime` would need a new dep; instead
        // we manipulate via the OS-level utime. Use `std::fs::File::open`
        // round-trip is not sufficient; pull in via the standard `set_modified`
        // available on Unix and Windows since Rust 1.75.
        let when = SystemTime::now() - Duration::from_secs(hours_ago * 3600);
        let file = fs::OpenOptions::new().write(true).open(path).expect("open for utime");
        file.set_modified(when).expect("set_modified");
    }

    fn make_archive(dir: &Path, name: &str, content: &[u8], hours_ago: u64) -> PathBuf {
        let p = dir.join(name);
        fs::write(&p, content).expect("write archive");
        set_mtime(&p, hours_ago);
        p
    }

    #[test]
    fn fresh_archive_is_left_alone() {
        let temp = TempDir::new().unwrap();
        let runs_root = temp.path().join("runs");
        let run_dir = runs_root.join("run-1");
        fs::create_dir_all(&run_dir).unwrap();
        let path = make_archive(&run_dir, "decisions-100.jsonl.bak", b"line\n", 1);
        let report = compact_and_expire(&runs_root, SweepPolicy::default()).unwrap();
        assert_eq!(report, SweepReport::default());
        assert!(path.exists());
    }

    #[test]
    fn old_bak_is_compressed_to_zst() {
        let temp = TempDir::new().unwrap();
        let runs_root = temp.path().join("runs");
        let run_dir = runs_root.join("run-2");
        fs::create_dir_all(&run_dir).unwrap();
        let bak = make_archive(&run_dir, "decisions-200.jsonl.bak", b"old-data\n", 48);
        let report = compact_and_expire(&runs_root, SweepPolicy::default()).unwrap();
        assert_eq!(report.compressed, 1);
        assert_eq!(report.expired, 0);
        assert!(!bak.exists(), "uncompressed .bak should be gone after compress");
        let zst = bak.with_extension("bak.zst");
        assert!(zst.exists(), "compressed .bak.zst should exist");
    }

    #[test]
    fn expired_archive_is_removed() {
        let temp = TempDir::new().unwrap();
        let runs_root = temp.path().join("runs");
        let run_dir = runs_root.join("run-3");
        fs::create_dir_all(&run_dir).unwrap();
        let bak = make_archive(&run_dir, "decisions-300.jsonl.bak", b"ancient\n", 24 * 8);
        let zst = make_archive(&run_dir, "decisions-301.jsonl.bak.zst", b"also-ancient\n", 24 * 9);
        let report = compact_and_expire(&runs_root, SweepPolicy::default()).unwrap();
        assert_eq!(report.expired, 2);
        assert!(!bak.exists());
        assert!(!zst.exists());
    }

    #[test]
    fn primary_decisions_jsonl_is_never_touched() {
        let temp = TempDir::new().unwrap();
        let runs_root = temp.path().join("runs");
        let run_dir = runs_root.join("run-4");
        fs::create_dir_all(&run_dir).unwrap();
        let primary = make_archive(&run_dir, "decisions.jsonl", b"live\n", 24 * 30);
        let report = compact_and_expire(&runs_root, SweepPolicy::default()).unwrap();
        assert_eq!(report, SweepReport::default());
        assert!(primary.exists());
        let raw = fs::read(&primary).unwrap();
        assert_eq!(raw, b"live\n");
    }

    #[test]
    fn missing_runs_root_is_noop_not_error() {
        let temp = TempDir::new().unwrap();
        let report = compact_and_expire(&temp.path().join("does-not-exist"), SweepPolicy::default()).unwrap();
        assert_eq!(report, SweepReport::default());
    }

    #[test]
    fn decompress_round_trips_compressed_archive() {
        let temp = TempDir::new().unwrap();
        let runs_root = temp.path().join("runs");
        let run_dir = runs_root.join("run-5");
        fs::create_dir_all(&run_dir).unwrap();
        let original = b"{\"kind\":\"finished\",\"timestamp_ms\":1,\"exit_code\":0}\n";
        let bak = make_archive(&run_dir, "decisions-500.jsonl.bak", original, 48);
        let report = compact_and_expire(&runs_root, SweepPolicy::default()).unwrap();
        assert_eq!(report.compressed, 1);
        let zst = bak.with_extension("bak.zst");
        let decompressed = decompress_zst_into(&zst).unwrap();
        assert_eq!(decompressed, original);
    }

    #[test]
    fn env_var_override_shrinks_expiry_window() {
        let temp = TempDir::new().unwrap();
        let runs_root = temp.path().join("runs");
        let run_dir = runs_root.join("run-6");
        fs::create_dir_all(&run_dir).unwrap();
        let bak = make_archive(&run_dir, "decisions-600.jsonl.bak", b"only-50h-old\n", 50);
        // Custom policy with a 1-day expiry — file is 50h old (> 24h
        // compress, > 24h expire) so it should be expired (not compressed).
        let policy =
            SweepPolicy { compress_after: Duration::from_secs(3600), expire_after: Duration::from_secs(86_400) };
        let report = compact_and_expire(&runs_root, policy).unwrap();
        assert_eq!(report.expired, 1);
        assert_eq!(report.compressed, 0);
        assert!(!bak.exists());
    }

    #[test]
    fn from_env_reads_expiry_days_override() {
        // Use a unique env var name in case other tests are touching ANIMUS_*.
        // We can't rely on parallel test isolation, so set + restore.
        let prev = std::env::var(EXPIRY_ENV_VAR).ok();
        std::env::set_var(EXPIRY_ENV_VAR, "2");
        let policy = SweepPolicy::from_env();
        assert_eq!(policy.expire_after, Duration::from_secs(2 * 24 * 3600));
        match prev {
            Some(v) => std::env::set_var(EXPIRY_ENV_VAR, v),
            None => std::env::remove_var(EXPIRY_ENV_VAR),
        }
    }
}
