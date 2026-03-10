use serde::{Deserialize, Serialize};

fn default_registry_available() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(super) struct SkillRegistrySourceConfig {
    pub(super) id: String,
    #[serde(default)]
    pub(super) priority: u32,
    #[serde(default = "default_registry_available")]
    pub(super) available: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(super) struct SkillVersionRecord {
    pub(super) name: String,
    pub(super) version: String,
    pub(super) source: String,
    pub(super) registry: String,
    pub(super) integrity: String,
    pub(super) artifact: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(super) struct ResolvedSkillEntry {
    pub(super) name: String,
    pub(super) version: String,
    pub(super) source: String,
    pub(super) registry: String,
    pub(super) integrity: String,
    pub(super) artifact: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(super) struct SkillProjectConstraint {
    pub(super) name: String,
    #[serde(default)]
    pub(super) version: Option<String>,
    #[serde(default)]
    pub(super) source: Option<String>,
    #[serde(default)]
    pub(super) registry: Option<String>,
    #[serde(default)]
    pub(super) allow_prerelease: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub(super) struct SkillRegistryStateV1 {
    #[serde(default)]
    pub(super) registries: Vec<SkillRegistrySourceConfig>,
    #[serde(default)]
    pub(super) catalog: Vec<SkillVersionRecord>,
    #[serde(default)]
    pub(super) installed: Vec<ResolvedSkillEntry>,
    #[serde(default)]
    pub(super) defaults: Vec<SkillProjectConstraint>,
}

impl SkillRegistryStateV1 {
    pub(super) fn normalize(&mut self) {
        self.registries.sort_by(|a, b| {
            a.priority
                .cmp(&b.priority)
                .then_with(|| a.id.cmp(&b.id))
                .then_with(|| a.available.cmp(&b.available))
        });
        self.registries.dedup_by(|a, b| a.id == b.id);

        self.catalog.sort_by(|a, b| {
            a.name
                .cmp(&b.name)
                .then_with(|| a.source.cmp(&b.source))
                .then_with(|| a.registry.cmp(&b.registry))
                .then_with(|| a.version.cmp(&b.version))
                .then_with(|| a.integrity.cmp(&b.integrity))
                .then_with(|| a.artifact.cmp(&b.artifact))
        });
        self.catalog.dedup_by(|a, b| {
            a.name == b.name
                && a.version == b.version
                && a.source == b.source
                && a.registry == b.registry
        });

        self.installed.sort_by(|a, b| {
            a.name
                .cmp(&b.name)
                .then_with(|| a.source.cmp(&b.source))
                .then_with(|| a.registry.cmp(&b.registry))
                .then_with(|| a.version.cmp(&b.version))
        });
        self.installed
            .dedup_by(|a, b| a.name == b.name && a.source == b.source);

        self.defaults.sort_by(|a, b| a.name.cmp(&b.name));
        self.defaults.dedup_by(|a, b| a.name == b.name);
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(super) struct SkillLockEntry {
    pub(super) name: String,
    pub(super) version: String,
    pub(super) source: String,
    pub(super) integrity: String,
    pub(super) artifact: String,
    #[serde(default)]
    pub(super) registry: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub(super) struct SkillLockStateV1 {
    #[serde(default)]
    pub(super) entries: Vec<SkillLockEntry>,
}

impl SkillLockStateV1 {
    pub(super) fn normalize(&mut self) {
        self.entries.sort_by(|a, b| {
            a.name
                .cmp(&b.name)
                .then_with(|| a.source.cmp(&b.source))
                .then_with(|| a.version.cmp(&b.version))
                .then_with(|| a.integrity.cmp(&b.integrity))
                .then_with(|| a.artifact.cmp(&b.artifact))
        });
        self.entries
            .dedup_by(|a, b| a.name == b.name && a.source == b.source);
    }
}
