use std::io::{self, IsTerminal, Read, Write as IoWrite};
use std::path::Path;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{print_value, UpdateArgs};

const GITHUB_REPO: &str = "AudioGenius-ai/ao-cli";
const GITHUB_API_BASE: &str = "https://api.github.com";

#[derive(Debug, Deserialize)]
struct GithubRelease {
    tag_name: String,
    assets: Vec<GithubAsset>,
}

#[derive(Debug, Deserialize)]
struct GithubAsset {
    name: String,
    browser_download_url: String,
    size: u64,
}

#[derive(Debug, Serialize)]
struct UpdateCheckResult {
    current_version: String,
    latest_version: String,
    up_to_date: bool,
    update_available: bool,
}

pub(crate) async fn handle_update(args: UpdateArgs, json: bool) -> Result<()> {
    let current_version = env!("CARGO_PKG_VERSION");

    eprintln!("Fetching latest release information...");
    let client = build_client()?;
    let release = fetch_latest_release(&client).await?;

    let latest_tag = &release.tag_name;
    let latest_version = latest_tag.trim_start_matches('v');

    let current_sv = semver::Version::parse(current_version)
        .with_context(|| format!("could not parse current version: {current_version}"))?;
    let latest_sv = semver::Version::parse(latest_version)
        .with_context(|| format!("could not parse latest version from tag: {latest_tag}"))?;

    let up_to_date = current_sv >= latest_sv;

    if args.check || json {
        return print_value(
            UpdateCheckResult {
                current_version: current_version.to_string(),
                latest_version: latest_version.to_string(),
                up_to_date,
                update_available: !up_to_date,
            },
            json,
        );
    }

    println!("Current version: v{current_version}");
    println!("Latest version:  {latest_tag}");

    if up_to_date {
        println!("ao is already up to date.");
        return Ok(());
    }

    println!("Update available: v{current_version} → {latest_tag}");

    let target = platform_target()
        .ok_or_else(|| anyhow::anyhow!("unsupported platform; no prebuilt binary is available for this OS/arch"))?;

    let archive_ext = if cfg!(target_os = "windows") { "zip" } else { "tar.gz" };
    let archive_name = format!("ao-{latest_version}-{target}.{archive_ext}");

    let archive_asset = release
        .assets
        .iter()
        .find(|a| a.name == archive_name)
        .ok_or_else(|| anyhow::anyhow!("release asset not found for this platform: {archive_name}"))?;

    let checksums_asset = release
        .assets
        .iter()
        .find(|a| a.name == "SHA256SUMS.txt")
        .ok_or_else(|| anyhow::anyhow!("SHA256SUMS.txt not found in release assets"))?;

    if !args.yes && io::stdin().is_terminal() {
        print!("Install {latest_tag}? [y/N] ");
        io::stdout().flush()?;
        let mut input = String::new();
        io::stdin().read_line(&mut input)?;
        let trimmed = input.trim().to_ascii_lowercase();
        if trimmed != "y" && trimmed != "yes" {
            println!("Update cancelled.");
            return Ok(());
        }
    }

    let current_exe = std::env::current_exe().context("could not determine current binary path")?;

    let tmp_dir = tempfile::tempdir().context("could not create temporary directory")?;

    let archive_path = tmp_dir.path().join(&archive_asset.name);
    println!("Downloading {} ({} bytes)...", archive_asset.name, archive_asset.size);
    download_with_progress(&client, &archive_asset.browser_download_url, &archive_path, archive_asset.size).await?;

    println!("Verifying checksum...");
    let checksums_bytes = client
        .get(&checksums_asset.browser_download_url)
        .send()
        .await?
        .error_for_status()?
        .bytes()
        .await?;
    let checksums_text =
        String::from_utf8(checksums_bytes.to_vec()).context("SHA256SUMS.txt is not valid UTF-8")?;
    verify_sha256(&archive_path, &archive_asset.name, &checksums_text)?;
    println!("Checksum verified.");

    let binary_suffix = if cfg!(target_os = "windows") { "ao.exe" } else { "ao" };
    let binary_name_in_archive = format!("ao-{latest_version}-{target}/{binary_suffix}");

    println!("Extracting binary...");
    let extracted_binary = tmp_dir.path().join("ao_new");
    extract_binary(&archive_path, &binary_name_in_archive, &extracted_binary)?;

    println!("Installing...");
    install_binary(&extracted_binary, &current_exe)?;

    println!("ao updated to {latest_tag} successfully.");
    Ok(())
}

fn build_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .user_agent(concat!("ao/", env!("CARGO_PKG_VERSION")))
        .build()
        .context("could not build HTTP client")
}

async fn fetch_latest_release(client: &reqwest::Client) -> Result<GithubRelease> {
    let url = format!("{GITHUB_API_BASE}/repos/{GITHUB_REPO}/releases/latest");
    client
        .get(&url)
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .send()
        .await?
        .error_for_status()?
        .json::<GithubRelease>()
        .await
        .context("could not parse GitHub release response")
}

fn platform_target() -> Option<&'static str> {
    if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        Some("x86_64-unknown-linux-gnu")
    } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
        Some("x86_64-apple-darwin")
    } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        Some("aarch64-apple-darwin")
    } else if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        Some("x86_64-pc-windows-msvc")
    } else {
        None
    }
}

async fn download_with_progress(
    client: &reqwest::Client,
    url: &str,
    dest: &Path,
    expected_size: u64,
) -> Result<()> {
    use tokio::io::AsyncWriteExt;

    let mut response = client.get(url).send().await?.error_for_status()?;
    let mut file = tokio::fs::File::create(dest).await?;
    let mut downloaded: u64 = 0;
    let mut last_reported: u64 = 0;

    while let Some(chunk) = response.chunk().await? {
        file.write_all(&chunk).await?;
        downloaded += chunk.len() as u64;
        if downloaded.saturating_sub(last_reported) >= 1_000_000 || downloaded >= expected_size {
            if expected_size > 0 {
                let pct = downloaded * 100 / expected_size;
                eprint!("\r  {downloaded} / {expected_size} bytes ({pct}%)   ");
            } else {
                eprint!("\r  {downloaded} bytes   ");
            }
            let _ = io::stderr().flush();
            last_reported = downloaded;
        }
    }
    eprintln!();
    file.flush().await?;
    Ok(())
}

fn verify_sha256(archive_path: &Path, archive_name: &str, checksums: &str) -> Result<()> {
    let expected_hash = checksums
        .lines()
        .find_map(|line| {
            let mut parts = line.splitn(2, "  ");
            let hash = parts.next()?;
            let name = parts.next()?.trim();
            if name == archive_name { Some(hash.to_string()) } else { None }
        })
        .ok_or_else(|| anyhow::anyhow!("checksum for {archive_name} not found in SHA256SUMS.txt"))?;

    let mut file = std::fs::File::open(archive_path)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 65536];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    let actual = format!("{:x}", hasher.finalize());

    if actual != expected_hash {
        bail!("SHA256 checksum mismatch for {archive_name}: expected {expected_hash}, got {actual}");
    }
    Ok(())
}

fn extract_binary(archive_path: &Path, binary_name: &str, dest: &Path) -> Result<()> {
    let archive_file_name = archive_path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    if archive_file_name.ends_with(".tar.gz") {
        extract_from_targz(archive_path, binary_name, dest)
    } else if archive_file_name.ends_with(".zip") {
        extract_from_zip(archive_path, binary_name, dest)
    } else {
        bail!("unknown archive format: {archive_file_name}")
    }
}

fn extract_from_targz(archive_path: &Path, binary_name: &str, dest: &Path) -> Result<()> {
    use flate2::read::GzDecoder;
    use tar::Archive;

    let file = std::fs::File::open(archive_path)?;
    let gz = GzDecoder::new(file);
    let mut archive = Archive::new(gz);

    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?;
        if path.to_str() == Some(binary_name) {
            let mut out = std::fs::File::create(dest)?;
            io::copy(&mut entry, &mut out)?;
            return Ok(());
        }
    }
    bail!("binary '{binary_name}' not found in archive")
}

fn extract_from_zip(archive_path: &Path, binary_name: &str, dest: &Path) -> Result<()> {
    use zip::ZipArchive;

    let file = std::fs::File::open(archive_path)?;
    let mut archive = ZipArchive::new(file)?;

    let mut zip_entry = archive
        .by_name(binary_name)
        .with_context(|| format!("binary '{binary_name}' not found in archive"))?;

    let mut out = std::fs::File::create(dest)?;
    io::copy(&mut zip_entry, &mut out)?;
    Ok(())
}

#[cfg(not(target_os = "windows"))]
fn install_binary(src: &Path, dest: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let tmp_dest = dest.with_extension("ao_update_tmp");
    std::fs::copy(src, &tmp_dest)
        .with_context(|| format!("could not write to {}: check write permission", tmp_dest.display()))?;
    std::fs::set_permissions(&tmp_dest, std::fs::Permissions::from_mode(0o755))
        .context("could not set binary permissions")?;
    std::fs::rename(&tmp_dest, dest).with_context(|| {
        format!("could not replace binary at {}: check write permission", dest.display())
    })?;
    Ok(())
}

#[cfg(target_os = "windows")]
fn install_binary(src: &Path, dest: &Path) -> Result<()> {
    let tmp_dest = dest.with_extension("exe.ao_update_tmp");
    std::fs::copy(src, &tmp_dest)
        .with_context(|| format!("could not write to {}: check write permission", tmp_dest.display()))?;
    std::fs::rename(&tmp_dest, dest).with_context(|| {
        format!(
            "could not replace binary at {}: close all running ao processes and retry",
            dest.display()
        )
    })?;
    Ok(())
}
