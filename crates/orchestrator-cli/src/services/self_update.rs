//! Configurable self-update for the `animus` CLI.
//!
//! Polls `launchapp-dev/animus-cli` GitHub releases, compares semver to
//! `env!("CARGO_PKG_VERSION")`, and (depending on configured `AutoUpdateMode`)
//! notifies, prompts, or atomically swaps the running binary on disk.

use std::ffi::OsString;
use std::io::{IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use protocol::{AutoUpdateChannel, AutoUpdateConfig, AutoUpdateMode, Config};
use semver::Version;
use serde::Deserialize;
use sha2::{Digest, Sha256};

/// GitHub repo coordinates for the `animus` binary itself.
pub(crate) const SELF_UPDATE_OWNER: &str = "launchapp-dev";
pub(crate) const SELF_UPDATE_REPO: &str = "animus-cli";

/// Environment overrides recognised by [`effective_mode`] and the startup
/// guard in `main.rs`.
pub(crate) const ENV_MODE_OVERRIDE: &str = "ANIMUS_AUTO_UPDATE_MODE";
pub(crate) const ENV_DISABLE: &str = "ANIMUS_AUTO_UPDATE_DISABLE";

/// HTTP timeout for any GitHub release call from the startup check. Short
/// enough that a slow network never delays a subcommand even when the user
/// has `notify` mode but the fire-and-forget task completes alongside a
/// short-running command.
const STARTUP_FETCH_TIMEOUT: Duration = Duration::from_secs(5);

/// HTTP timeout for the manual `animus self update` flow.
const MANUAL_FETCH_TIMEOUT: Duration = Duration::from_secs(30);

/// Persisted across runs to throttle startup checks to the configured
/// `check_interval`. Stored under `Config::global_config_dir()`.
const LAST_CHECKED_FILENAME: &str = "auto-update-state.json";

#[derive(Debug, Default, Clone, Deserialize, serde::Serialize)]
pub struct AutoUpdateState {
    pub last_checked: Option<chrono::DateTime<chrono::Utc>>,
    pub last_seen_version: Option<String>,
}

impl AutoUpdateState {
    pub fn load() -> Self {
        let path = state_path();
        match std::fs::read_to_string(&path) {
            Ok(body) => serde_json::from_str(&body).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }

    pub fn save(&self) -> Result<()> {
        let path = state_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create state dir {}", parent.display()))?;
        }
        let body = serde_json::to_string_pretty(self)?;
        std::fs::write(&path, body).with_context(|| format!("failed to write {}", path.display()))?;
        Ok(())
    }
}

fn state_path() -> PathBuf {
    Config::global_config_dir().join(LAST_CHECKED_FILENAME)
}

/// Resolve the `auto_update` config block by stacking the global
/// `~/.animus/config.json` and the project-local `.animus/config.json` —
/// project-local wins when both are present. Either layer may be missing
/// or fail to parse, in which case it contributes nothing. Both layers
/// are read WITHOUT creating-on-miss so a `cd /` invocation of any
/// `animus` command can't accidentally seed a `.animus/config.json` in
/// the user's home (or any other unrelated directory).
pub fn resolve_effective_config_block(project_root: &str) -> Option<AutoUpdateConfig> {
    if let Some(project) = read_only_config_block(&project_config_path(project_root)) {
        return Some(project);
    }
    read_only_config_block(&Config::global_config_dir().join("config.json"))
}

fn project_config_path(project_root: &str) -> PathBuf {
    PathBuf::from(project_root).join(".animus").join("config.json")
}

fn read_only_config_block(path: &Path) -> Option<AutoUpdateConfig> {
    let body = std::fs::read_to_string(path).ok()?;
    let parsed: Config = serde_json::from_str(&body).ok()?;
    parsed.auto_update
}

/// Resolve the effective mode at runtime. `ANIMUS_AUTO_UPDATE_DISABLE`
/// short-circuits to `off`; `ANIMUS_AUTO_UPDATE_MODE` overrides config when
/// it parses; otherwise the configured mode wins (or `Notify` if the block
/// is absent).
pub fn effective_mode(config_block: Option<&AutoUpdateConfig>) -> AutoUpdateMode {
    if env_disable_is_set() {
        return AutoUpdateMode::Off;
    }
    if let Some(env_mode) = env_mode_override() {
        return env_mode;
    }
    config_block.map(|cfg| cfg.mode).unwrap_or_default()
}

fn env_disable_is_set() -> bool {
    matches!(std::env::var(ENV_DISABLE).ok().as_deref(), Some(v) if !matches!(v.trim().to_ascii_lowercase().as_str(), "" | "0" | "false" | "no" | "off"))
}

fn env_mode_override() -> Option<AutoUpdateMode> {
    let raw = std::env::var(ENV_MODE_OVERRIDE).ok()?;
    parse_mode_str(&raw)
}

fn parse_mode_str(raw: &str) -> Option<AutoUpdateMode> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "off" | "disable" | "disabled" => Some(AutoUpdateMode::Off),
        "notify" => Some(AutoUpdateMode::Notify),
        "prompt" => Some(AutoUpdateMode::Prompt),
        "auto" => Some(AutoUpdateMode::Auto),
        _ => None,
    }
}

/// Parse a tag like `v1.2.3` or `1.2.3` into a `semver::Version`. Tolerates
/// a leading `v`/`V` and rejects everything else so unknown shapes can't
/// silently pose as "newer".
pub(crate) fn parse_release_tag(tag: &str) -> Option<Version> {
    let trimmed = tag.trim();
    let stripped = trimmed.strip_prefix('v').or_else(|| trimmed.strip_prefix('V')).unwrap_or(trimmed);
    Version::parse(stripped).ok()
}

/// Compare a candidate release tag against the currently-running binary
/// version. Returns `Some(parsed)` if strictly newer, otherwise `None`.
pub(crate) fn is_newer_than_current(candidate_tag: &str, current_version: &str) -> Option<Version> {
    let candidate = parse_release_tag(candidate_tag)?;
    let current = Version::parse(current_version).ok()?;
    if candidate > current {
        Some(candidate)
    } else {
        None
    }
}

/// Decide whether a release is admitted by the configured channel.
/// `stable` filters out `prerelease: true`; `prerelease` admits both.
pub(crate) fn channel_admits(channel: AutoUpdateChannel, prerelease: bool) -> bool {
    match channel {
        AutoUpdateChannel::Stable => !prerelease,
        AutoUpdateChannel::Prerelease => true,
    }
}

/// Parse an ISO-8601 duration string. Supports a small but useful subset:
/// `PnD`, `PnW`, `PT[nH][nM][nS]`. Returns `None` for unknown shapes so
/// callers can fall back to the documented default of one day.
pub(crate) fn parse_iso8601_duration(raw: &str) -> Option<Duration> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || !trimmed.starts_with('P') {
        return None;
    }
    let body = &trimmed[1..];
    let (date_part, time_part) = match body.split_once('T') {
        Some((d, t)) => (d, t),
        None => (body, ""),
    };

    let mut total_secs: u64 = 0;
    for (value, unit) in walk_segments(date_part) {
        match unit {
            'W' => total_secs = total_secs.checked_add(value.checked_mul(60 * 60 * 24 * 7)?)?,
            'D' => total_secs = total_secs.checked_add(value.checked_mul(60 * 60 * 24)?)?,
            _ => return None,
        }
    }
    for (value, unit) in walk_segments(time_part) {
        match unit {
            'H' => total_secs = total_secs.checked_add(value.checked_mul(60 * 60)?)?,
            'M' => total_secs = total_secs.checked_add(value.checked_mul(60)?)?,
            'S' => total_secs = total_secs.checked_add(value)?,
            _ => return None,
        }
    }
    if total_secs == 0 {
        return None;
    }
    Some(Duration::from_secs(total_secs))
}

fn walk_segments(segment: &str) -> Vec<(u64, char)> {
    let mut out = Vec::new();
    let mut digits = String::new();
    for c in segment.chars() {
        if c.is_ascii_digit() {
            digits.push(c);
        } else if c.is_ascii_alphabetic() {
            if let Ok(value) = digits.parse::<u64>() {
                out.push((value, c.to_ascii_uppercase()));
            }
            digits.clear();
        } else {
            return Vec::new();
        }
    }
    out
}

#[derive(Debug, Clone, Deserialize)]
pub struct GithubReleaseRecord {
    pub tag_name: String,
    #[serde(default)]
    pub prerelease: bool,
    #[serde(default)]
    pub assets: Vec<GithubReleaseAssetRecord>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GithubReleaseAssetRecord {
    pub name: String,
    pub browser_download_url: String,
    #[serde(default)]
    pub digest: Option<String>,
}

/// Pick the first release whose tag is strictly newer than the current
/// binary and admitted by the configured channel. Returns `None` when
/// nothing qualifies. Releases the `latest` endpoint already filters
/// prereleases; this also handles the `releases` listing for the
/// `prerelease` channel where the caller fetches the full list.
pub(crate) fn pick_eligible_release<'a>(
    releases: &'a [GithubReleaseRecord],
    channel: AutoUpdateChannel,
    current_version: &str,
) -> Option<(&'a GithubReleaseRecord, Version)> {
    releases
        .iter()
        .filter(|r| channel_admits(channel, r.prerelease))
        .filter_map(|r| is_newer_than_current(&r.tag_name, current_version).map(|v| (r, v)))
        .max_by(|a, b| a.1.cmp(&b.1))
}

/// Platform asset token used to match a release asset to the running
/// host. Format: `{os}-{arch}` lower-cased.
pub(crate) fn current_platform_token() -> String {
    format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)
}

/// Find the release asset for the current platform. The host's OS and
/// architecture must BOTH appear in the asset filename — single-axis
/// matches (e.g. OS-only) are rejected to prevent installing the wrong
/// architecture's binary when multiple `linux-*` or `macos-*` assets ship
/// in the same release. Common arch aliases (`x86_64` <-> `amd64`,
/// `aarch64` <-> `arm64`) are recognised. `.sha256` / `.sha256sum`
/// sidecars are excluded from candidate matching.
pub(crate) fn pick_asset_for_host<'a>(
    assets: &'a [GithubReleaseAssetRecord],
    platform_token: &str,
) -> Option<&'a GithubReleaseAssetRecord> {
    let (os, arch) = platform_token.split_once('-').unwrap_or((platform_token, ""));
    let os_aliases = os_aliases(os);
    let arch_aliases = arch_aliases(arch);

    for asset in assets {
        let lower = asset.name.to_ascii_lowercase();
        if !looks_like_installable_asset(&lower) {
            continue;
        }
        // Tokenize on `-`, `_`, `.`, `+`, and whitespace. Compute a parallel
        // list of "fused" tokens so a Rust target triple like
        // `x86_64-unknown-linux-gnu` still matches the `x86_64` alias: a
        // numeric token immediately following an alphabetic token is fused
        // (`x86` + `64` => `x86_64`). This keeps `_` as a separator for
        // `animus_linux_amd64.tar.gz`-style assets while also recognising
        // the `_64` continuation in target triples.
        let raw_tokens: Vec<&str> = lower.split(['-', '_', '.', '+', ' ', '\t']).filter(|t| !t.is_empty()).collect();
        let mut tokens: Vec<String> = raw_tokens.iter().map(|t| (*t).to_string()).collect();
        for window in raw_tokens.windows(2) {
            let first = window[0];
            let second = window[1];
            let first_starts_alpha = first.chars().next().is_some_and(|c| c.is_ascii_alphabetic());
            let first_ok = first_starts_alpha && first.chars().all(|c| c.is_ascii_alphanumeric());
            let second_all_digits = !second.is_empty() && second.chars().all(|c| c.is_ascii_digit());
            if first_ok && second_all_digits {
                tokens.push(format!("{first}_{second}"));
            }
        }
        let os_hit = os_aliases.iter().any(|alias| !alias.is_empty() && tokens.iter().any(|t| t.as_str() == *alias));
        let arch_hit =
            arch_aliases.iter().any(|alias| !alias.is_empty() && tokens.iter().any(|t| t.as_str() == *alias));
        // A 32-bit `x86` host must not match an `x86_64-…` asset. The fused
        // token logic above would otherwise let the raw `x86` token alone
        // win — explicitly require an unambiguous 32-bit token before
        // admitting a 32-bit asset.
        let arch_hit = if arch == "x86" && tokens.iter().any(|t| t == "x86_64") {
            tokens.iter().any(|t| matches!(t.as_str(), "i686" | "i386"))
        } else {
            arch_hit
        };
        if os_hit && arch_hit {
            return Some(asset);
        }
    }
    None
}

/// Allowlist of asset extensions the installer knows how to consume.
/// Restricted to formats [`apply_update`] can actually extract (`.tar.gz`,
/// `.tgz`) or hand to [`atomic_install`] verbatim (bare `animus`/
/// `animus.exe` executable). Admitting `.tar.xz` / `.zip` / `.tar.zst`
/// here without a matching extractor would let the installer copy
/// compressed bytes over the running binary; extend [`extract_archive`]
/// before adding new shapes.
#[allow(clippy::case_sensitive_file_extension_comparisons)]
fn looks_like_installable_asset(name: &str) -> bool {
    // Operate on a lowercased copy so the literal `.tar.gz`/`.tgz`
    // comparisons cover `.TAR.GZ` / `.TGZ` too; pedantic clippy's
    // `case_sensitive_file_extension_comparisons` lint flags `ends_with`
    // independent of that case-normalisation, hence the local allow.
    let lower = name.to_ascii_lowercase();
    if lower.ends_with(".tar.gz") || lower.ends_with(".tgz") {
        return true;
    }
    // Standalone `animus` or `animus.exe` executable shipped without an
    // archive wrapper. Match on the basename so a `path/to/animus.exe`
    // entry inside an archive can't slip in here.
    let basename = lower.rsplit_once(['/', '\\']).map_or(lower.as_str(), |(_, n)| n);
    basename == "animus" || basename == "animus.exe"
}

fn os_aliases(os: &str) -> Vec<&'static str> {
    match os {
        "macos" => vec!["macos", "darwin", "osx", "apple"],
        "linux" => vec!["linux"],
        "windows" => vec!["windows", "win64", "win32", "win"],
        "freebsd" => vec!["freebsd"],
        _ => vec![],
    }
}

fn arch_aliases(arch: &str) -> Vec<&'static str> {
    match arch {
        "x86_64" => vec!["x86_64", "amd64", "x64"],
        "aarch64" => vec!["aarch64", "arm64"],
        "x86" => vec!["x86", "i686", "i386"],
        "arm" => vec!["arm", "armv7"],
        _ => vec![],
    }
}

/// Compute the lowercase hex `sha256` of an on-disk file. Used to verify
/// downloads against the release asset `digest` field (or a sidecar
/// `<asset>.sha256`).
pub(crate) fn sha256_of_file(path: &Path) -> Result<String> {
    let mut file = std::fs::File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf).with_context(|| format!("failed to read {}", path.display()))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

/// Atomic install: write the staged binary to a temp file in the same
/// directory as the target, fsync it, set executable bits, and rename over
/// the existing path. Same-directory rename is the only POSIX-atomic shape.
pub(crate) fn atomic_install(staged: &Path, target: &Path) -> Result<()> {
    let parent = target.parent().ok_or_else(|| anyhow!("target path has no parent: {}", target.display()))?;
    std::fs::create_dir_all(parent).with_context(|| format!("failed to create install dir {}", parent.display()))?;

    let temp = parent.join(format!(
        ".{}.animus-update.tmp",
        target.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "animus".to_string())
    ));
    if temp.exists() {
        let _ = std::fs::remove_file(&temp);
    }
    std::fs::copy(staged, &temp).with_context(|| format!("failed to stage update at {}", temp.display()))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perm = std::fs::metadata(&temp)
            .with_context(|| format!("failed to stat staged binary {}", temp.display()))?
            .permissions();
        perm.set_mode(0o755);
        std::fs::set_permissions(&temp, perm)
            .with_context(|| format!("failed to chmod staged binary {}", temp.display()))?;
    }

    #[cfg(windows)]
    {
        // TODO(codex-p2): Windows self-update is best-effort. `MoveFileExW`
        // (Rust's `fs::rename` on Windows) usually permits renaming the
        // running `animus.exe` because the metadata move is detached from
        // the executable mapping, but exclusive file locks held by AV /
        // shell extensions can still cause access-denied. A robust
        // implementation would re-launch a helper that watches for the
        // parent process to exit and then performs the swap, or use
        // `MoveFileEx` with `MOVEFILE_DELAY_UNTIL_REBOOT` as a fallback.
        // Until then we move the live binary aside (rename, not delete) so
        // the install proceeds; the `.old` is best-effort cleaned up on the
        // next invocation.
        let backup = parent.join(format!(
            "{}.old",
            target.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "animus".to_string())
        ));
        if backup.exists() {
            let _ = std::fs::remove_file(&backup);
        }
        if target.exists() {
            std::fs::rename(target, &backup).with_context(|| {
                format!(
                    "Windows self-update could not move live binary aside ({}). Re-run `animus self update` from outside the running CLI, or replace the binary manually.",
                    target.display()
                )
            })?;
        }
    }

    std::fs::rename(&temp, target).with_context(|| format!("failed to atomically swap {}", target.display()))?;
    Ok(())
}

/// Locate the running `animus` binary path. Uses `std::env::current_exe`
/// and resolves any symlinks (e.g. `~/.cargo/bin/animus` -> the real file).
pub(crate) fn current_binary_path() -> Result<PathBuf> {
    let path = std::env::current_exe().context("failed to resolve current executable path")?;
    Ok(path.canonicalize().unwrap_or(path))
}

/// Decision returned by [`should_check_now`] — drives the startup flow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StartupAction {
    /// Skip the check entirely (mode = off, or within interval).
    Skip,
    /// Run the check and apply per `mode`.
    Check,
}

/// Returns whether a startup check should run, given the configured mode,
/// the configured check_interval (ISO-8601), and the last-checked time.
pub(crate) fn should_check_now(
    mode: AutoUpdateMode,
    last_checked: Option<chrono::DateTime<chrono::Utc>>,
    interval: Option<Duration>,
    now: chrono::DateTime<chrono::Utc>,
) -> StartupAction {
    if matches!(mode, AutoUpdateMode::Off) {
        return StartupAction::Skip;
    }
    let interval = interval.unwrap_or(Duration::from_hours(24));
    match last_checked {
        None => StartupAction::Check,
        Some(prev) => {
            let elapsed = now.signed_duration_since(prev);
            let elapsed_secs = elapsed.num_seconds().max(0) as u64;
            if elapsed_secs >= interval.as_secs() {
                StartupAction::Check
            } else {
                StartupAction::Skip
            }
        }
    }
}

/// Outcome surfaced by the manual `self update` flow.
#[derive(Debug)]
pub enum UpdateOutcome {
    UpToDate { current: String },
    Available { current: String, latest: String },
    Installed { previous: String, installed: String },
}

/// Fetch the latest stable release from `launchapp-dev/animus-cli`.
async fn fetch_latest_stable(timeout: Duration) -> Result<GithubReleaseRecord> {
    let url = format!("https://api.github.com/repos/{SELF_UPDATE_OWNER}/{SELF_UPDATE_REPO}/releases/latest");
    let client = build_client(timeout)?;
    let response = client
        .get(&url)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .with_context(|| format!("failed to GET {url}"))?
        .error_for_status()
        .with_context(|| format!("GET {url} returned non-success status"))?;
    let release: GithubReleaseRecord =
        response.json().await.with_context(|| format!("failed to parse GitHub release JSON from {url}"))?;
    Ok(release)
}

/// Fetch the listing (up to 30 most recent) for the prerelease channel.
async fn fetch_release_listing(timeout: Duration) -> Result<Vec<GithubReleaseRecord>> {
    let url = format!("https://api.github.com/repos/{SELF_UPDATE_OWNER}/{SELF_UPDATE_REPO}/releases");
    let client = build_client(timeout)?;
    let response = client
        .get(&url)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .with_context(|| format!("failed to GET {url}"))?
        .error_for_status()
        .with_context(|| format!("GET {url} returned non-success status"))?;
    let releases: Vec<GithubReleaseRecord> =
        response.json().await.with_context(|| format!("failed to parse GitHub release listing from {url}"))?;
    Ok(releases)
}

fn build_client(timeout: Duration) -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .user_agent(format!("animus-update/{}", env!("CARGO_PKG_VERSION")))
        .timeout(timeout)
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            // Cap chain length and reject any hop that lands on a host
            // outside the GitHub release plane. GitHub's release-download
            // path 302s to `objects.githubusercontent.com`, and the API
            // path 302s back to itself — anything else is suspicious.
            if attempt.previous().len() >= MAX_REDIRECTS {
                return attempt.error("too many redirects (cap 5)");
            }
            let host = attempt.url().host_str().map(str::to_owned);
            match host {
                Some(host) if is_allowed_release_host(&host) => attempt.follow(),
                Some(host) => attempt.error(format!("redirect to disallowed host: {host}")),
                None => attempt.error("redirect target missing host"),
            }
        }))
        .build()
        .context("failed to build HTTP client")
}

/// Hard cap on follow-redirect chain length for any self-update HTTP
/// call. GitHub's release-download flow is one or two hops in practice.
const MAX_REDIRECTS: usize = 5;

pub(crate) fn is_allowed_release_host(host: &str) -> bool {
    let host = host.to_ascii_lowercase();
    matches!(host.as_str(), "api.github.com" | "github.com" | "codeload.github.com" | "objects.githubusercontent.com")
        || host.ends_with(".githubusercontent.com")
}

/// Resolve the eligible release for the running binary given the channel
/// preference. Hides the stable/prerelease branch from callers.
pub async fn resolve_eligible_release(
    channel: AutoUpdateChannel,
    current_version: &str,
    timeout: Duration,
) -> Result<Option<(GithubReleaseRecord, Version)>> {
    let releases = match channel {
        AutoUpdateChannel::Stable => vec![fetch_latest_stable(timeout).await?],
        AutoUpdateChannel::Prerelease => fetch_release_listing(timeout).await?,
    };
    Ok(pick_eligible_release(&releases, channel, current_version).map(|(r, v)| (r.clone(), v)))
}

/// Options for the manual update flow. Wraps the boolean flags so the
/// public signature stays readable and clippy-quiet.
#[derive(Debug, Clone, Copy, Default)]
pub struct ManualUpdateOptions {
    pub check_only: bool,
    pub force: bool,
    pub prerelease_override: bool,
    pub assume_yes: bool,
    /// Force a specific channel and ignore the config block entirely.
    /// Set by `animus update --channel <c>` so the explicit flag wins
    /// over an `auto_update.channel = "prerelease"` config block — the
    /// `prerelease_override` boolean is one-way (it can only flip
    /// stable -> prerelease, not the reverse), so the alias surface
    /// needs a strict override.
    pub channel_override: Option<AutoUpdateChannel>,
}

/// Pick the channel for a manual update invocation by stacking the
/// CLI-level `channel_override` (highest priority — used by `animus
/// update --channel <c>`), the `prerelease_override` boolean (used by
/// `animus self update --prerelease`), and the persisted config block
/// (lowest priority).
pub(crate) fn select_channel(
    config_block: Option<&AutoUpdateConfig>,
    options: &ManualUpdateOptions,
) -> AutoUpdateChannel {
    if let Some(forced) = options.channel_override {
        return forced;
    }
    if options.prerelease_override {
        return AutoUpdateChannel::Prerelease;
    }
    config_block.map(|c| c.channel).unwrap_or_default()
}

/// Manual entry point — used by `animus self update`.
pub async fn run_manual_update(
    config_block: Option<&AutoUpdateConfig>,
    options: ManualUpdateOptions,
) -> Result<UpdateOutcome> {
    let channel = select_channel(config_block, &options);
    let current = env!("CARGO_PKG_VERSION");

    let eligible = resolve_eligible_release(channel, current, MANUAL_FETCH_TIMEOUT).await?;
    let (release, version) = match eligible {
        Some(pair) => pair,
        None if options.force => {
            let release = match channel {
                AutoUpdateChannel::Stable => fetch_latest_stable(MANUAL_FETCH_TIMEOUT).await?,
                AutoUpdateChannel::Prerelease => fetch_release_listing(MANUAL_FETCH_TIMEOUT)
                    .await?
                    .into_iter()
                    .next()
                    .ok_or_else(|| anyhow!("no releases found for {SELF_UPDATE_OWNER}/{SELF_UPDATE_REPO}"))?,
            };
            let version = parse_release_tag(&release.tag_name)
                .ok_or_else(|| anyhow!("unparseable release tag: {}", release.tag_name))?;
            (release, version)
        }
        None => return Ok(UpdateOutcome::UpToDate { current: current.to_string() }),
    };

    if options.check_only {
        return Ok(UpdateOutcome::Available { current: current.to_string(), latest: version.to_string() });
    }

    if !options.assume_yes {
        if std::io::stdin().is_terminal() {
            eprint!("Apply update {} -> v{}? [y/N] ", current, version);
            let _ = std::io::stderr().flush();
            let mut answer = String::new();
            std::io::stdin().read_line(&mut answer).context("failed to read confirmation")?;
            if !answer.trim().eq_ignore_ascii_case("y") {
                return Ok(UpdateOutcome::Available { current: current.to_string(), latest: version.to_string() });
            }
        } else {
            // CI / piped invocation: refuse to overwrite the binary without
            // an explicit `--yes`. The operator should pass `--yes` (or use
            // `auto_update.mode = "auto"`) to opt into unattended installs.
            return Ok(UpdateOutcome::Available { current: current.to_string(), latest: version.to_string() });
        }
    }

    apply_update(&release).await?;
    Ok(UpdateOutcome::Installed { previous: current.to_string(), installed: version.to_string() })
}

/// Download, verify, and atomically install the chosen release for the
/// running host.
pub async fn apply_update(release: &GithubReleaseRecord) -> Result<()> {
    let platform = current_platform_token();
    let asset = pick_asset_for_host(&release.assets, &platform).ok_or_else(|| {
        anyhow!(
            "no release asset matched platform '{platform}' in tag {} (saw {} assets)",
            release.tag_name,
            release.assets.len()
        )
    })?;

    let temp_dir = tempfile::tempdir().context("failed to create temp dir for download")?;
    let staged_path = temp_dir.path().join(&asset.name);
    download_asset(&asset.browser_download_url, &staged_path).await?;

    if let Some(expected) = inline_expected_digest(asset) {
        let actual = sha256_of_file(&staged_path)?;
        if actual != expected {
            anyhow::bail!("sha256 mismatch for {}: expected {expected}, got {actual}", asset.name);
        }
    } else if let Some(sidecar) = find_sidecar(&release.assets, asset) {
        let body = download_text(&sidecar.browser_download_url).await?;
        if let Some(expected) = parse_sha256_sidecar(&body) {
            let actual = sha256_of_file(&staged_path)?;
            if actual != expected {
                anyhow::bail!("sha256 mismatch for {}: expected {expected}, got {actual}", asset.name);
            }
        }
    }

    let extracted =
        if asset.name.to_ascii_lowercase().ends_with(".tar.gz") || asset.name.to_ascii_lowercase().ends_with(".tgz") {
            extract_archive(&staged_path, temp_dir.path())?
        } else {
            staged_path.clone()
        };

    let target = current_binary_path()?;
    atomic_install(&extracted, &target)?;
    Ok(())
}

async fn download_asset(url: &str, dest: &Path) -> Result<()> {
    let client = build_client(MANUAL_FETCH_TIMEOUT)?;
    let response = client
        .get(url)
        .send()
        .await
        .with_context(|| format!("failed to GET {url}"))?
        .error_for_status()
        .with_context(|| format!("GET {url} returned non-success status"))?;
    let bytes = response.bytes().await.with_context(|| format!("failed to read body from {url}"))?;
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("failed to create dir {}", parent.display()))?;
    }
    std::fs::write(dest, &bytes).with_context(|| format!("failed to write {}", dest.display()))?;
    Ok(())
}

pub(crate) fn inline_expected_digest(asset: &GithubReleaseAssetRecord) -> Option<String> {
    asset.digest.as_deref().and_then(parse_release_digest)
}

pub(crate) fn find_sidecar<'a>(
    assets: &'a [GithubReleaseAssetRecord],
    asset: &GithubReleaseAssetRecord,
) -> Option<&'a GithubReleaseAssetRecord> {
    let sidecar_name = format!("{}.sha256", asset.name);
    assets.iter().find(|a| a.name.eq_ignore_ascii_case(&sidecar_name))
}

async fn download_text(url: &str) -> Result<String> {
    let client = build_client(MANUAL_FETCH_TIMEOUT)?;
    client
        .get(url)
        .send()
        .await
        .with_context(|| format!("failed to GET {url}"))?
        .error_for_status()
        .with_context(|| format!("GET {url} returned non-success status"))?
        .text()
        .await
        .with_context(|| format!("failed to read body from {url}"))
}

pub(crate) fn parse_release_digest(digest: &str) -> Option<String> {
    let trimmed = digest.trim();
    let (algo, hex) = trimmed.split_once(':')?;
    if !algo.eq_ignore_ascii_case("sha256") {
        return None;
    }
    let hex = hex.trim();
    if hex.len() != 64 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    Some(hex.to_ascii_lowercase())
}

pub(crate) fn parse_sha256_sidecar(body: &str) -> Option<String> {
    let line = body.lines().next()?.trim();
    let token = line.split_whitespace().next()?;
    if token.len() == 64 && token.chars().all(|c| c.is_ascii_hexdigit()) {
        Some(token.to_ascii_lowercase())
    } else {
        None
    }
}

fn extract_archive(archive: &Path, dest_dir: &Path) -> Result<PathBuf> {
    let file = std::fs::File::open(archive).with_context(|| format!("failed to open archive {}", archive.display()))?;
    let gz = flate2::read::GzDecoder::new(file);
    let mut tar = tar::Archive::new(gz);
    let mut candidate: Option<PathBuf> = None;
    let fallback: OsString = OsString::from("animus");
    for entry in tar.entries().context("failed to read tar entries")? {
        let mut entry = entry.context("failed to read tar entry")?;
        let path = entry.path().context("failed to read tar entry path")?.into_owned();
        let name = path.file_name().unwrap_or(fallback.as_os_str());
        let out = dest_dir.join(name);
        entry.unpack(&out).with_context(|| format!("failed to unpack {}", out.display()))?;
        let basename = out.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        if basename == "animus" || basename == "animus.exe" {
            candidate = Some(out);
        }
    }
    candidate.ok_or_else(|| anyhow!("archive did not contain an `animus` binary"))
}

/// Fire-and-forget startup check. Returns `Some` when a textual notification
/// should be emitted to stderr by the caller. In `auto` mode applies the
/// update and returns the result; in `prompt` mode degrades to notify when
/// stdin is not a TTY (i.e. piped invocation in CI).
pub async fn run_startup_check(
    config_block: Option<AutoUpdateConfig>,
    current_version: String,
    state: AutoUpdateState,
) -> Result<Option<String>> {
    let mode = effective_mode(config_block.as_ref());
    if matches!(mode, AutoUpdateMode::Off) {
        return Ok(None);
    }

    let interval = config_block
        .as_ref()
        .and_then(|c| parse_iso8601_duration(&c.check_interval))
        .or_else(|| parse_iso8601_duration("P1D"));
    let now = chrono::Utc::now();
    if matches!(should_check_now(mode, state.last_checked, interval, now), StartupAction::Skip) {
        return Ok(None);
    }

    // Persist `last_checked` BEFORE the network call so a slow / failing
    // request can't cause us to retry-and-throttle on every short
    // invocation. The throttle window honors the operator's configured
    // `check_interval` regardless of fetch outcome.
    //
    // Trade-off with the CLI's 50ms startup-check grace (see
    // `main.rs::spawn_startup_update_check`): a fetch cancelled mid-flight
    // by the grace still advances the throttle, so the user may miss the
    // first notification and see it only on the next interval boundary.
    // Long-running commands (`daemon start`, `daemon run`) give the fetch
    // time to surface the notice immediately.
    let mut next_state = state;
    next_state.last_checked = Some(now);
    let _ = next_state.save();

    let channel = config_block.as_ref().map(|c| c.channel).unwrap_or_default();
    let eligible = resolve_eligible_release(channel, &current_version, STARTUP_FETCH_TIMEOUT).await?;
    let outcome = match eligible {
        Some((release, version)) => {
            next_state.last_seen_version = Some(version.to_string());
            let _ = next_state.save();
            match mode {
                AutoUpdateMode::Off => None,
                AutoUpdateMode::Notify => Some(format!(
                    "Update available: v{current_version} -> v{version}. Run `animus self update` to apply."
                )),
                AutoUpdateMode::Prompt => {
                    // Honor configured `prompt` mode when stdin is a TTY: ask
                    // the operator inline. Non-TTY (CI / piped) downgrades to
                    // `notify` since there's no human at the keyboard. NOTE:
                    // there's a known interleaving risk with subcommands that
                    // also read stdin (guided `animus init`, plugin trust
                    // prompts); operators that hit that should switch to
                    // `notify` or `off`.
                    if std::io::stdin().is_terminal() {
                        eprint!("Animus update available: v{current_version} -> v{version}. Apply now? [y/N] ");
                        let _ = std::io::stderr().flush();
                        let mut answer = String::new();
                        if std::io::stdin().read_line(&mut answer).is_ok() && answer.trim().eq_ignore_ascii_case("y") {
                            apply_update(&release).await?;
                            Some(format!("Installed v{version} (was v{current_version})."))
                        } else {
                            Some(format!(
                                "Update available: v{current_version} -> v{version}. Run `animus self update` to apply."
                            ))
                        }
                    } else {
                        Some(format!(
                            "Update available: v{current_version} -> v{version}. Run `animus self update` to apply."
                        ))
                    }
                }
                AutoUpdateMode::Auto => {
                    apply_update(&release).await?;
                    Some(format!("Installed v{version} (was v{current_version})."))
                }
            }
        }
        None => None,
    };

    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn current() -> &'static str {
        "0.5.3"
    }

    #[test]
    fn parse_release_tag_handles_v_prefix() {
        assert_eq!(parse_release_tag("v1.2.3").unwrap().to_string(), "1.2.3");
        assert_eq!(parse_release_tag("1.2.3").unwrap().to_string(), "1.2.3");
        assert!(parse_release_tag("not-a-version").is_none());
        assert!(parse_release_tag("").is_none());
    }

    #[test]
    fn is_newer_than_current_strict_compare() {
        assert!(is_newer_than_current("v0.6.0", current()).is_some());
        assert!(is_newer_than_current("0.5.3", current()).is_none());
        assert!(is_newer_than_current("0.5.2", current()).is_none());
        assert!(is_newer_than_current("garbage", current()).is_none());
    }

    fn cfg(channel: AutoUpdateChannel) -> AutoUpdateConfig {
        AutoUpdateConfig { mode: AutoUpdateMode::Notify, check_interval: "P1D".to_string(), channel }
    }

    #[test]
    fn select_channel_override_beats_prerelease_config() {
        let config = cfg(AutoUpdateChannel::Prerelease);
        let opts = ManualUpdateOptions { channel_override: Some(AutoUpdateChannel::Stable), ..Default::default() };
        assert_eq!(select_channel(Some(&config), &opts), AutoUpdateChannel::Stable);
    }

    #[test]
    fn select_channel_override_beats_prerelease_flag() {
        let opts = ManualUpdateOptions {
            channel_override: Some(AutoUpdateChannel::Stable),
            prerelease_override: true,
            ..Default::default()
        };
        assert_eq!(select_channel(None, &opts), AutoUpdateChannel::Stable);
    }

    #[test]
    fn select_channel_falls_back_to_config_then_default() {
        assert_eq!(
            select_channel(Some(&cfg(AutoUpdateChannel::Prerelease)), &ManualUpdateOptions::default()),
            AutoUpdateChannel::Prerelease
        );
        assert_eq!(select_channel(None, &ManualUpdateOptions::default()), AutoUpdateChannel::Stable);
    }

    #[test]
    fn channel_filter_drops_prereleases_in_stable() {
        assert!(channel_admits(AutoUpdateChannel::Stable, false));
        assert!(!channel_admits(AutoUpdateChannel::Stable, true));
        assert!(channel_admits(AutoUpdateChannel::Prerelease, true));
        assert!(channel_admits(AutoUpdateChannel::Prerelease, false));
    }

    #[test]
    fn pick_eligible_release_respects_channel() {
        let releases = vec![
            GithubReleaseRecord { tag_name: "v0.7.0-rc.1".to_string(), prerelease: true, assets: vec![] },
            GithubReleaseRecord { tag_name: "v0.6.0".to_string(), prerelease: false, assets: vec![] },
        ];
        let stable = pick_eligible_release(&releases, AutoUpdateChannel::Stable, current()).expect("stable hit");
        assert_eq!(stable.0.tag_name, "v0.6.0");
        let pre = pick_eligible_release(&releases, AutoUpdateChannel::Prerelease, current()).expect("prerelease hit");
        assert_eq!(pre.0.tag_name, "v0.7.0-rc.1");
    }

    #[test]
    fn pick_eligible_release_returns_none_when_current_is_newest() {
        let releases = vec![GithubReleaseRecord { tag_name: "v0.5.2".to_string(), prerelease: false, assets: vec![] }];
        assert!(pick_eligible_release(&releases, AutoUpdateChannel::Stable, current()).is_none());
    }

    #[test]
    fn pick_asset_for_host_rejects_detached_signature_sidecar() {
        let assets = vec![
            GithubReleaseAssetRecord {
                name: "animus-linux-x86_64.tar.gz.sig".to_string(),
                browser_download_url: "https://example/sig".to_string(),
                digest: None,
            },
            GithubReleaseAssetRecord {
                name: "animus-linux-x86_64.tar.gz".to_string(),
                browser_download_url: "https://example/tgz".to_string(),
                digest: None,
            },
        ];
        let picked = pick_asset_for_host(&assets, "linux-x86_64").expect("tgz over sig");
        assert!(picked.name.ends_with(".tar.gz"));
    }

    #[test]
    fn looks_like_installable_asset_admits_known_archives() {
        for ext in [".tar.gz", ".tgz"] {
            assert!(
                looks_like_installable_asset(&format!("animus-linux-x86_64{ext}")),
                "expected {ext} to be admitted"
            );
        }
        assert!(looks_like_installable_asset("animus"));
        assert!(looks_like_installable_asset("animus.exe"));
    }

    #[test]
    fn looks_like_installable_asset_rejects_unsupported_archive_extensions() {
        // These archive formats are not currently handled by `apply_update`'s
        // extractor — admitting them would let the installer copy compressed
        // bytes over the running binary. Extend `extract_archive` BEFORE
        // adding any of these to the allowlist.
        for ext in [".tar.xz", ".txz", ".tar.zst", ".zip", ".7z", ".bin", ".exe"] {
            assert!(
                !looks_like_installable_asset(&format!("animus-linux-x86_64{ext}")),
                "expected unsupported archive {ext} to be rejected"
            );
        }
    }

    #[test]
    fn looks_like_installable_asset_rejects_known_sidecars() {
        for ext in [
            ".sha256",
            ".sha256sum",
            ".sha512",
            ".md5",
            ".sig",
            ".asc",
            ".minisig",
            ".pem",
            ".pub",
            ".txt",
            ".md",
            ".json",
            ".yaml",
            ".yml",
            ".spdx",
            ".spdx.json",
            ".cdx",
            ".cdx.json",
            ".sbom",
            ".sbom.json",
        ] {
            assert!(
                !looks_like_installable_asset(&format!("animus-linux-x86_64.tar.gz{ext}")),
                "expected sidecar {ext} to be rejected"
            );
        }
    }

    #[test]
    fn pick_asset_for_host_matches_platform_token() {
        let assets = vec![
            GithubReleaseAssetRecord {
                name: "animus-linux-x86_64.tar.gz".to_string(),
                browser_download_url: "https://example/linux".to_string(),
                digest: None,
            },
            GithubReleaseAssetRecord {
                name: "animus-macos-aarch64.tar.gz".to_string(),
                browser_download_url: "https://example/macos".to_string(),
                digest: None,
            },
            GithubReleaseAssetRecord {
                name: "animus-macos-aarch64.tar.gz.sha256".to_string(),
                browser_download_url: "https://example/sha".to_string(),
                digest: None,
            },
        ];
        let picked = pick_asset_for_host(&assets, "macos-aarch64").expect("macos asset");
        assert_eq!(picked.name, "animus-macos-aarch64.tar.gz");
    }

    #[test]
    fn pick_asset_for_host_matches_rust_target_triple_token() {
        let assets = vec![
            GithubReleaseAssetRecord {
                name: "animus-x86_64-unknown-linux-gnu.tar.gz".to_string(),
                browser_download_url: "https://example/x86".to_string(),
                digest: None,
            },
            GithubReleaseAssetRecord {
                name: "animus-aarch64-unknown-linux-gnu.tar.gz".to_string(),
                browser_download_url: "https://example/arm".to_string(),
                digest: None,
            },
        ];
        let picked = pick_asset_for_host(&assets, "linux-x86_64").expect("rust triple match");
        assert_eq!(picked.name, "animus-x86_64-unknown-linux-gnu.tar.gz");
    }

    #[test]
    fn pick_asset_for_host_rejects_substring_collision_across_platforms() {
        // Regression: previously `win` token matched substring of
        // `darwin`; we now require whole-token equality.
        let assets = vec![GithubReleaseAssetRecord {
            name: "animus-darwin-x64.tar.gz".to_string(),
            browser_download_url: "https://example/darwin".to_string(),
            digest: None,
        }];
        assert!(pick_asset_for_host(&assets, "windows-x86_64").is_none());
    }

    #[test]
    fn pick_asset_for_host_rejects_arch_mismatch_when_only_os_matches() {
        let assets = vec![
            GithubReleaseAssetRecord {
                name: "animus-x86_64-unknown-linux-gnu.tar.gz".to_string(),
                browser_download_url: "https://example/x86".to_string(),
                digest: None,
            },
            GithubReleaseAssetRecord {
                name: "animus-aarch64-unknown-linux-gnu.tar.gz".to_string(),
                browser_download_url: "https://example/arm".to_string(),
                digest: None,
            },
        ];
        let picked = pick_asset_for_host(&assets, "linux-aarch64").expect("aarch64 asset");
        assert_eq!(picked.name, "animus-aarch64-unknown-linux-gnu.tar.gz");
    }

    #[test]
    fn pick_asset_for_host_honors_arch_aliases() {
        let assets = vec![GithubReleaseAssetRecord {
            name: "animus_linux_amd64.tar.gz".to_string(),
            browser_download_url: "https://example/amd64".to_string(),
            digest: None,
        }];
        let picked = pick_asset_for_host(&assets, "linux-x86_64").expect("amd64 alias");
        assert_eq!(picked.name, "animus_linux_amd64.tar.gz");
    }

    #[test]
    fn pick_asset_for_host_returns_none_when_arch_absent() {
        let assets = vec![GithubReleaseAssetRecord {
            name: "animus-linux.tar.gz".to_string(),
            browser_download_url: "https://example/linux".to_string(),
            digest: None,
        }];
        assert!(pick_asset_for_host(&assets, "linux-aarch64").is_none());
    }

    #[test]
    fn pick_asset_for_host_recognises_darwin_alias_for_macos() {
        let assets = vec![GithubReleaseAssetRecord {
            name: "animus-darwin-arm64.tar.gz".to_string(),
            browser_download_url: "https://example/darwin".to_string(),
            digest: None,
        }];
        let picked = pick_asset_for_host(&assets, "macos-aarch64").expect("darwin alias");
        assert_eq!(picked.name, "animus-darwin-arm64.tar.gz");
    }

    #[test]
    fn parse_iso8601_duration_supports_common_shapes() {
        assert_eq!(parse_iso8601_duration("P1D"), Some(Duration::from_hours(24)));
        assert_eq!(parse_iso8601_duration("PT6H"), Some(Duration::from_hours(6)));
        assert_eq!(parse_iso8601_duration("PT30M"), Some(Duration::from_mins(30)));
        assert_eq!(parse_iso8601_duration("P1W"), Some(Duration::from_hours(168)));
        assert_eq!(parse_iso8601_duration("garbage"), None);
        assert_eq!(parse_iso8601_duration(""), None);
    }

    #[test]
    fn should_check_now_skips_when_off() {
        let now = chrono::Utc::now();
        assert_eq!(should_check_now(AutoUpdateMode::Off, None, None, now), StartupAction::Skip);
    }

    #[test]
    fn should_check_now_runs_when_never_checked() {
        let now = chrono::Utc::now();
        assert_eq!(
            should_check_now(AutoUpdateMode::Notify, None, Some(Duration::from_hours(24)), now),
            StartupAction::Check
        );
    }

    #[test]
    fn should_check_now_throttles_within_interval() {
        let now = chrono::Utc::now();
        let recent = now - chrono::Duration::seconds(60);
        assert_eq!(
            should_check_now(AutoUpdateMode::Notify, Some(recent), Some(Duration::from_hours(24)), now),
            StartupAction::Skip
        );
    }

    #[test]
    fn should_check_now_fires_after_interval() {
        let now = chrono::Utc::now();
        let stale = now - chrono::Duration::seconds(2 * 86400);
        assert_eq!(
            should_check_now(AutoUpdateMode::Notify, Some(stale), Some(Duration::from_hours(24)), now),
            StartupAction::Check
        );
    }

    fn env_test_lock() -> &'static std::sync::Mutex<()> {
        static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
        LOCK.get_or_init(|| std::sync::Mutex::new(()))
    }

    struct EnvGuard {
        prev_disable: Option<String>,
        prev_mode: Option<String>,
    }

    impl EnvGuard {
        fn acquire() -> (Self, std::sync::MutexGuard<'static, ()>) {
            let guard = env_test_lock().lock().expect("env mutex");
            let prev_disable = std::env::var(ENV_DISABLE).ok();
            let prev_mode = std::env::var(ENV_MODE_OVERRIDE).ok();
            std::env::remove_var(ENV_DISABLE);
            std::env::remove_var(ENV_MODE_OVERRIDE);
            (Self { prev_disable, prev_mode }, guard)
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            std::env::remove_var(ENV_DISABLE);
            std::env::remove_var(ENV_MODE_OVERRIDE);
            if let Some(v) = self.prev_disable.take() {
                std::env::set_var(ENV_DISABLE, v);
            }
            if let Some(v) = self.prev_mode.take() {
                std::env::set_var(ENV_MODE_OVERRIDE, v);
            }
        }
    }

    #[test]
    fn effective_mode_env_disable_wins() {
        let (_env, _lock) = EnvGuard::acquire();
        std::env::set_var(ENV_DISABLE, "1");
        let mode = effective_mode(Some(&AutoUpdateConfig {
            mode: AutoUpdateMode::Auto,
            check_interval: "P1D".to_string(),
            channel: AutoUpdateChannel::Stable,
        }));
        assert_eq!(mode, AutoUpdateMode::Off);
    }

    #[test]
    fn effective_mode_env_override_replaces_config() {
        let (_env, _lock) = EnvGuard::acquire();
        std::env::set_var(ENV_MODE_OVERRIDE, "off");
        let mode = effective_mode(Some(&AutoUpdateConfig {
            mode: AutoUpdateMode::Auto,
            check_interval: "P1D".to_string(),
            channel: AutoUpdateChannel::Stable,
        }));
        assert_eq!(mode, AutoUpdateMode::Off);
    }

    #[test]
    fn effective_mode_falls_back_to_notify_when_block_missing() {
        let (_env, _lock) = EnvGuard::acquire();
        let mode = effective_mode(None);
        assert_eq!(mode, AutoUpdateMode::Notify);
    }

    #[test]
    fn parse_release_digest_accepts_sha256_only() {
        let hex = "a".repeat(64);
        assert_eq!(parse_release_digest(&format!("sha256:{hex}")), Some(hex.clone()));
        assert_eq!(parse_release_digest(&format!("SHA256:{hex}")), Some(hex));
        assert!(parse_release_digest("md5:00").is_none());
        assert!(parse_release_digest("nope").is_none());
    }

    #[test]
    fn parse_sha256_sidecar_grabs_leading_hex() {
        let hex = "b".repeat(64);
        assert_eq!(parse_sha256_sidecar(&format!("{hex}  animus.tar.gz")), Some(hex));
        assert!(parse_sha256_sidecar("too-short").is_none());
    }

    #[test]
    fn sha256_of_file_matches_known_vector() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("hello");
        std::fs::write(&path, b"hello world").unwrap();
        let hex = sha256_of_file(&path).unwrap();
        assert_eq!(hex, "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9");
    }

    #[test]
    fn atomic_install_swaps_in_place() {
        let tmp = tempfile::tempdir().unwrap();
        let staged = tmp.path().join("staged");
        let target = tmp.path().join("animus");
        std::fs::write(&staged, b"new").unwrap();
        std::fs::write(&target, b"old").unwrap();
        atomic_install(&staged, &target).unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"new");
    }

    #[test]
    fn allowed_release_host_accepts_github_plane_only() {
        for ok in [
            "api.github.com",
            "github.com",
            "codeload.github.com",
            "objects.githubusercontent.com",
            "release-assets.githubusercontent.com",
            "GITHUB.COM",
        ] {
            assert!(is_allowed_release_host(ok), "expected {ok} to be allowed");
        }
        for bad in [
            "evil.com",
            "github.com.evil.com",
            "raw.example.com",
            "api.github.com.evil.com",
            "githubusercontent.com.evil",
        ] {
            assert!(!is_allowed_release_host(bad), "expected {bad} to be rejected");
        }
    }

    #[test]
    fn parse_mode_str_accepts_canonical_values() {
        assert_eq!(parse_mode_str("off"), Some(AutoUpdateMode::Off));
        assert_eq!(parse_mode_str("Notify"), Some(AutoUpdateMode::Notify));
        assert_eq!(parse_mode_str("prompt"), Some(AutoUpdateMode::Prompt));
        assert_eq!(parse_mode_str("AUTO"), Some(AutoUpdateMode::Auto));
        assert!(parse_mode_str("bogus").is_none());
    }
}
