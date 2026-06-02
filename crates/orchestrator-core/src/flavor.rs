//! Animus flavor manifests (`flavors/<name>.toml`).
//!
//! A *flavor* is a curated bundle of plugins, packs, and defaults that
//! ship as the canonical install for a given user persona. v0.5 ships
//! exactly one flavor — `default` — per the discipline rule
//! "One flavor at launch" in `docs/architecture/kernel-and-flavors.md`.
//!
//! This module loads `flavors/<name>.toml` from the repo's working tree
//! and exposes the typed manifest to the CLI (`animus flavor`), to
//! `animus plugin install-defaults`, and to the daemon health surface
//! (`animus daemon health --json | jq '.flavor'`).
//!
//! Discovery rule: the loader probes for `flavors/<name>.toml` relative
//! to (in order):
//!
//! 1. `ANIMUS_FLAVORS_DIR` env var if set.
//! 2. `<cwd>/flavors/`.
//! 3. Walk up from `<cwd>` looking for a sibling `flavors/` directory
//!    next to a `Cargo.toml` or `.git`.
//!
//! The first hit wins. If no manifest is found, the loader returns
//! `Ok(None)` and callers should fall back to the hardcoded constants
//! in [`crate::plugin_registry`].

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Schema constant emitted in every flavor manifest's `schema` field.
pub const FLAVOR_SCHEMA_V1: &str = "animus.flavor.v1";

/// Canonical flavor id for v0.5.
pub const DEFAULT_FLAVOR_ID: &str = "default";

/// One required/recommended plugin section in a flavor manifest.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct FlavorRoleSection {
    #[serde(default)]
    pub required: Vec<String>,
    #[serde(default)]
    pub recommended: Vec<String>,
}

/// Defaults block (free-form key/value hints for the daemon, not load-bearing
/// for plugin install in v0.5).
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct FlavorDefaults {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_routing: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_ceiling_daily_usd: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cloud: Option<String>,
}

/// A parsed `flavors/<name>.toml` manifest.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct FlavorManifest {
    /// MUST equal [`FLAVOR_SCHEMA_V1`].
    pub schema: String,
    pub id: String,
    pub version: String,
    pub title: String,
    pub description: String,

    #[serde(default)]
    pub providers: FlavorRoleSection,
    #[serde(default)]
    pub subjects: FlavorRoleSection,
    #[serde(default)]
    pub transports: FlavorRoleSection,
    #[serde(default)]
    pub ui: FlavorRoleSection,
    #[serde(default)]
    pub triggers: FlavorRoleSection,
    #[serde(default)]
    pub workflow_runner: FlavorRoleSection,
    #[serde(default)]
    pub queue: FlavorRoleSection,
    #[serde(default)]
    pub durable_store: FlavorRoleSection,
    #[serde(default)]
    pub memory_store: FlavorRoleSection,
    #[serde(default)]
    pub packs: FlavorRoleSection,
    #[serde(default)]
    pub defaults: FlavorDefaults,
}

impl FlavorManifest {
    /// Validate that the manifest declares the v1 schema. Returns
    /// `Err(...)` for schema drift so callers cannot silently consume an
    /// unknown manifest shape.
    pub fn validate(&self) -> Result<()> {
        if self.schema != FLAVOR_SCHEMA_V1 {
            anyhow::bail!("unknown flavor manifest schema: '{}' (expected '{}')", self.schema, FLAVOR_SCHEMA_V1);
        }
        Ok(())
    }

    /// Collect every plugin slug declared in this manifest under the
    /// `workflow_runner`, `queue`, `providers`, `subjects`, `transports`,
    /// `ui`, `triggers`, `durable_store`, and `memory_store` sections.
    /// Used by `animus flavor install` and by `plugin install-defaults`
    /// to assemble the install plan.
    pub fn all_plugin_slugs(&self, include_recommended: bool) -> Vec<String> {
        let mut out = Vec::new();
        for section in [
            &self.workflow_runner,
            &self.queue,
            &self.providers,
            &self.subjects,
            &self.transports,
            &self.ui,
            &self.triggers,
            &self.durable_store,
            &self.memory_store,
        ] {
            out.extend(section.required.iter().cloned());
            if include_recommended {
                out.extend(section.recommended.iter().cloned());
            }
        }
        out
    }
}

/// Locate `flavors/<name>.toml` according to the discovery rule documented
/// at the top of this module, anchored at the supplied project root. The
/// caller MUST pass the resolved project root rather than relying on
/// process CWD, so the loader stays stable when the CLI is invoked from
/// arbitrary working directories.
pub fn locate_flavor_manifest_in(project_root: &Path, name: &str) -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("ANIMUS_FLAVORS_DIR") {
        let path = Path::new(&dir).join(format!("{name}.toml"));
        if path.is_file() {
            return Some(path);
        }
    }

    let direct = project_root.join("flavors").join(format!("{name}.toml"));
    if direct.is_file() {
        return Some(direct);
    }

    let mut walker: &Path = project_root;
    while let Some(parent) = walker.parent() {
        let candidate = parent.join("flavors").join(format!("{name}.toml"));
        if candidate.is_file() {
            return Some(candidate);
        }
        walker = parent;
    }

    None
}

/// CWD-anchored variant retained for callers that have no project root to
/// hand (the standalone `animus flavor list` command). Production
/// surfaces should prefer [`locate_flavor_manifest_in`].
pub fn locate_flavor_manifest(name: &str) -> Option<PathBuf> {
    let cwd = std::env::current_dir().ok()?;
    locate_flavor_manifest_in(&cwd, name)
}

/// Built-in copy of the canonical `flavors/default.toml` manifest,
/// shipped with the binary so cargo-installed `animus` can still resolve
/// the default flavor when no `flavors/` directory is found in the user
/// project tree. Codex R6 [P2] fix.
const BUNDLED_DEFAULT_FLAVOR: &str = include_str!("../../../flavors/default.toml");

/// Load and validate a flavor manifest by name. Anchors discovery at the
/// supplied project root. Falls back to the binary-bundled
/// `flavors/default.toml` when (and only when) `name` is the canonical
/// default and no on-disk manifest is found.
pub fn load_flavor_in(project_root: &Path, name: &str) -> Result<Option<FlavorManifest>> {
    if let Some(path) = locate_flavor_manifest_in(project_root, name) {
        let bytes = std::fs::read_to_string(&path)
            .with_context(|| format!("failed to read flavor manifest at {}", path.display()))?;
        let manifest: FlavorManifest =
            toml::from_str(&bytes).with_context(|| format!("failed to parse flavor manifest at {}", path.display()))?;
        manifest.validate()?;
        return Ok(Some(manifest));
    }
    if name == DEFAULT_FLAVOR_ID {
        let manifest: FlavorManifest =
            toml::from_str(BUNDLED_DEFAULT_FLAVOR).context("failed to parse bundled default flavor manifest")?;
        manifest.validate()?;
        return Ok(Some(manifest));
    }
    Ok(None)
}

/// CWD-anchored variant retained for callers that have no project root to
/// hand. Production surfaces should prefer [`load_flavor_in`].
pub fn load_flavor(name: &str) -> Result<Option<FlavorManifest>> {
    let cwd = match std::env::current_dir() {
        Ok(c) => c,
        Err(_) => return Ok(None),
    };
    load_flavor_in(&cwd, name)
}

/// List every flavor name available on the discovery search paths. v0.5
/// always returns at least `["default"]` even if the flavor file is
/// missing, so the CLI surface stays consistent.
pub fn list_available_flavor_names() -> Vec<String> {
    let mut names = std::collections::BTreeSet::new();
    let mut probe = |dir: PathBuf| {
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                if let Some(stem) = entry.path().file_stem().and_then(|s| s.to_str()) {
                    if entry.path().extension().and_then(|s| s.to_str()) == Some("toml") {
                        names.insert(stem.to_string());
                    }
                }
            }
        }
    };

    if let Ok(dir) = std::env::var("ANIMUS_FLAVORS_DIR") {
        probe(PathBuf::from(dir));
    }
    if let Ok(cwd) = std::env::current_dir() {
        probe(cwd.join("flavors"));
        let mut walker: &Path = &cwd;
        while let Some(parent) = walker.parent() {
            probe(parent.join("flavors"));
            walker = parent;
        }
    }

    if names.is_empty() {
        names.insert(DEFAULT_FLAVOR_ID.to_string());
    }
    names.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
schema = "animus.flavor.v1"
id = "default"
version = "0.5.0"
title = "Animus Default"
description = "Curated bundle."

[workflow_runner]
required = ["launchapp-dev/animus-workflow-runner-default"]

[queue]
required = ["launchapp-dev/animus-queue-default"]

[providers]
required = ["launchapp-dev/animus-provider-claude"]
recommended = ["launchapp-dev/animus-provider-codex"]
"#;

    #[test]
    fn parses_and_validates_v1_manifest() {
        let manifest: FlavorManifest = toml::from_str(SAMPLE).unwrap();
        manifest.validate().unwrap();
        assert_eq!(manifest.id, "default");
        assert_eq!(manifest.workflow_runner.required.len(), 1);
        assert_eq!(manifest.queue.required.len(), 1);
        assert_eq!(manifest.providers.recommended.len(), 1);
    }

    #[test]
    fn rejects_unknown_schema() {
        let bad = SAMPLE.replace("animus.flavor.v1", "animus.flavor.v999");
        let manifest: FlavorManifest = toml::from_str(&bad).unwrap();
        assert!(manifest.validate().is_err());
    }

    #[test]
    fn all_plugin_slugs_collects_required_and_optional_recommendations() {
        let manifest: FlavorManifest = toml::from_str(SAMPLE).unwrap();
        let required_only = manifest.all_plugin_slugs(false);
        assert!(required_only.contains(&"launchapp-dev/animus-workflow-runner-default".to_string()));
        assert!(required_only.contains(&"launchapp-dev/animus-queue-default".to_string()));
        assert!(required_only.contains(&"launchapp-dev/animus-provider-claude".to_string()));
        assert!(!required_only.contains(&"launchapp-dev/animus-provider-codex".to_string()));

        let with_recommended = manifest.all_plugin_slugs(true);
        assert!(with_recommended.contains(&"launchapp-dev/animus-provider-codex".to_string()));
    }
}
