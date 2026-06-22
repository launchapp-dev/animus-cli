use crate::{invalid_input_error, not_found_error, unavailable_error};
use anyhow::{Context, Result};
use orchestrator_config::skill_scoping::{import_skill_definition, DetectedSkillFormat};
use orchestrator_config::SkillDefinition;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// A parsed GitHub skill source: owner/repo, an optional git ref (branch, tag,
/// or sha), and an optional in-repo subpath (a directory of skills, a single
/// skill folder, or a `SKILL.md` file).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GithubSkillSource {
    pub(crate) owner: String,
    pub(crate) repo: String,
    pub(crate) git_ref: Option<String>,
    pub(crate) subpath: Option<String>,
}

fn clean_segment(value: &str) -> &str {
    value.trim().trim_matches('/')
}

fn strip_dot_git(repo: &str) -> &str {
    repo.strip_suffix(".git").unwrap_or(repo)
}

fn normalize_subpath(raw: &str) -> Option<String> {
    let trimmed = clean_segment(raw);
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Parse the many accepted GitHub source shapes into a [`GithubSkillSource`]:
///
/// - `OWNER/REPO` and `OWNER/REPO@REF`
/// - `https://github.com/OWNER/REPO[.git]` (optionally `@REF`)
/// - `https://github.com/OWNER/REPO/tree/REF[/SUBPATH]`
/// - `https://github.com/OWNER/REPO/blob/REF/PATH/SKILL.md`
/// - raw `https://raw.githubusercontent.com/OWNER/REPO/REF/PATH/SKILL.md`
pub(crate) fn parse_github_skill_source(raw: &str) -> Result<GithubSkillSource> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(invalid_input_error("GitHub skill source must not be empty"));
    }

    let lower = trimmed.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") {
        return parse_github_url(trimmed);
    }

    parse_owner_repo_slug(trimmed)
}

/// Parse `OWNER/REPO` or `OWNER/REPO@REF` (no scheme).
fn parse_owner_repo_slug(trimmed: &str) -> Result<GithubSkillSource> {
    let (slug, git_ref) = split_ref(trimmed);
    let mut parts = slug.split('/');
    let owner = parts.next().map(clean_segment).unwrap_or_default();
    let repo = parts.next().map(clean_segment).map(strip_dot_git).unwrap_or_default();
    if owner.is_empty() || repo.is_empty() || parts.next().is_some() {
        return Err(invalid_input_error(format!(
            "GitHub skill source '{trimmed}' must be 'OWNER/REPO[@ref]' or a github.com URL"
        )));
    }
    Ok(GithubSkillSource { owner: owner.to_string(), repo: repo.to_string(), git_ref, subpath: None })
}

/// Split a trailing `@ref` off a slug (URLs use path components for the ref, so
/// this only applies to the bare-slug form).
fn split_ref(value: &str) -> (&str, Option<String>) {
    match value.rsplit_once('@') {
        Some((slug, git_ref)) => {
            let git_ref = git_ref.trim();
            if git_ref.is_empty() {
                (value, None)
            } else {
                (slug.trim(), Some(git_ref.to_string()))
            }
        }
        None => (value, None),
    }
}

fn parse_github_url(url: &str) -> Result<GithubSkillSource> {
    let (url, slug_ref) = match url.rsplit_once('@') {
        // Only treat a trailing `@ref` as a ref when the remainder still looks
        // like a github URL (so emails inside a path do not trip this).
        Some((head, git_ref)) if !git_ref.contains('/') && !git_ref.is_empty() => (head, Some(git_ref.to_string())),
        _ => (url, None),
    };

    let without_scheme = url.split_once("://").map(|(_, rest)| rest).unwrap_or(url);
    let (host, path) = without_scheme
        .split_once('/')
        .ok_or_else(|| invalid_input_error(format!("GitHub skill source URL '{url}' has no path")))?;
    let host = host.to_ascii_lowercase();
    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();

    if host == "raw.githubusercontent.com" {
        // raw.githubusercontent.com/OWNER/REPO/REF/PATH...
        if segments.len() < 3 {
            return Err(invalid_input_error(format!("raw GitHub URL '{url}' must include OWNER/REPO/REF/PATH")));
        }
        let owner = clean_segment(segments[0]);
        let repo = strip_dot_git(clean_segment(segments[1]));
        let git_ref = clean_segment(segments[2]);
        let subpath = normalize_subpath(&segments[3..].join("/"));
        return Ok(GithubSkillSource {
            owner: owner.to_string(),
            repo: repo.to_string(),
            git_ref: Some(git_ref.to_string()),
            subpath,
        });
    }

    if host != "github.com" && host != "www.github.com" {
        return Err(invalid_input_error(format!(
            "unsupported host '{host}' (expected github.com or raw.githubusercontent.com)"
        )));
    }

    if segments.len() < 2 {
        return Err(invalid_input_error(format!("GitHub URL '{url}' must include OWNER/REPO")));
    }
    let owner = clean_segment(segments[0]).to_string();
    let repo = strip_dot_git(clean_segment(segments[1])).to_string();

    // github.com/OWNER/REPO[/tree|/blob/REF[/SUBPATH]]
    //
    // A `/tree/<ref>/<path>` URL is inherently ambiguous when the branch name
    // itself contains `/` (e.g. `feature/foo`): the URL string alone cannot
    // tell where the ref ends and the path begins. We treat the first segment
    // after `tree`/`blob` as the ref; users with slash-bearing branches should
    // use the unambiguous `OWNER/REPO@feature/foo` slug form (handled by
    // `split_ref`, which splits on the last `@`).
    let (git_ref, subpath) = match segments.get(2).copied() {
        Some("tree") | Some("blob") => {
            let git_ref = segments.get(3).map(|s| clean_segment(s).to_string());
            let subpath = normalize_subpath(&segments[4.min(segments.len())..].join("/"));
            (git_ref, subpath)
        }
        Some(_) => {
            return Err(invalid_input_error(format!(
                "GitHub URL '{url}' is not understood (use /tree/<ref>/<path> or /blob/<ref>/<path>)"
            )));
        }
        None => (slug_ref.clone(), None),
    };

    let git_ref = git_ref.or(slug_ref);
    Ok(GithubSkillSource { owner, repo, git_ref, subpath })
}

fn release_user_agent() -> String {
    format!("animus-cli/{}", env!("CARGO_PKG_VERSION"))
}

/// Abstraction over GitHub content access so the discovery + import logic is
/// unit-testable without the network. The real implementation talks to the
/// GitHub git-trees + raw-content endpoints; tests inject a fake.
pub(crate) trait RepoFetcher {
    /// Resolve the default branch when no ref was supplied.
    fn default_branch(&self, owner: &str, repo: &str) -> Result<String>;
    /// Resolve a branch/tag/sha to a commit sha. Resolving up front lets the
    /// tree + raw-content URLs use an unambiguous single-segment sha, so refs
    /// that contain `/` (e.g. `feature/foo`) work correctly.
    fn resolve_ref_to_sha(&self, owner: &str, repo: &str, git_ref: &str) -> Result<String>;
    /// List every file path under the repo at `commit_sha` (recursive tree).
    fn list_tree(&self, owner: &str, repo: &str, commit_sha: &str) -> Result<Vec<String>>;
    /// Download a single repo-relative file's raw bytes at `commit_sha`.
    fn fetch_file(&self, owner: &str, repo: &str, commit_sha: &str, path: &str) -> Result<Vec<u8>>;
}

/// One discovered, normalized skill ready to be materialized: its assets keyed
/// by the path relative to the skill's own root directory.
#[derive(Debug, Clone)]
pub(crate) struct DiscoveredSkill {
    pub(crate) name: String,
    pub(crate) format: DetectedSkillFormat,
    /// The normalized animus skill definition (for the registry snapshot).
    pub(crate) definition: SkillDefinition,
    /// Repo-relative path of the SKILL.md, for provenance/errors.
    pub(crate) skill_md_repo_path: String,
    /// Files keyed by path relative to the skill's root (the dir holding
    /// SKILL.md). Always includes `SKILL.md`.
    pub(crate) files: BTreeMap<String, Vec<u8>>,
}

/// Identify every `SKILL.md` in `tree` that lives at or under `subpath`
/// (`None` = whole repo). Returns repo-relative SKILL.md paths, sorted.
pub(crate) fn discover_skill_md_paths(tree: &[String], subpath: Option<&str>) -> Vec<String> {
    let prefix = subpath.map(clean_segment).filter(|s| !s.is_empty());

    // A subpath that points directly at a SKILL.md selects exactly that file.
    if let Some(prefix) = prefix {
        if prefix.eq_ignore_ascii_case("SKILL.md") || prefix.to_ascii_lowercase().ends_with("/skill.md") {
            return tree.iter().filter(|p| p.as_str() == prefix).cloned().collect();
        }
    }

    let mut found: Vec<String> = tree
        .iter()
        .filter(|path| {
            let is_skill_md =
                path.rsplit('/').next().map(|name| name.eq_ignore_ascii_case("SKILL.md")).unwrap_or(false);
            if !is_skill_md {
                return false;
            }
            match prefix {
                None => true,
                Some(prefix) => path.as_str() == prefix || path.starts_with(&format!("{prefix}/")),
            }
        })
        .cloned()
        .collect();
    found.sort();
    found.dedup();
    found
}

/// The repo-relative directory that holds a SKILL.md (`""` when at repo root).
fn skill_root_dir(skill_md_path: &str) -> &str {
    match skill_md_path.rsplit_once('/') {
        Some((dir, _)) => dir,
        None => "",
    }
}

/// Fetch + normalize all skills selected by `source` using `fetcher`. Pure
/// orchestration over the fetcher trait — no direct network calls — so it is
/// fully unit-testable.
pub(crate) fn discover_and_import(
    source: &GithubSkillSource,
    fetcher: &dyn RepoFetcher,
) -> Result<Vec<DiscoveredSkill>> {
    let git_ref = match &source.git_ref {
        Some(git_ref) => git_ref.clone(),
        None => fetcher.default_branch(&source.owner, &source.repo)?,
    };
    // Resolve to a commit sha so slash-bearing branch refs (e.g.
    // `feature/foo`) survive URL interpolation unambiguously.
    let commit_sha = fetcher.resolve_ref_to_sha(&source.owner, &source.repo, &git_ref)?;

    // Fast path: a source that points directly at a SKILL.md (e.g. a raw or
    // blob URL) fetches that one file without enumerating the whole repo tree,
    // so it works even in large monorepos where the recursive tree is
    // truncated. Bundled assets are not enumerable without a tree, so a direct
    // file import installs the SKILL.md alone.
    if let Some(skill_md_path) = source.subpath.as_deref().filter(|p| points_at_skill_md(p)) {
        let bytes = fetcher.fetch_file(&source.owner, &source.repo, &commit_sha, skill_md_path)?;
        let content =
            String::from_utf8(bytes).with_context(|| format!("SKILL.md at {skill_md_path} is not valid UTF-8"))?;
        return Ok(vec![import_one(&content, skill_md_path)?]);
    }

    // TODO(codex-p2): for a `/tree/<ref>/<subpath>` source in a very large
    // monorepo, GitHub may mark the recursive tree `truncated` and the install
    // fails before the subpath filter narrows it. A scoped (non-recursive,
    // per-directory) tree walk rooted at `subpath` would let narrow installs
    // succeed where the whole-repo tree is truncated. Deferred: needs a new
    // per-directory enumeration path on the fetcher.
    let tree = fetcher.list_tree(&source.owner, &source.repo, &commit_sha)?;
    let skill_md_paths = discover_skill_md_paths(&tree, source.subpath.as_deref());

    if skill_md_paths.is_empty() {
        return Err(not_found_error(format!(
            "no SKILL.md found in {}/{}@{}{}",
            source.owner,
            source.repo,
            git_ref,
            source.subpath.as_deref().map(|p| format!(" under '{p}'")).unwrap_or_default()
        )));
    }

    // When several skills are discovered, a root-level SKILL.md must not vacuum
    // the sibling skill folders. Collect the non-root skill directories so a
    // root skill can exclude them from its asset sweep.
    let other_skill_roots: Vec<String> =
        skill_md_paths.iter().map(|path| skill_root_dir(path).to_string()).filter(|root| !root.is_empty()).collect();

    let mut discovered = Vec::new();
    for skill_md_path in &skill_md_paths {
        let root = skill_root_dir(skill_md_path).to_string();
        // Every tree entry under the skill's root is a bundled asset
        // (scripts/, references/, ...). Preserve the layout relative to root.
        // A root-level SKILL.md treats the whole repo as its bundle, minus any
        // sibling skill directories.
        let asset_paths: Vec<String> = tree
            .iter()
            .filter(|path| {
                if root.is_empty() {
                    path.as_str() == skill_md_path.as_str()
                        || !other_skill_roots.iter().any(|other| path.starts_with(&format!("{other}/")))
                } else {
                    path.as_str() == skill_md_path.as_str() || path.starts_with(&format!("{root}/"))
                }
            })
            .cloned()
            .collect();

        let mut asset_bytes: BTreeMap<String, Vec<u8>> = BTreeMap::new();
        let mut skill_md_bytes: Option<Vec<u8>> = None;
        for asset in &asset_paths {
            let bytes = fetcher.fetch_file(&source.owner, &source.repo, &commit_sha, asset)?;
            if asset == skill_md_path {
                skill_md_bytes = Some(bytes.clone());
                continue;
            }
            let relative = if root.is_empty() {
                asset.clone()
            } else {
                asset.strip_prefix(&format!("{root}/")).unwrap_or(asset).to_string()
            };
            asset_bytes.insert(relative, bytes);
        }

        let skill_md_bytes = skill_md_bytes.context("internal: SKILL.md was discovered but not fetched")?;
        let content = String::from_utf8(skill_md_bytes)
            .with_context(|| format!("SKILL.md at {skill_md_path} is not valid UTF-8"))?;
        let mut skill = import_one(&content, skill_md_path)?;
        skill.files.extend(asset_bytes);
        discovered.push(skill);
    }

    // Two skills in the same repo whose names normalize to the same slug would
    // materialize into the same staging folder and collapse under one registry
    // entry — silently dropping one. Detect and report instead of corrupting.
    let mut seen: BTreeMap<&str, &str> = BTreeMap::new();
    for skill in &discovered {
        if let Some(prior) = seen.insert(skill.name.as_str(), skill.skill_md_repo_path.as_str()) {
            return Err(invalid_input_error(format!(
                "skill name collision: '{}' and '{}' both normalize to '{}'; install one at a time with a narrower /tree/<ref>/<path> source",
                prior, skill.skill_md_repo_path, skill.name
            )));
        }
    }

    Ok(discovered)
}

/// Does `subpath` point directly at a `SKILL.md` file (case-insensitive)?
fn points_at_skill_md(subpath: &str) -> bool {
    subpath.rsplit('/').next().map(|name| name.eq_ignore_ascii_case("SKILL.md")).unwrap_or(false)
}

/// Import one SKILL.md's content into a [`DiscoveredSkill`] with only its
/// (name-normalized) SKILL.md staged. The skill name comes from untrusted
/// frontmatter, so it is forced to a safe single-segment slug AND the staged
/// SKILL.md's `name:` is rewritten to match, so the reparse on install yields
/// the same slug and the install destination can never escape the skills dir.
fn import_one(content: &str, skill_md_path: &str) -> Result<DiscoveredSkill> {
    let default_name = default_name_for(skill_md_path);
    let (mut definition, format) =
        import_skill_definition(content, &default_name).with_context(|| format!("failed to import {skill_md_path}"))?;
    let slug = slugify_skill_name(&definition.name);
    definition.name = slug.clone();
    let rewritten = rewrite_skill_md_name(content, &slug);
    let mut files = BTreeMap::new();
    files.insert("SKILL.md".to_string(), rewritten.into_bytes());
    Ok(DiscoveredSkill { name: slug, format, definition, skill_md_repo_path: skill_md_path.to_string(), files })
}

/// Reduce an imported skill name to a safe single-segment slug (lowercase ASCII
/// alphanumerics plus `-`/`_`). Shared by the staging-folder name and the
/// rewritten `name:` frontmatter so the install destination can never escape
/// the skills directory.
fn slugify_skill_name(name: &str) -> String {
    let lowered: String = name
        .trim()
        .to_ascii_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' })
        .collect();
    let collapsed = lowered.trim_matches('-').to_string();
    if collapsed.is_empty() {
        "skill".to_string()
    } else {
        collapsed
    }
}

/// Rewrite (or insert) the `name:` field in a SKILL.md's YAML frontmatter so it
/// equals `slug`. Preserves the body verbatim. When the file has no
/// frontmatter, one is prepended.
fn rewrite_skill_md_name(content: &str, slug: &str) -> String {
    let normalized = content.replace("\r\n", "\n");
    let trimmed = normalized.trim_start_matches('\u{feff}');
    let name_line = format!("name: {slug}");

    let Some(rest) = trimmed.strip_prefix("---\n") else {
        return format!("---\n{name_line}\n---\n\n{}", trimmed.trim_start());
    };
    let Some(end) = rest.find("\n---\n") else {
        // Frontmatter-only document; rewrite within the block.
        let block = rest.strip_suffix("\n---").unwrap_or(rest);
        let new_block = replace_or_prepend_name(block, &name_line);
        return format!("---\n{new_block}\n---\n");
    };
    let block = &rest[..end];
    let body = &rest[end + 5..];
    let new_block = replace_or_prepend_name(block, &name_line);
    format!("---\n{new_block}\n---\n{body}")
}

fn replace_or_prepend_name(block: &str, name_line: &str) -> String {
    let mut replaced = false;
    let mut lines: Vec<String> = block
        .lines()
        .map(|line| {
            // Only rewrite a top-level (non-indented) `name:` key.
            if !replaced && !line.starts_with(char::is_whitespace) {
                let key = line.split(':').next().unwrap_or("").trim();
                if key == "name" {
                    replaced = true;
                    return name_line.to_string();
                }
            }
            line.to_string()
        })
        .collect();
    if !replaced {
        lines.insert(0, name_line.to_string());
    }
    lines.join("\n")
}

/// Derive a fallback skill name from a SKILL.md repo path: the holding
/// directory name, or the repo root marker otherwise.
fn default_name_for(skill_md_path: &str) -> String {
    let root = skill_root_dir(skill_md_path);
    let candidate = root.rsplit('/').next().unwrap_or("");
    if candidate.trim().is_empty() {
        "skill".to_string()
    } else {
        candidate.to_string()
    }
}

/// Materialize `skills` into `dest_dir` as `<name>/SKILL.md` (+ assets),
/// producing one folder per skill that `install_local_markdown_skills` can
/// then install through the same path that `--path` uses.
pub(crate) fn materialize_to_dir(skills: &[DiscoveredSkill], dest_dir: &Path) -> Result<Vec<PathBuf>> {
    let mut roots = Vec::new();
    for skill in skills {
        let safe_name = sanitize_dir_name(&skill.name);
        let root = dest_dir.join(&safe_name);
        for (relative, bytes) in &skill.files {
            let target = root.join(relative);
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent).with_context(|| format!("failed to create {}", parent.display()))?;
            }
            std::fs::write(&target, bytes).with_context(|| format!("failed to write {}", target.display()))?;
        }
        roots.push(root);
    }
    Ok(roots)
}

/// Reduce an imported skill name to a single safe path segment so a malicious
/// `name:` frontmatter can never escape the staging directory.
fn sanitize_dir_name(name: &str) -> String {
    let cleaned: String =
        name.trim().chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' }).collect();
    let cleaned = cleaned.trim_matches('-').to_string();
    if cleaned.is_empty() {
        "skill".to_string()
    } else {
        cleaned
    }
}

// ---------------------------------------------------------------------------
// Real network fetcher (GitHub git-trees + raw content).
// ---------------------------------------------------------------------------

pub(crate) struct HttpRepoFetcher {
    client: reqwest::blocking::Client,
    token: Option<String>,
}

impl HttpRepoFetcher {
    pub(crate) fn new() -> Result<Self> {
        let client = reqwest::blocking::Client::builder()
            .user_agent(release_user_agent())
            .build()
            .context("failed to build HTTP client")?;
        let token = std::env::var("GITHUB_TOKEN").ok().filter(|value| !value.trim().is_empty());
        Ok(Self { client, token })
    }

    fn get(&self, url: &str, accept: &str) -> Result<reqwest::blocking::Response> {
        let mut request = self.client.get(url).header("Accept", accept);
        if let Some(token) = &self.token {
            request = request.header("Authorization", format!("Bearer {token}"));
        }
        let response = request.send().map_err(|err| unavailable_error(format!("failed to GET {url}: {err}")))?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(not_found_error(format!("not found: {url}")));
        }
        response
            .error_for_status()
            .map_err(|err| unavailable_error(format!("GET {url} returned non-success status: {err}")))
    }
}

#[derive(serde::Deserialize)]
struct RepoInfo {
    default_branch: String,
}

#[derive(serde::Deserialize)]
struct GitTree {
    tree: Vec<GitTreeEntry>,
    #[serde(default)]
    truncated: bool,
}

#[derive(serde::Deserialize)]
struct GitTreeEntry {
    path: String,
    #[serde(rename = "type")]
    kind: String,
}

#[derive(serde::Deserialize)]
struct CommitRef {
    sha: String,
}

impl RepoFetcher for HttpRepoFetcher {
    fn default_branch(&self, owner: &str, repo: &str) -> Result<String> {
        let url = format!("https://api.github.com/repos/{owner}/{repo}");
        let info: RepoInfo = self
            .get(&url, "application/vnd.github+json")?
            .json()
            .with_context(|| format!("failed to parse repo info from {url}"))?;
        Ok(info.default_branch)
    }

    fn resolve_ref_to_sha(&self, owner: &str, repo: &str, git_ref: &str) -> Result<String> {
        // The commits endpoint resolves a branch, tag, or sha to its commit and
        // takes the ref as a single path segment, so a slash-bearing branch
        // must be percent-encoded.
        let encoded = encode_path_segment(git_ref);
        let url = format!("https://api.github.com/repos/{owner}/{repo}/commits/{encoded}");
        let commit: CommitRef = self
            .get(&url, "application/vnd.github+json")?
            .json()
            .with_context(|| format!("failed to resolve ref '{git_ref}' via {url}"))?;
        Ok(commit.sha)
    }

    fn list_tree(&self, owner: &str, repo: &str, commit_sha: &str) -> Result<Vec<String>> {
        let url = format!("https://api.github.com/repos/{owner}/{repo}/git/trees/{commit_sha}?recursive=1");
        let tree: GitTree = self
            .get(&url, "application/vnd.github+json")?
            .json()
            .with_context(|| format!("failed to parse git tree from {url}"))?;
        if tree.truncated {
            return Err(unavailable_error(format!(
                "repo tree at {owner}/{repo}@{commit_sha} is too large to enumerate (GitHub truncated the response); pass a narrower /tree/<ref>/<path> URL"
            )));
        }
        Ok(tree.tree.into_iter().filter(|entry| entry.kind == "blob").map(|entry| entry.path).collect())
    }

    fn fetch_file(&self, owner: &str, repo: &str, commit_sha: &str, path: &str) -> Result<Vec<u8>> {
        // `commit_sha` is a resolved 40-hex sha (single path segment); the file
        // path's `/` separators are intended path structure, not encoded.
        let url = format!("https://raw.githubusercontent.com/{owner}/{repo}/{commit_sha}/{path}");
        let bytes = self
            .get(&url, "application/octet-stream")?
            .bytes()
            .with_context(|| format!("failed to read body from {url}"))?;
        Ok(bytes.to_vec())
    }
}

/// Percent-encode a single URL path segment (encodes `/` and other reserved
/// characters) so a slash-bearing git ref stays one segment.
fn encode_path_segment(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(byte as char),
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_owner_repo_slug_forms() {
        let s = parse_github_skill_source("anthropics/skills").unwrap();
        assert_eq!(
            s,
            GithubSkillSource { owner: "anthropics".into(), repo: "skills".into(), git_ref: None, subpath: None }
        );

        let s = parse_github_skill_source("anthropics/skills@v1.2.0").unwrap();
        assert_eq!(s.git_ref.as_deref(), Some("v1.2.0"));
        assert_eq!(s.subpath, None);
    }

    #[test]
    fn parse_slug_with_slash_bearing_ref() {
        // The slug `@` form unambiguously delimits a slash-bearing branch ref.
        let s = parse_github_skill_source("owner/repo@feature/foo").unwrap();
        assert_eq!(s.owner, "owner");
        assert_eq!(s.repo, "repo");
        assert_eq!(s.git_ref.as_deref(), Some("feature/foo"));
        assert_eq!(s.subpath, None);
    }

    #[test]
    fn parse_repo_url_with_and_without_dot_git() {
        let s = parse_github_skill_source("https://github.com/anthropics/skills.git").unwrap();
        assert_eq!(s.owner, "anthropics");
        assert_eq!(s.repo, "skills");
        assert_eq!(s.subpath, None);

        let s = parse_github_skill_source("https://github.com/anthropics/skills").unwrap();
        assert_eq!(s.repo, "skills");
    }

    #[test]
    fn parse_tree_url_with_ref_and_subpath() {
        let s =
            parse_github_skill_source("https://github.com/anthropics/skills/tree/main/document-skills/pdf").unwrap();
        assert_eq!(s.owner, "anthropics");
        assert_eq!(s.repo, "skills");
        assert_eq!(s.git_ref.as_deref(), Some("main"));
        assert_eq!(s.subpath.as_deref(), Some("document-skills/pdf"));
    }

    #[test]
    fn parse_blob_url_to_skill_md() {
        let s = parse_github_skill_source("https://github.com/owner/repo/blob/abc123/skills/foo/SKILL.md").unwrap();
        assert_eq!(s.git_ref.as_deref(), Some("abc123"));
        assert_eq!(s.subpath.as_deref(), Some("skills/foo/SKILL.md"));
    }

    #[test]
    fn parse_raw_skill_md_url() {
        let s =
            parse_github_skill_source("https://raw.githubusercontent.com/owner/repo/main/skills/foo/SKILL.md").unwrap();
        assert_eq!(s.owner, "owner");
        assert_eq!(s.repo, "repo");
        assert_eq!(s.git_ref.as_deref(), Some("main"));
        assert_eq!(s.subpath.as_deref(), Some("skills/foo/SKILL.md"));
    }

    #[test]
    fn parse_rejects_garbage_and_non_github_hosts() {
        assert!(parse_github_skill_source("").is_err());
        assert!(parse_github_skill_source("not-a-slug").is_err());
        assert!(parse_github_skill_source("a/b/c").is_err());
        assert!(parse_github_skill_source("https://gitlab.com/a/b").is_err());
    }

    #[test]
    fn discover_finds_single_skill_md_at_subpath() {
        let tree = vec![
            "README.md".to_string(),
            "skills/foo/SKILL.md".to_string(),
            "skills/foo/scripts/run.sh".to_string(),
            "skills/bar/SKILL.md".to_string(),
        ];
        let found = discover_skill_md_paths(&tree, Some("skills/foo"));
        assert_eq!(found, vec!["skills/foo/SKILL.md".to_string()]);
    }

    #[test]
    fn discover_finds_all_skill_md_in_repo() {
        let tree =
            vec!["skills/foo/SKILL.md".to_string(), "skills/bar/SKILL.md".to_string(), "docs/guide.md".to_string()];
        let mut found = discover_skill_md_paths(&tree, None);
        found.sort();
        assert_eq!(found, vec!["skills/bar/SKILL.md".to_string(), "skills/foo/SKILL.md".to_string()]);
    }

    #[test]
    fn discover_subpath_directly_to_skill_md() {
        let tree = vec!["skills/foo/SKILL.md".to_string()];
        let found = discover_skill_md_paths(&tree, Some("skills/foo/SKILL.md"));
        assert_eq!(found, vec!["skills/foo/SKILL.md".to_string()]);
    }

    struct FakeFetcher {
        files: BTreeMap<String, Vec<u8>>,
        default_branch: String,
    }

    impl RepoFetcher for FakeFetcher {
        fn default_branch(&self, _owner: &str, _repo: &str) -> Result<String> {
            Ok(self.default_branch.clone())
        }
        fn resolve_ref_to_sha(&self, _owner: &str, _repo: &str, git_ref: &str) -> Result<String> {
            Ok(format!("sha-for-{git_ref}"))
        }
        fn list_tree(&self, _owner: &str, _repo: &str, _git_ref: &str) -> Result<Vec<String>> {
            Ok(self.files.keys().cloned().collect())
        }
        fn fetch_file(&self, _owner: &str, _repo: &str, _git_ref: &str, path: &str) -> Result<Vec<u8>> {
            self.files.get(path).cloned().ok_or_else(|| not_found_error(format!("fake: {path}")))
        }
    }

    #[test]
    fn discover_and_import_maps_anthropic_skill_with_assets() {
        let mut files = BTreeMap::new();
        files.insert(
            "skills/pdf/SKILL.md".to_string(),
            b"---\nname: pdf\ndescription: Work with PDFs\nallowed-tools:\n  - Read\n---\n\nExtract text.\n".to_vec(),
        );
        files.insert("skills/pdf/scripts/run.sh".to_string(), b"echo hi\n".to_vec());
        files.insert("README.md".to_string(), b"ignore me\n".to_vec());
        let fetcher = FakeFetcher { files, default_branch: "main".to_string() };

        let source = GithubSkillSource {
            owner: "o".into(),
            repo: "r".into(),
            git_ref: None,
            subpath: Some("skills/pdf".into()),
        };
        let discovered = discover_and_import(&source, &fetcher).unwrap();
        assert_eq!(discovered.len(), 1);
        let skill = &discovered[0];
        assert_eq!(skill.name, "pdf");
        assert_eq!(skill.format, DetectedSkillFormat::AnthropicAgentSkill);
        // Assets keyed relative to the skill root, README excluded.
        assert!(skill.files.contains_key("SKILL.md"));
        assert!(skill.files.contains_key("scripts/run.sh"));
        assert!(!skill.files.keys().any(|k| k.contains("README")));
    }

    #[test]
    fn discover_and_import_errors_on_normalized_name_collision() {
        let mut files = BTreeMap::new();
        files.insert("a/SKILL.md".to_string(), b"---\nname: PDF Helper\ndescription: x\n---\n\nbody\n".to_vec());
        files.insert("b/SKILL.md".to_string(), b"---\nname: pdf-helper\ndescription: x\n---\n\nbody\n".to_vec());
        let fetcher = FakeFetcher { files, default_branch: "main".to_string() };
        let source =
            GithubSkillSource { owner: "o".into(), repo: "r".into(), git_ref: Some("main".into()), subpath: None };
        let err = discover_and_import(&source, &fetcher).unwrap_err();
        assert!(err.to_string().contains("collision"), "got: {err}");
    }

    #[test]
    fn discover_and_import_errors_when_no_skill_md() {
        let mut files = BTreeMap::new();
        files.insert("README.md".to_string(), b"no skills here\n".to_vec());
        let fetcher = FakeFetcher { files, default_branch: "main".to_string() };
        let source =
            GithubSkillSource { owner: "o".into(), repo: "r".into(), git_ref: Some("main".into()), subpath: None };
        let err = discover_and_import(&source, &fetcher).unwrap_err();
        assert!(err.to_string().contains("no SKILL.md"), "got: {err}");
    }

    struct NoTreeFetcher {
        skill_md: Vec<u8>,
    }
    impl RepoFetcher for NoTreeFetcher {
        fn default_branch(&self, _owner: &str, _repo: &str) -> Result<String> {
            Ok("main".to_string())
        }
        fn resolve_ref_to_sha(&self, _owner: &str, _repo: &str, git_ref: &str) -> Result<String> {
            Ok(format!("sha-{git_ref}"))
        }
        fn list_tree(&self, _owner: &str, _repo: &str, _git_ref: &str) -> Result<Vec<String>> {
            panic!("list_tree must not be called for a direct SKILL.md source");
        }
        fn fetch_file(&self, _owner: &str, _repo: &str, _git_ref: &str, _path: &str) -> Result<Vec<u8>> {
            Ok(self.skill_md.clone())
        }
    }

    #[test]
    fn discover_and_import_direct_skill_md_skips_tree_enumeration() {
        let fetcher = NoTreeFetcher { skill_md: b"---\nname: Direct Skill\ndescription: d\n---\n\nbody\n".to_vec() };
        let source = GithubSkillSource {
            owner: "o".into(),
            repo: "r".into(),
            git_ref: Some("main".into()),
            subpath: Some("skills/foo/SKILL.md".into()),
        };
        let discovered = discover_and_import(&source, &fetcher).unwrap();
        assert_eq!(discovered.len(), 1);
        assert_eq!(discovered[0].name, "direct-skill");
        assert_eq!(discovered[0].files.len(), 1, "direct import stages only the SKILL.md");
    }

    #[test]
    fn points_at_skill_md_detects_trailing_file() {
        assert!(points_at_skill_md("skills/foo/SKILL.md"));
        assert!(points_at_skill_md("SKILL.md"));
        assert!(!points_at_skill_md("skills/foo"));
    }

    #[test]
    fn materialize_writes_skill_folders() {
        let mut files = BTreeMap::new();
        files.insert("SKILL.md".to_string(), b"---\nname: x\n---\nbody\n".to_vec());
        files.insert("scripts/run.sh".to_string(), b"echo\n".to_vec());
        let skill = DiscoveredSkill {
            name: "x".to_string(),
            format: DetectedSkillFormat::AnthropicAgentSkill,
            definition: SkillDefinition {
                name: "x".to_string(),
                version: None,
                description: String::new(),
                category: None,
                activation: Default::default(),
                prompt: Default::default(),
                tool_policy: None,
                model: Default::default(),
                mcp_servers: Vec::new(),
                timeout_secs: None,
                capabilities: Default::default(),
                extra_args: Vec::new(),
                env: Default::default(),
                codex_config_overrides: Vec::new(),
                adapters: Default::default(),
                tags: Vec::new(),
            },
            skill_md_repo_path: "skills/x/SKILL.md".to_string(),
            files,
        };
        let tmp = tempfile::tempdir().unwrap();
        let roots = materialize_to_dir(&[skill], tmp.path()).unwrap();
        assert_eq!(roots.len(), 1);
        assert!(roots[0].join("SKILL.md").exists());
        assert!(roots[0].join("scripts/run.sh").exists());
    }

    #[test]
    fn sanitize_dir_name_strips_path_traversal() {
        assert_eq!(sanitize_dir_name("../../etc/passwd"), "etc-passwd");
        assert_eq!(sanitize_dir_name("good-name_1"), "good-name_1");
        assert_eq!(sanitize_dir_name("///"), "skill");
    }

    #[test]
    fn encode_path_segment_encodes_slashes() {
        assert_eq!(encode_path_segment("feature/foo"), "feature%2Ffoo");
        assert_eq!(encode_path_segment("v1.2.0"), "v1.2.0");
        assert_eq!(encode_path_segment("main"), "main");
    }

    #[test]
    fn slugify_skill_name_is_safe_single_segment() {
        assert_eq!(slugify_skill_name("PDF Helper"), "pdf-helper");
        assert_eq!(slugify_skill_name("../../etc/passwd"), "etc-passwd");
        assert_eq!(slugify_skill_name("good_name-1"), "good_name-1");
        assert_eq!(slugify_skill_name("***"), "skill");
    }

    #[test]
    fn rewrite_skill_md_name_replaces_top_level_name_and_keeps_body() {
        let content = "---\nname: Evil/../Name\ndescription: x\n---\n\nbody text\n";
        let out = rewrite_skill_md_name(content, "evil-name");
        let (skill, _) = import_skill_definition(&out, "fallback").unwrap();
        assert_eq!(skill.name, "evil-name");
        assert_eq!(skill.description, "x");
        assert_eq!(skill.prompt.system.as_deref(), Some("body text"));
    }

    #[test]
    fn rewrite_skill_md_name_inserts_name_when_absent() {
        let content = "Just a body, no frontmatter.\n";
        let out = rewrite_skill_md_name(content, "my-skill");
        let (skill, _) = import_skill_definition(&out, "fallback").unwrap();
        assert_eq!(skill.name, "my-skill");
        assert_eq!(skill.prompt.system.as_deref(), Some("Just a body, no frontmatter."));
    }

    #[test]
    fn discover_and_import_slugifies_unsafe_name() {
        let mut files = BTreeMap::new();
        files.insert(
            "skills/x/SKILL.md".to_string(),
            b"---\nname: ../../escape\ndescription: x\n---\n\nbody\n".to_vec(),
        );
        let fetcher = FakeFetcher { files, default_branch: "main".to_string() };
        let source =
            GithubSkillSource { owner: "o".into(), repo: "r".into(), git_ref: Some("main".into()), subpath: None };
        let discovered = discover_and_import(&source, &fetcher).unwrap();
        assert_eq!(discovered[0].name, "escape");
        assert_eq!(discovered[0].definition.name, "escape");
        // The staged SKILL.md reparses to the safe slug.
        let staged = String::from_utf8(discovered[0].files["SKILL.md"].clone()).unwrap();
        let (reparsed, _) = import_skill_definition(&staged, "fallback").unwrap();
        assert_eq!(reparsed.name, "escape");
    }

    #[test]
    fn discover_and_import_root_skill_includes_assets_excluding_other_skills() {
        let mut files = BTreeMap::new();
        files.insert("SKILL.md".to_string(), b"---\nname: root\n---\n\nroot body\n".to_vec());
        files.insert("scripts/run.sh".to_string(), b"echo\n".to_vec());
        files.insert("skills/nested/SKILL.md".to_string(), b"---\nname: nested\n---\n\nnested\n".to_vec());
        let fetcher = FakeFetcher { files, default_branch: "main".to_string() };
        let source =
            GithubSkillSource { owner: "o".into(), repo: "r".into(), git_ref: Some("main".into()), subpath: None };
        let discovered = discover_and_import(&source, &fetcher).unwrap();
        let root = discovered.iter().find(|s| s.name == "root").expect("root skill");
        // Root skill keeps its sibling assets ...
        assert!(root.files.contains_key("scripts/run.sh"));
        // ... but does not vacuum the nested skill's tree.
        assert!(!root.files.keys().any(|k| k.contains("nested")));
    }
}
