//! `animus.toml` — the project dependency manifest.
//!
//! A committed `animus.toml` declares a project's intended kernel version,
//! plugins, and packs. `animus install` resolves it into `.animus/plugins.lock`
//! and installs the set; `animus add` / `animus remove` mutate the manifest.
//! This mirrors the npm/cargo model: the manifest states intent, the lock pins
//! the exact installed artifacts.
//!
//! Schema:
//!
//! ```toml
//! [project]
//! kernel = ">=0.6.8"
//!
//! [plugins]
//! animus-provider-claude = ">=0.2.7"                                       # curated: bare version req
//! animus-queue-default   = { git = "launchapp-dev/animus-queue-default", tag = "v0.3.3" }  # explicit git pin
//! animus-config-postgres = { path = "deploy/plugin-src/animus-config-postgres" }            # vendored
//!
//! [packs]
//! "animus.core-skills" = ">=0.1.0"
//! ```
//!
//! A dependency value is one of:
//! - a bare string (a version requirement — resolved against the curated
//!   plugin/pack tables at install time),
//! - `{ git = "OWNER/REPO", tag = "vX.Y.Z", version = "<opt>" }`,
//! - `{ path = "relative/or/absolute/path" }`.
//!
//! Edits re-serialize the in-memory model, so hand-authored comments are not
//! preserved across `animus add` / `animus remove` (v1 limitation).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::Deserialize;

/// The manifest file name, resolved relative to the project root.
pub const PROJECT_MANIFEST_FILE_NAME: &str = "animus.toml";

/// Resolve `<project_root>/animus.toml`.
pub fn project_manifest_path(project_root: &Path) -> PathBuf {
    project_root.join(PROJECT_MANIFEST_FILE_NAME)
}

/// One dependency entry in the manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Dependency {
    /// A bare version requirement (e.g. `">=0.2.7"`). Resolved against the
    /// curated plugin/pack tables at install time.
    Version(String),
    /// An explicit Git pin: `OWNER/REPO` at a release tag.
    Git { repo: String, tag: String, version: Option<String> },
    /// A local source path (relative to the project root or absolute).
    Path { path: String },
}

/// A parsed, validated `animus.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ProjectManifest {
    /// `[project].kernel` — the required kernel version (advisory in v1).
    pub kernel: Option<String>,
    /// `[plugins]` — keyed by plugin slug (the repo basename for curated plugins).
    pub plugins: BTreeMap<String, Dependency>,
    /// `[packs]` — keyed by pack id (e.g. `animus.core-skills`).
    pub packs: BTreeMap<String, Dependency>,
}

impl ProjectManifest {
    /// Insert or replace a plugin dependency.
    pub fn upsert_plugin(&mut self, name: &str, dep: Dependency) {
        self.plugins.insert(name.to_string(), dep);
    }

    /// Remove a plugin dependency. Returns `true` when an entry was removed.
    pub fn remove_plugin(&mut self, name: &str) -> bool {
        self.plugins.remove(name).is_some()
    }

    /// Insert or replace a pack dependency.
    pub fn upsert_pack(&mut self, id: &str, dep: Dependency) {
        self.packs.insert(id.to_string(), dep);
    }

    /// Remove a pack dependency. Returns `true` when an entry was removed.
    pub fn remove_pack(&mut self, id: &str) -> bool {
        self.packs.remove(id).is_some()
    }

    /// Serialize to clean, deterministic TOML. Keys sort via `BTreeMap`, so
    /// diffs stay stable across edits.
    pub fn to_toml_string(&self) -> String {
        let mut out = String::new();
        out.push_str("[project]\n");
        if let Some(kernel) = &self.kernel {
            out.push_str(&format!("kernel = \"{}\"\n", escape_toml(kernel)));
        }
        out.push_str("\n[plugins]\n");
        for (name, dep) in &self.plugins {
            out.push_str(&format!("{} = {}\n", toml_key(name), dependency_to_toml_value(dep)));
        }
        out.push_str("\n[packs]\n");
        for (id, dep) in &self.packs {
            out.push_str(&format!("{} = {}\n", toml_key(id), dependency_to_toml_value(dep)));
        }
        out
    }
}

/// Load `<project_root>/animus.toml`. Returns `Ok(None)` when the file is
/// absent (callers surface an actionable "run `animus init`" error).
pub fn load_project_manifest(project_root: &Path) -> Result<Option<ProjectManifest>> {
    let path = project_manifest_path(project_root);
    if !path.exists() {
        return Ok(None);
    }
    let contents = std::fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
    let manifest =
        parse_project_manifest(&contents).with_context(|| format!("invalid manifest at {}", path.display()))?;
    Ok(Some(manifest))
}

/// Write `<project_root>/animus.toml`.
pub fn save_project_manifest(project_root: &Path, manifest: &ProjectManifest) -> Result<()> {
    let path = project_manifest_path(project_root);
    std::fs::write(&path, manifest.to_toml_string()).with_context(|| format!("failed to write {}", path.display()))?;
    Ok(())
}

/// Parse + validate manifest TOML.
pub fn parse_project_manifest(contents: &str) -> Result<ProjectManifest> {
    let raw: RawManifest = toml::from_str(contents).context("failed to parse animus.toml")?;

    let kernel = match raw.project.kernel {
        Some(kernel) if kernel.trim().is_empty() => bail!("[project].kernel must not be empty"),
        other => other,
    };

    let mut plugins = BTreeMap::new();
    for (name, raw_dep) in raw.plugins {
        let dep = convert_dependency(&name, raw_dep, "plugin")?;
        plugins.insert(name, dep);
    }

    let mut packs = BTreeMap::new();
    for (id, raw_dep) in raw.packs {
        let dep = convert_dependency(&id, raw_dep, "pack")?;
        packs.insert(id, dep);
    }

    Ok(ProjectManifest { kernel, plugins, packs })
}

// ---------------------------------------------------------------------------
// Raw deserialization shapes
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct RawManifest {
    #[serde(default)]
    project: RawProject,
    #[serde(default)]
    plugins: BTreeMap<String, RawDependency>,
    #[serde(default)]
    packs: BTreeMap<String, RawDependency>,
}

#[derive(Debug, Default, Deserialize)]
struct RawProject {
    kernel: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum RawDependency {
    Version(String),
    Detailed(RawDetailed),
}

#[derive(Debug, Deserialize)]
struct RawDetailed {
    git: Option<String>,
    tag: Option<String>,
    path: Option<String>,
    version: Option<String>,
}

fn convert_dependency(name: &str, raw: RawDependency, kind: &str) -> Result<Dependency> {
    match raw {
        RawDependency::Version(version) => {
            if version.trim().is_empty() {
                bail!("{kind} '{name}': version requirement must not be empty");
            }
            Ok(Dependency::Version(version))
        }
        RawDependency::Detailed(detailed) => match (detailed.git, detailed.path) {
            (Some(_), Some(_)) => bail!("{kind} '{name}': `git` and `path` are mutually exclusive"),
            (Some(repo), None) => {
                validate_repo_slug(name, kind, &repo)?;
                let tag = detailed
                    .tag
                    .ok_or_else(|| anyhow::anyhow!("{kind} '{name}': a `git` dependency requires a release `tag`"))?;
                if tag.trim().is_empty() {
                    bail!("{kind} '{name}': `tag` must not be empty");
                }
                Ok(Dependency::Git { repo, tag, version: detailed.version })
            }
            (None, Some(path)) => {
                if path.trim().is_empty() {
                    bail!("{kind} '{name}': `path` must not be empty");
                }
                if detailed.tag.is_some() {
                    bail!("{kind} '{name}': `tag` only applies to `git` dependencies");
                }
                Ok(Dependency::Path { path })
            }
            (None, None) => {
                bail!("{kind} '{name}': a table dependency must set either `git` (with `tag`) or `path`")
            }
        },
    }
}

fn validate_repo_slug(name: &str, kind: &str, repo: &str) -> Result<()> {
    let parts: Vec<&str> = repo.split('/').collect();
    if parts.len() != 2 || parts.iter().any(|part| part.trim().is_empty()) {
        bail!("{kind} '{name}': `git` must be in OWNER/REPO form, got '{repo}'");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// TOML emission helpers
// ---------------------------------------------------------------------------

/// Render a TOML key, quoting it when it is not a bare-key-safe identifier
/// (pack ids carry `.` and must be quoted; plugin slugs with `-`/`_` are bare).
fn toml_key(key: &str) -> String {
    let bare_safe = !key.is_empty() && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    if bare_safe {
        key.to_string()
    } else {
        format!("\"{}\"", escape_toml(key))
    }
}

fn dependency_to_toml_value(dep: &Dependency) -> String {
    match dep {
        Dependency::Version(version) => format!("\"{}\"", escape_toml(version)),
        Dependency::Git { repo, tag, version } => {
            let mut value = format!("{{ git = \"{}\", tag = \"{}\"", escape_toml(repo), escape_toml(tag));
            if let Some(version) = version {
                value.push_str(&format!(", version = \"{}\"", escape_toml(version)));
            }
            value.push_str(" }");
            value
        }
        Dependency::Path { path } => format!("{{ path = \"{}\" }}", escape_toml(path)),
    }
}

fn escape_toml(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_all_three_dependency_forms() {
        let manifest = parse_project_manifest(
            r#"
[project]
kernel = ">=0.6.8"

[plugins]
animus-provider-claude = ">=0.2.7"
animus-queue-default = { git = "launchapp-dev/animus-queue-default", tag = "v0.3.3" }
animus-config-postgres = { path = "deploy/plugin-src/animus-config-postgres" }

[packs]
"animus.core-skills" = ">=0.1.0"
"#,
        )
        .expect("manifest should parse");

        assert_eq!(manifest.kernel.as_deref(), Some(">=0.6.8"));
        assert_eq!(manifest.plugins.get("animus-provider-claude"), Some(&Dependency::Version(">=0.2.7".to_string())));
        assert_eq!(
            manifest.plugins.get("animus-queue-default"),
            Some(&Dependency::Git {
                repo: "launchapp-dev/animus-queue-default".to_string(),
                tag: "v0.3.3".to_string(),
                version: None,
            })
        );
        assert_eq!(
            manifest.plugins.get("animus-config-postgres"),
            Some(&Dependency::Path { path: "deploy/plugin-src/animus-config-postgres".to_string() })
        );
        assert_eq!(manifest.packs.get("animus.core-skills"), Some(&Dependency::Version(">=0.1.0".to_string())));
    }

    #[test]
    fn empty_version_requirement_is_rejected() {
        let err = parse_project_manifest("[plugins]\nfoo = \"\"\n").expect_err("empty version must error");
        assert!(err.to_string().contains("must not be empty"), "got: {err}");
    }

    #[test]
    fn git_and_path_are_mutually_exclusive() {
        let err = parse_project_manifest("[plugins]\nfoo = { git = \"o/r\", tag = \"v1\", path = \"p\" }\n")
            .expect_err("git+path must error");
        assert!(err.to_string().contains("mutually exclusive"), "got: {err}");
    }

    #[test]
    fn git_dependency_requires_tag() {
        let err = parse_project_manifest("[plugins]\nfoo = { git = \"owner/repo\" }\n")
            .expect_err("git without tag must error");
        assert!(err.to_string().contains("requires a release `tag`"), "got: {err}");
    }

    #[test]
    fn tag_without_git_is_rejected() {
        let err = parse_project_manifest("[plugins]\nfoo = { path = \"p\", tag = \"v1\" }\n")
            .expect_err("tag on a path dep must error");
        assert!(err.to_string().contains("only applies to `git`"), "got: {err}");
    }

    #[test]
    fn malformed_repo_slug_is_rejected() {
        let err = parse_project_manifest("[plugins]\nfoo = { git = \"not-a-slug\", tag = \"v1\" }\n")
            .expect_err("bad slug must error");
        assert!(err.to_string().contains("OWNER/REPO"), "got: {err}");
    }

    #[test]
    fn table_without_source_is_rejected() {
        let err = parse_project_manifest("[plugins]\nfoo = { version = \"1\" }\n")
            .expect_err("table without git/path must error");
        assert!(err.to_string().contains("must set either `git`"), "got: {err}");
    }

    #[test]
    fn empty_kernel_is_rejected() {
        let err = parse_project_manifest("[project]\nkernel = \"\"\n").expect_err("empty kernel must error");
        assert!(err.to_string().contains("kernel must not be empty"), "got: {err}");
    }

    #[test]
    fn round_trips_through_toml() {
        let mut manifest = ProjectManifest { kernel: Some(">=0.6.8".to_string()), ..Default::default() };
        manifest.upsert_plugin("animus-provider-claude", Dependency::Version(">=0.2.7".to_string()));
        manifest.upsert_plugin(
            "animus-queue-default",
            Dependency::Git {
                repo: "launchapp-dev/animus-queue-default".to_string(),
                tag: "v0.3.3".to_string(),
                version: None,
            },
        );
        manifest.upsert_plugin("vendored", Dependency::Path { path: "plugins/vendored".to_string() });
        manifest.upsert_pack("animus.core-skills", Dependency::Version(">=0.1.0".to_string()));

        let rendered = manifest.to_toml_string();
        let reparsed = parse_project_manifest(&rendered).expect("rendered manifest should reparse");
        assert_eq!(manifest, reparsed, "manifest should survive a TOML round-trip");
    }

    #[test]
    fn pack_ids_are_quoted_plugin_slugs_are_bare() {
        let mut manifest = ProjectManifest::default();
        manifest.upsert_plugin("animus-provider-claude", Dependency::Version("*".to_string()));
        manifest.upsert_pack("animus.core-skills", Dependency::Version("*".to_string()));
        let rendered = manifest.to_toml_string();
        assert!(rendered.contains("animus-provider-claude = \"*\""), "bare plugin key: {rendered}");
        assert!(rendered.contains("\"animus.core-skills\" = \"*\""), "quoted pack key: {rendered}");
    }
}
