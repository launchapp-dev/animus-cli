use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use semver::VersionReq;
use serde::{Deserialize, Serialize};

use crate::PackRegistrySource;

pub const PACK_SELECTION_SCHEMA_ID: &str = "ao.pack-selection.v1";
pub const PACK_SELECTION_FILE_NAME: &str = "pack-selection.v1.json";

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PackSelectionSource {
    Bundled,
    Installed,
    ProjectOverride,
}

impl PackSelectionSource {
    #[must_use]
    pub const fn as_registry_source(self) -> PackRegistrySource {
        match self {
            Self::Bundled => PackRegistrySource::Bundled,
            Self::Installed => PackRegistrySource::Installed,
            Self::ProjectOverride => PackRegistrySource::ProjectOverride,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PackSelectionEntry {
    pub pack_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<PackSelectionSource>,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
}

impl PackSelectionEntry {
    pub fn validate(&self) -> Result<()> {
        let pack_id = self.pack_id.trim();
        if pack_id.is_empty() {
            return Err(anyhow!("pack selection pack_id must not be empty"));
        }

        if let Some(version) = self.version.as_deref() {
            let trimmed = version.trim();
            if trimmed.is_empty() {
                return Err(anyhow!("pack selection version must not be empty when provided"));
            }
            VersionReq::parse(trimmed).with_context(|| {
                format!("pack selection version '{}' for '{}' is not a valid semver requirement", trimmed, pack_id)
            })?;
        }

        Ok(())
    }

    #[must_use]
    pub fn matches_pack_id(&self, pack_id: &str) -> bool {
        self.pack_id.eq_ignore_ascii_case(pack_id)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PackSelectionState {
    pub schema: String,
    #[serde(default)]
    pub selections: Vec<PackSelectionEntry>,
}

impl Default for PackSelectionState {
    fn default() -> Self {
        Self { schema: PACK_SELECTION_SCHEMA_ID.to_string(), selections: Vec::new() }
    }
}

impl PackSelectionState {
    pub fn validate(&self) -> Result<()> {
        if self.schema.trim() != PACK_SELECTION_SCHEMA_ID {
            return Err(anyhow!(
                "pack selection schema must be '{}' (got '{}')",
                PACK_SELECTION_SCHEMA_ID,
                self.schema
            ));
        }

        let mut seen = std::collections::BTreeSet::new();
        for selection in &self.selections {
            selection.validate()?;
            let key = selection.pack_id.trim().to_ascii_lowercase();
            if !seen.insert(key) {
                return Err(anyhow!("duplicate pack selection for '{}'", selection.pack_id));
            }
        }

        Ok(())
    }

    #[must_use]
    pub fn selection_for(&self, pack_id: &str) -> Option<&PackSelectionEntry> {
        self.selections.iter().find(|selection| selection.matches_pack_id(pack_id))
    }

    pub fn upsert(&mut self, entry: PackSelectionEntry) -> Result<()> {
        entry.validate()?;
        self.selections.retain(|current| !current.matches_pack_id(&entry.pack_id));
        self.selections.push(entry);
        self.selections.sort_by(|left, right| left.pack_id.cmp(&right.pack_id));
        Ok(())
    }
}

fn default_enabled() -> bool {
    true
}

pub fn pack_selection_path(project_root: &Path) -> PathBuf {
    let base = protocol::scoped_state_root(project_root).unwrap_or_else(|| project_root.join(".ao"));
    base.join("state").join(PACK_SELECTION_FILE_NAME)
}

pub fn load_pack_selection_state(project_root: &Path) -> Result<PackSelectionState> {
    let path = pack_selection_path(project_root);
    if !path.exists() {
        return Ok(PackSelectionState::default());
    }

    let raw = fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
    let state: PackSelectionState =
        serde_json::from_str(&raw).with_context(|| format!("failed to parse {}", path.display()))?;
    state.validate()?;
    Ok(state)
}

pub fn save_pack_selection_state(project_root: &Path, state: &PackSelectionState) -> Result<()> {
    state.validate()?;
    let path = pack_selection_path(project_root);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let raw = serde_json::to_string_pretty(state)?;
    fs::write(&path, raw).with_context(|| format!("failed to write {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pack_selection_upsert_replaces_existing_entry() {
        let mut state = PackSelectionState::default();
        state
            .upsert(PackSelectionEntry {
                pack_id: "ao.review".to_string(),
                version: Some("^0.1".to_string()),
                source: Some(PackSelectionSource::Installed),
                enabled: true,
            })
            .expect("initial upsert should succeed");
        state
            .upsert(PackSelectionEntry {
                pack_id: "ao.review".to_string(),
                version: Some("^0.2".to_string()),
                source: Some(PackSelectionSource::ProjectOverride),
                enabled: false,
            })
            .expect("replacement upsert should succeed");

        let selection = state.selection_for("ao.review").expect("selection should exist");
        assert_eq!(selection.version.as_deref(), Some("^0.2"));
        assert_eq!(selection.source, Some(PackSelectionSource::ProjectOverride));
        assert!(!selection.enabled);
    }

    #[test]
    fn pack_selection_rejects_invalid_version_requirement() {
        let state = PackSelectionState {
            schema: PACK_SELECTION_SCHEMA_ID.to_string(),
            selections: vec![PackSelectionEntry {
                pack_id: "ao.review".to_string(),
                version: Some("not-a-version".to_string()),
                source: None,
                enabled: true,
            }],
        };

        let error = state.validate().expect_err("invalid version requirement should fail");
        assert!(error.to_string().contains("valid semver requirement"));
    }
}
