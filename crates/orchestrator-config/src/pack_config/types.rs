use serde::{Deserialize, Serialize};

pub const PACK_MANIFEST_FILE_NAME: &str = "pack.toml";
pub const PACK_MANIFEST_SCHEMA_ID: &str = "ao.pack.v1";

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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ao_core: Option<String>,
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
    pub workflows: PackWorkflows,
    #[serde(default)]
    pub runtime: PackRuntime,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp: Option<PackMcp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schedules: Option<PackSchedules>,
    #[serde(default)]
    pub dependencies: Vec<PackDependency>,
    #[serde(default)]
    pub permissions: PackPermissions,
    #[serde(default)]
    pub secrets: PackSecrets,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_module: Option<PackNativeModule>,
}
