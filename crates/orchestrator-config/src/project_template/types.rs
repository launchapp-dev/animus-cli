use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::PackSelectionSource;

pub const PROJECT_TEMPLATE_MANIFEST_FILE_NAME: &str = "template.toml";
pub const PROJECT_TEMPLATE_MANIFEST_SCHEMA_ID: &str = "animus.template.v1";

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ProjectTemplateSourceMode {
    Copy,
    Clone,
    Overlay,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProjectTemplateSource {
    pub mode: ProjectTemplateSourceMode,
    pub root: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(deny_unknown_fields)]
pub struct ProjectTemplatePack {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<PackSelectionSource>,
    #[serde(default)]
    pub activate: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(deny_unknown_fields)]
pub struct ProjectTemplateDaemon {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_merge: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_pr: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_commit_before_merge: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProjectTemplateManifest {
    pub schema: String,
    pub id: String,
    pub version: String,
    pub title: String,
    #[serde(default)]
    pub description: String,
    pub pattern: String,
    pub source: ProjectTemplateSource,
    #[serde(default)]
    pub daemon: ProjectTemplateDaemon,
    #[serde(default)]
    pub packs: Vec<ProjectTemplatePack>,
    #[serde(default)]
    pub next_steps: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProjectTemplateSourceKind {
    Bundled,
    Local,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProjectTemplateSummary {
    pub id: String,
    pub version: String,
    pub title: String,
    pub description: String,
    pub pattern: String,
    pub source_kind: ProjectTemplateSourceKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectTemplateFile {
    pub relative_path: PathBuf,
    pub contents: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedProjectTemplate {
    pub source_kind: ProjectTemplateSourceKind,
    pub template_root: Option<PathBuf>,
    pub manifest: ProjectTemplateManifest,
    pub files: Vec<ProjectTemplateFile>,
}

impl LoadedProjectTemplate {
    #[must_use]
    pub fn summary(&self) -> ProjectTemplateSummary {
        ProjectTemplateSummary {
            id: self.manifest.id.clone(),
            version: self.manifest.version.clone(),
            title: self.manifest.title.clone(),
            description: self.manifest.description.clone(),
            pattern: self.manifest.pattern.clone(),
            source_kind: self.source_kind,
        }
    }
}
