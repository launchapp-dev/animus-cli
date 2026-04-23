use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};

use super::types::{
    LoadedProjectTemplate, ProjectTemplateFile, ProjectTemplateManifest, ProjectTemplateSourceKind,
    ProjectTemplateSummary, PROJECT_TEMPLATE_MANIFEST_FILE_NAME, PROJECT_TEMPLATE_MANIFEST_SCHEMA_ID,
};

#[derive(Clone, Copy)]
struct BundledProjectTemplateFile {
    relative_path: &'static str,
    contents: &'static [u8],
}

#[derive(Clone, Copy)]
struct BundledProjectTemplateDescriptor {
    manifest_toml: &'static str,
    files: &'static [BundledProjectTemplateFile],
}

const CONDUCTOR_TEMPLATE_FILES: &[BundledProjectTemplateFile] = &[
    BundledProjectTemplateFile {
        relative_path: ".ao/workflows/custom.yaml",
        contents: include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/config/project-templates/conductor/skeleton/.ao/workflows/custom.yaml"
        )),
    },
    BundledProjectTemplateFile {
        relative_path: ".ao/workflows/standard-workflow.yaml",
        contents: include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/config/project-templates/conductor/skeleton/.ao/workflows/standard-workflow.yaml"
        )),
    },
    BundledProjectTemplateFile {
        relative_path: ".ao/workflows/hotfix-workflow.yaml",
        contents: include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/config/project-templates/conductor/skeleton/.ao/workflows/hotfix-workflow.yaml"
        )),
    },
    BundledProjectTemplateFile {
        relative_path: ".ao/workflows/research-workflow.yaml",
        contents: include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/config/project-templates/conductor/skeleton/.ao/workflows/research-workflow.yaml"
        )),
    },
    BundledProjectTemplateFile {
        relative_path: ".ao/workflows/conductor-workflow.yaml",
        contents: include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/config/project-templates/conductor/skeleton/.ao/workflows/conductor-workflow.yaml"
        )),
    },
];

const TASK_QUEUE_TEMPLATE_FILES: &[BundledProjectTemplateFile] = &[
    BundledProjectTemplateFile {
        relative_path: ".ao/workflows/custom.yaml",
        contents: include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/config/project-templates/task-queue/skeleton/.ao/workflows/custom.yaml"
        )),
    },
    BundledProjectTemplateFile {
        relative_path: ".ao/workflows/standard-workflow.yaml",
        contents: include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/config/project-templates/task-queue/skeleton/.ao/workflows/standard-workflow.yaml"
        )),
    },
    BundledProjectTemplateFile {
        relative_path: ".ao/workflows/hotfix-workflow.yaml",
        contents: include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/config/project-templates/task-queue/skeleton/.ao/workflows/hotfix-workflow.yaml"
        )),
    },
    BundledProjectTemplateFile {
        relative_path: ".ao/workflows/research-workflow.yaml",
        contents: include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/config/project-templates/task-queue/skeleton/.ao/workflows/research-workflow.yaml"
        )),
    },
];

const DIRECT_WORKFLOW_TEMPLATE_FILES: &[BundledProjectTemplateFile] = &[
    BundledProjectTemplateFile {
        relative_path: ".ao/workflows/custom.yaml",
        contents: include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/config/project-templates/direct-workflow/skeleton/.ao/workflows/custom.yaml"
        )),
    },
    BundledProjectTemplateFile {
        relative_path: ".ao/workflows/standard-workflow.yaml",
        contents: include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/config/project-templates/direct-workflow/skeleton/.ao/workflows/standard-workflow.yaml"
        )),
    },
    BundledProjectTemplateFile {
        relative_path: ".ao/workflows/hotfix-workflow.yaml",
        contents: include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/config/project-templates/direct-workflow/skeleton/.ao/workflows/hotfix-workflow.yaml"
        )),
    },
    BundledProjectTemplateFile {
        relative_path: ".ao/workflows/research-workflow.yaml",
        contents: include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/config/project-templates/direct-workflow/skeleton/.ao/workflows/research-workflow.yaml"
        )),
    },
];

const BUNDLED_PROJECT_TEMPLATES: &[BundledProjectTemplateDescriptor] = &[
    BundledProjectTemplateDescriptor {
        manifest_toml: include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/config/project-templates/conductor/template.toml"
        )),
        files: CONDUCTOR_TEMPLATE_FILES,
    },
    BundledProjectTemplateDescriptor {
        manifest_toml: include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/config/project-templates/task-queue/template.toml"
        )),
        files: TASK_QUEUE_TEMPLATE_FILES,
    },
    BundledProjectTemplateDescriptor {
        manifest_toml: include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/config/project-templates/direct-workflow/template.toml"
        )),
        files: DIRECT_WORKFLOW_TEMPLATE_FILES,
    },
];

pub fn parse_project_template_manifest(raw_toml: &str) -> Result<ProjectTemplateManifest> {
    let manifest: ProjectTemplateManifest =
        toml::from_str(raw_toml).context("failed to parse project template manifest TOML")?;
    validate_project_template_manifest(&manifest)?;
    Ok(manifest)
}

pub fn load_project_template_from_dir(template_root: &Path) -> Result<LoadedProjectTemplate> {
    let manifest_path = template_root.join(PROJECT_TEMPLATE_MANIFEST_FILE_NAME);
    load_project_template_from_file(&manifest_path)
}

pub fn load_project_template_from_file(manifest_path: &Path) -> Result<LoadedProjectTemplate> {
    let raw = fs::read_to_string(manifest_path)
        .with_context(|| format!("failed to read project template manifest at {}", manifest_path.display()))?;
    let manifest = parse_project_template_manifest(&raw)?;
    let template_root = manifest_path
        .parent()
        .ok_or_else(|| anyhow!("template manifest path '{}' has no parent directory", manifest_path.display()))?;
    let source_root = template_root.join(&manifest.source.root);
    if !source_root.is_dir() {
        return Err(anyhow!(
            "project template source root '{}' is not a directory",
            source_root.display()
        ));
    }
    let mut files = Vec::new();
    collect_template_files(&source_root, Path::new(""), &mut files)?;
    Ok(LoadedProjectTemplate {
        source_kind: ProjectTemplateSourceKind::Local,
        template_root: Some(template_root.to_path_buf()),
        manifest,
        files,
    })
}

pub fn load_bundled_project_template(template_id: &str) -> Result<LoadedProjectTemplate> {
    let descriptor = bundled_template_descriptor(template_id)
        .ok_or_else(|| anyhow!("bundled project template '{}' not found", template_id))?;
    let manifest = parse_project_template_manifest(descriptor.manifest_toml)?;
    let files = descriptor
        .files
        .iter()
        .map(|file| ProjectTemplateFile {
            relative_path: PathBuf::from(file.relative_path),
            contents: file.contents.to_vec(),
        })
        .collect::<Vec<_>>();
    Ok(LoadedProjectTemplate {
        source_kind: ProjectTemplateSourceKind::Bundled,
        template_root: None,
        manifest,
        files,
    })
}

pub fn list_bundled_project_templates() -> Result<Vec<ProjectTemplateSummary>> {
    let mut templates = BUNDLED_PROJECT_TEMPLATES
        .iter()
        .map(|descriptor| parse_project_template_manifest(descriptor.manifest_toml))
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .map(|manifest| ProjectTemplateSummary {
            id: manifest.id,
            version: manifest.version,
            title: manifest.title,
            description: manifest.description,
            pattern: manifest.pattern,
            source_kind: ProjectTemplateSourceKind::Bundled,
        })
        .collect::<Vec<_>>();
    templates.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(templates)
}

fn bundled_template_descriptor(template_id: &str) -> Option<&'static BundledProjectTemplateDescriptor> {
    BUNDLED_PROJECT_TEMPLATES.iter().find(|descriptor| {
        parse_project_template_manifest(descriptor.manifest_toml)
            .map(|manifest| manifest.id.eq_ignore_ascii_case(template_id))
            .unwrap_or(false)
    })
}

fn collect_template_files(root: &Path, relative: &Path, files: &mut Vec<ProjectTemplateFile>) -> Result<()> {
    let dir = root.join(relative);
    let mut entries = fs::read_dir(&dir)
        .with_context(|| format!("failed to read template source directory {}", dir.display()))?
        .collect::<std::result::Result<Vec<_>, _>>()
        .with_context(|| format!("failed to enumerate template source directory {}", dir.display()))?;
    entries.sort_by_key(|entry| entry.file_name());

    for entry in entries {
        let file_name = entry.file_name();
        let relative_path = relative.join(&file_name);
        let path = entry.path();
        if path.is_dir() {
            collect_template_files(root, &relative_path, files)?;
            continue;
        }

        let contents = fs::read(&path).with_context(|| format!("failed to read {}", path.display()))?;
        files.push(ProjectTemplateFile { relative_path, contents });
    }

    Ok(())
}

fn validate_project_template_manifest(manifest: &ProjectTemplateManifest) -> Result<()> {
    if manifest.schema.trim() != PROJECT_TEMPLATE_MANIFEST_SCHEMA_ID {
        return Err(anyhow!(
            "project template schema must be '{}' (got '{}')",
            PROJECT_TEMPLATE_MANIFEST_SCHEMA_ID,
            manifest.schema
        ));
    }
    if manifest.id.trim().is_empty() {
        return Err(anyhow!("project template id must not be empty"));
    }
    if manifest.version.trim().is_empty() {
        return Err(anyhow!("project template version must not be empty"));
    }
    if manifest.title.trim().is_empty() {
        return Err(anyhow!("project template title must not be empty"));
    }
    if manifest.pattern.trim().is_empty() {
        return Err(anyhow!("project template pattern must not be empty"));
    }
    if manifest.source.root.trim().is_empty() {
        return Err(anyhow!("project template source.root must not be empty"));
    }
    if Path::new(&manifest.source.root).is_absolute() {
        return Err(anyhow!("project template source.root must be relative"));
    }
    for pack in &manifest.packs {
        if pack.id.trim().is_empty() {
            return Err(anyhow!("project template pack id must not be empty"));
        }
        if let Some(version) = pack.version.as_deref() {
            if version.trim().is_empty() {
                return Err(anyhow!("project template pack version must not be empty when provided"));
            }
        }
    }
    for step in &manifest.next_steps {
        if step.trim().is_empty() {
            return Err(anyhow!("project template next_steps entries must not be empty"));
        }
    }
    Ok(())
}
