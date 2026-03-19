use anyhow::{bail, Context, Result};
use chrono::{DateTime, Duration, Utc};
use protocol::UpdateConfig;
use semver::Version;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};

use crate::{print_value, UpdateArgs};

const CHECK_TTL_HOURS: i64 = 24;
const VERSION_CHECK_TIMEOUT_SECS: u64 = 10;
const DOWNLOAD_TIMEOUT_SECS: u64 = 300;

#[derive(Debug, Serialize, Deserialize, Clone)]
struct UpdateCheckCache {
    checked_at: DateTime<Utc>,
    latest_version: String,
    current_version: String,
}

#[derive(Debug, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum VersionCheckResult {
    UpToDate { current: String, latest: String },
    UpdateAvailable { current: String, latest: String },
    CheckFailed { reason: String },
}

#[derive(Debug, Serialize)]
struct InstallResult {
    path: String,
    previous_backed_up_to: String,
}

pub(crate) async fn handle_update(args: UpdateArgs, json: bool) -> Result<()> {
    let config = UpdateConfig::default();
    let current_version = env!("CARGO_PKG_VERSION").to_string();

    let check_result = check_version(&config, &current_version, args.version.as_deref(), args.force).await;

    if args.check {
        return print_value(serde_json::json!({ "check": check_result }), json);
    }

    let latest_version = match &check_result {
        VersionCheckResult::UpdateAvailable { latest, .. } => latest.clone(),
        VersionCheckResult::UpToDate { latest, .. } => {
            if args.force {
                latest.clone()
            } else {
                return print_value(
                    serde_json::json!({
                        "action": "skipped",
                        "reason": "already_up_to_date",
                        "check": check_result,
                    }),
                    json,
                );
            }
        }
        VersionCheckResult::CheckFailed { reason } => {
            return print_value(
                serde_json::json!({
                    "action": "failed",
                    "reason": reason,
                    "check": check_result,
                }),
                json,
            );
        }
    };

    let install = download_and_install(&config, &latest_version).await?;

    print_value(
        serde_json::json!({
            "action": "updated",
            "from": current_version,
            "to": latest_version,
            "check": check_result,
            "install": install,
        }),
        json,
    )
}

async fn check_version(
    config: &UpdateConfig,
    current_version: &str,
    pinned_version: Option<&str>,
    force: bool,
) -> VersionCheckResult {
    let cache_path = UpdateConfig::update_check_cache_path();

    if pinned_version.is_none() && !force {
        if let Some(cached) = load_valid_cache(&cache_path, current_version) {
            return compare_versions(current_version, &cached.latest_version);
        }
    }

    let url = config.releases_api_url(pinned_version);
    match fetch_latest_version_tag(&url).await {
        Ok(latest) => {
            let _ = save_cache(
                &cache_path,
                &UpdateCheckCache {
                    checked_at: Utc::now(),
                    latest_version: latest.clone(),
                    current_version: current_version.to_string(),
                },
            );
            compare_versions(current_version, &latest)
        }
        Err(e) => VersionCheckResult::CheckFailed { reason: e.to_string() },
    }
}

fn compare_versions(current: &str, latest: &str) -> VersionCheckResult {
    let cur = Version::parse(current);
    let lat = Version::parse(latest.trim_start_matches('v'));

    match (cur, lat) {
        (Ok(cur), Ok(lat)) => {
            if lat > cur {
                VersionCheckResult::UpdateAvailable { current: current.to_string(), latest: latest.to_string() }
            } else {
                VersionCheckResult::UpToDate { current: current.to_string(), latest: latest.to_string() }
            }
        }
        _ => VersionCheckResult::CheckFailed {
            reason: format!("Failed to parse versions: current={current}, latest={latest}"),
        },
    }
}

fn load_valid_cache(cache_path: &Path, current_version: &str) -> Option<UpdateCheckCache> {
    let content = fs::read_to_string(cache_path).ok()?;
    let cache: UpdateCheckCache = serde_json::from_str(&content).ok()?;

    if cache.current_version != current_version {
        return None;
    }

    let age = Utc::now().signed_duration_since(cache.checked_at);
    if age > Duration::hours(CHECK_TTL_HOURS) {
        return None;
    }

    Some(cache)
}

fn save_cache(cache_path: &Path, cache: &UpdateCheckCache) -> Result<()> {
    if let Some(parent) = cache_path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(cache_path, serde_json::to_string_pretty(cache)?)?;
    Ok(())
}

async fn fetch_latest_version_tag(url: &str) -> Result<String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(VERSION_CHECK_TIMEOUT_SECS))
        .user_agent(format!("ao-cli/{}", env!("CARGO_PKG_VERSION")))
        .build()
        .context("Failed to build HTTP client")?;

    let response = client
        .get(url)
        .header("Accept", "application/vnd.github.v3+json")
        .send()
        .await
        .context("Failed to connect to GitHub API")?;

    if response.status() == reqwest::StatusCode::FORBIDDEN {
        bail!("GitHub API rate limit exceeded — try again later");
    }

    if !response.status().is_success() {
        bail!("GitHub API returned {}: {url}", response.status());
    }

    let body: serde_json::Value = response.json().await.context("Failed to parse GitHub API response")?;

    body["tag_name"]
        .as_str()
        .map(|t| t.trim_start_matches('v').to_string())
        .ok_or_else(|| anyhow::anyhow!("GitHub API response missing tag_name field"))
}

async fn download_and_install(config: &UpdateConfig, version: &str) -> Result<InstallResult> {
    let platform = detect_platform()?;
    let archive_name = format!("ao-{platform}.tar.gz");
    let base_url = config.download_base_url(version);
    let archive_url = format!("{base_url}/{archive_name}");
    let checksums_url = format!("{base_url}/SHA256SUMS.txt");

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(DOWNLOAD_TIMEOUT_SECS))
        .user_agent(format!("ao-cli/{}", env!("CARGO_PKG_VERSION")))
        .build()
        .context("Failed to build HTTP client")?;

    let checksums = client
        .get(&checksums_url)
        .send()
        .await
        .context("Failed to download SHA256SUMS.txt")?
        .text()
        .await
        .context("Failed to read SHA256SUMS.txt")?;

    let expected_checksum = parse_checksum(&checksums, &archive_name)
        .ok_or_else(|| anyhow::anyhow!("Checksum for {archive_name} not found in SHA256SUMS.txt"))?;

    let archive_bytes = client
        .get(&archive_url)
        .send()
        .await
        .context("Failed to download archive")?
        .bytes()
        .await
        .context("Failed to read archive bytes")?;

    let actual_checksum: String = Sha256::digest(&archive_bytes).iter().map(|b| format!("{b:02x}")).collect();

    if actual_checksum != expected_checksum {
        bail!("Checksum mismatch: expected {expected_checksum}, got {actual_checksum}");
    }

    let temp_dir = std::env::temp_dir().join(format!("ao-update-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&temp_dir).context("Failed to create temp dir")?;

    let result = extract_and_install(&archive_bytes, &temp_dir, &archive_name);
    let _ = fs::remove_dir_all(&temp_dir);
    result
}

fn detect_platform() -> Result<&'static str> {
    let platform = match (std::env::consts::ARCH, std::env::consts::OS) {
        ("x86_64", "linux") => "x86_64-linux",
        ("x86_64", "macos") => "x86_64-darwin",
        ("aarch64", "macos") => "aarch64-darwin",
        ("x86_64", "windows") => "x86_64-windows",
        (arch, os) => bail!("Unsupported platform: {arch}/{os}"),
    };
    Ok(platform)
}

fn parse_checksum(checksums: &str, filename: &str) -> Option<String> {
    for line in checksums.lines() {
        let mut parts = line.splitn(2, ' ');
        let hash = parts.next()?.trim();
        let file = parts.next()?.trim().trim_start_matches('*');
        if file == filename {
            return Some(hash.to_string());
        }
    }
    None
}

fn extract_and_install(archive_bytes: &[u8], temp_dir: &Path, archive_name: &str) -> Result<InstallResult> {
    let archive_path = temp_dir.join(archive_name);
    fs::write(&archive_path, archive_bytes).context("Failed to write archive to temp dir")?;

    let output = std::process::Command::new("tar")
        .args(["xzf", archive_path.to_str().unwrap_or(""), "-C", temp_dir.to_str().unwrap_or("")])
        .output()
        .context("Failed to run tar — ensure tar is installed on your PATH")?;

    if !output.status.success() {
        bail!("tar extraction failed: {}", String::from_utf8_lossy(&output.stderr));
    }

    let new_binary = find_binary(temp_dir)?;
    let current_exe = std::env::current_exe().context("Failed to determine current executable path")?;

    smoke_test(&new_binary)?;
    atomic_install(&current_exe, &new_binary)
}

fn find_binary(dir: &Path) -> Result<PathBuf> {
    let binary_name = if cfg!(target_os = "windows") { "ao.exe" } else { "ao" };

    let direct = dir.join(binary_name);
    if direct.exists() {
        return Ok(direct);
    }

    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            let candidate = path.join(binary_name);
            if candidate.exists() {
                return Ok(candidate);
            }
        }
    }

    bail!("Binary '{binary_name}' not found in extracted archive at {}", dir.display())
}

fn smoke_test(binary: &Path) -> Result<()> {
    let output = std::process::Command::new(binary)
        .arg("--version")
        .output()
        .context("Failed to run smoke test on new binary")?;

    if !output.status.success() {
        bail!(
            "New binary failed smoke test (--version exited {}): {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    Ok(())
}

fn atomic_install(current: &Path, new_binary: &Path) -> Result<InstallResult> {
    let backup = current.with_extension("old");

    fs::rename(current, &backup).with_context(|| {
        format!("Failed to rename {} to backup", current.display())
    })?;

    if let Err(e) = fs::copy(new_binary, current) {
        let _ = fs::rename(&backup, current);
        return Err(e).context("Failed to copy new binary; rolled back to previous version");
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        match fs::metadata(current) {
            Ok(meta) => {
                let mut perms = meta.permissions();
                perms.set_mode(0o755);
                let _ = fs::set_permissions(current, perms);
            }
            Err(_) => {}
        }
    }

    let _ = fs::remove_file(&backup);

    Ok(InstallResult {
        path: current.display().to_string(),
        previous_backed_up_to: backup.display().to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compare_versions_detects_update_available() {
        let result = compare_versions("0.0.10", "0.1.0");
        assert!(matches!(result, VersionCheckResult::UpdateAvailable { .. }));
    }

    #[test]
    fn compare_versions_up_to_date_when_equal() {
        let result = compare_versions("0.0.10", "v0.0.10");
        assert!(matches!(result, VersionCheckResult::UpToDate { .. }));
    }

    #[test]
    fn compare_versions_up_to_date_when_ahead() {
        let result = compare_versions("0.1.0", "0.0.9");
        assert!(matches!(result, VersionCheckResult::UpToDate { .. }));
    }

    #[test]
    fn parse_checksum_finds_entry_with_two_spaces() {
        let checksums = "abc123  ao-x86_64-linux.tar.gz\ndef456  ao-aarch64-darwin.tar.gz\n";
        assert_eq!(parse_checksum(checksums, "ao-x86_64-linux.tar.gz"), Some("abc123".to_string()));
        assert_eq!(parse_checksum(checksums, "ao-aarch64-darwin.tar.gz"), Some("def456".to_string()));
    }

    #[test]
    fn parse_checksum_finds_entry_with_star_prefix() {
        let checksums = "abc123 *ao-x86_64-darwin.tar.gz\n";
        assert_eq!(parse_checksum(checksums, "ao-x86_64-darwin.tar.gz"), Some("abc123".to_string()));
    }

    #[test]
    fn parse_checksum_returns_none_for_missing_entry() {
        let checksums = "abc123  ao-x86_64-linux.tar.gz\n";
        assert_eq!(parse_checksum(checksums, "ao-aarch64-darwin.tar.gz"), None);
    }

    #[test]
    fn detect_platform_returns_known_triple() {
        let result = detect_platform();
        assert!(result.is_ok(), "Platform detection should succeed on CI targets");
        let triple = result.unwrap();
        assert!(["x86_64-linux", "x86_64-darwin", "aarch64-darwin", "x86_64-windows"].contains(&triple));
    }

    #[test]
    fn update_check_cache_ttl_invalidates_old_entry() {
        let cache = UpdateCheckCache {
            checked_at: Utc::now() - Duration::hours(25),
            latest_version: "1.0.0".to_string(),
            current_version: "0.0.10".to_string(),
        };

        let dir = std::env::temp_dir().join(format!("ao-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("update-check.json");
        std::fs::write(&path, serde_json::to_string(&cache).unwrap()).unwrap();

        let result = load_valid_cache(&path, "0.0.10");
        assert!(result.is_none(), "Expired cache should not be returned");

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn update_check_cache_invalidates_on_version_change() {
        let cache = UpdateCheckCache {
            checked_at: Utc::now(),
            latest_version: "1.0.0".to_string(),
            current_version: "0.0.9".to_string(),
        };

        let dir = std::env::temp_dir().join(format!("ao-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("update-check.json");
        std::fs::write(&path, serde_json::to_string(&cache).unwrap()).unwrap();

        let result = load_valid_cache(&path, "0.0.10");
        assert!(result.is_none(), "Cache for different version should be invalidated");

        let _ = std::fs::remove_dir_all(dir);
    }
}
