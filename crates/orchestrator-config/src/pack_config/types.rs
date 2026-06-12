use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

pub const PACK_MANIFEST_FILE_NAME: &str = "pack.toml";
pub const PACK_MANIFEST_SCHEMA_ID: &str = "animus.pack.v1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum PackKind {
    DomainPack,
    ConnectorPack,
    CapabilityPack,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum PackOwnershipMode {
    Bundled,
    Installed,
    Project,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ExternalRuntimeKind {
    Node,
    Python,
    Uv,
    Npm,
    Pnpm,
}

impl ExternalRuntimeKind {
    pub fn binary_name(&self) -> &'static str {
        match self {
            Self::Node => "node",
            Self::Python => "python",
            Self::Uv => "uv",
            Self::Npm => "npm",
            Self::Pnpm => "pnpm",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PackOwnership {
    pub mode: PackOwnershipMode,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PackCompatibility {
    #[serde(default, alias = "ao_core", skip_serializing_if = "Option::is_none")]
    pub animus_core: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_schema: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject_schema: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PackSubjects {
    #[serde(default)]
    pub kinds: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_kind: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PackWorkflows {
    pub root: String,
    #[serde(default)]
    pub exports: Vec<String>,
}

/// Skills-only contribution shipped by a pack.
///
/// Packs that ship YAML skill definitions (no workflows) declare a `[skills]`
/// section pointing at a relative directory. The pack registry surfaces every
/// `*.yaml` file in that directory as a `SkillSourceOrigin::Installed` entry,
/// using the pack id/version for provenance and integrity tracking.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PackSkills {
    /// Directory relative to the pack root that contains `*.yaml` skill manifests.
    /// Defaults to `"skills"` so packs can omit the field entirely.
    #[serde(default = "default_pack_skills_root")]
    pub root: String,
    /// Optional skill-name aliases. The map key is the new skill name; the
    /// value is the YAML file stem (without extension) under `[skills].root`
    /// to expose under that alias. Used to keep persona/task references valid
    /// when several names should resolve to the same underlying skill manifest.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub aliases: BTreeMap<String, String>,
}

fn default_pack_skills_root() -> String {
    "skills".to_string()
}

impl Default for PackSkills {
    fn default() -> Self {
        Self { root: default_pack_skills_root(), aliases: BTreeMap::new() }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PackRuntimeRequirement {
    pub runtime: ExternalRuntimeKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default)]
    pub optional: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PackRuntime {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_overlay: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_overlay: Option<String>,
    #[serde(default)]
    pub requirements: Vec<PackRuntimeRequirement>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PackMcp {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub servers: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PackSchedules {
    pub file: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PackDependency {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default)]
    pub optional: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// A plugin requirement declared by a pack via `[[requires_plugins]]`.
///
/// Packs whose workflows depend on installed Animus plugins (for example a
/// subject backend or provider) declare them here so `animus pack install`
/// can check the installed-plugin registry up front instead of failing at
/// phase time. Additive as of animus v0.5.14: older animus versions reject
/// manifests that USE this section (`deny_unknown_fields`), so packs that
/// declare it should also set `compatibility.animus_core = ">=0.5.14"`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PackPluginRequirement {
    /// GitHub repo slug of the plugin, e.g. `launchapp-dev/animus-subject-linear`.
    pub repo: String,
    /// Optional release tag to suggest/install, e.g. `v0.2.0`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
    /// Informational role hint, e.g. `subject_backend:linear`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default)]
    pub optional: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PackPermissions {
    #[serde(default)]
    pub tools: Vec<String>,
    #[serde(default)]
    pub mcp_namespaces: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PackSecrets {
    #[serde(default)]
    pub required: Vec<String>,
    #[serde(default)]
    pub optional: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PackNativeModule {
    pub feature: String,
    pub module_id: String,
    #[serde(default)]
    pub optional: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PackManifest {
    pub schema: String,
    pub id: String,
    pub version: String,
    pub kind: PackKind,
    pub title: String,
    #[serde(default)]
    pub description: String,
    pub ownership: PackOwnership,
    #[serde(default)]
    pub compatibility: PackCompatibility,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subjects: Option<PackSubjects>,
    /// Workflow contributions are optional so packs may ship skills-only
    /// content. Either `workflows` or `skills` must be present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflows: Option<PackWorkflows>,
    /// Skill catalog contributions. Either `workflows` or `skills` must be present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skills: Option<PackSkills>,
    #[serde(default)]
    pub runtime: PackRuntime,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp: Option<PackMcp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schedules: Option<PackSchedules>,
    #[serde(default)]
    pub dependencies: Vec<PackDependency>,
    /// Animus plugins this pack's workflows require. Additive section: older
    /// animus versions reject manifests that declare it, so pair with a
    /// `compatibility.animus_core` floor of `>=0.5.14`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requires_plugins: Vec<PackPluginRequirement>,
    #[serde(default)]
    pub permissions: PackPermissions,
    #[serde(default)]
    pub secrets: PackSecrets,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_module: Option<PackNativeModule>,
}
