use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use super::yaml_compiler::yaml_workflows_dir;
use super::yaml_types::*;

pub fn default_workflow_template_files() -> [(&'static str, &'static str); 4] {
    [
        (
            DEFAULT_WORKFLOW_TEMPLATE_FILE_NAME,
            r#"# Project-local workflow extensions and overrides.
tools_allowlist:
  - cargo
"#,
        ),
        (
            STANDARD_WORKFLOW_TEMPLATE_FILE_NAME,
            r#"workflows:
  - id: standard-workflow
    name: Standard Workflow
    description: Default task delivery workflow for this repository.
    phases:
      - workflow_ref: ao.task/standard
"#,
        ),
        (
            HOTFIX_WORKFLOW_TEMPLATE_FILE_NAME,
            r#"workflows:
  - id: hotfix-workflow
    name: Hotfix Workflow
    description: Fast-track workflow for urgent fixes.
    phases:
      - workflow_ref: ao.task/quick-fix
"#,
        ),
        (
            RESEARCH_WORKFLOW_TEMPLATE_FILE_NAME,
            r#"workflows:
  - id: research-workflow
    name: Research Workflow
    description: Validate scope and produce findings without landing implementation changes.
    phases:
      - workflow_ref: ao.task/triage
      - requirements
"#,
        ),
    ]
}

pub fn ensure_workflow_yaml_scaffold(project_root: &Path) -> Result<Vec<PathBuf>> {
    let workflows_dir = yaml_workflows_dir(project_root);
    fs::create_dir_all(&workflows_dir).with_context(|| format!("failed to create {}", workflows_dir.display()))?;

    let single_file = project_root.join(".ao").join("workflows.yaml");
    let has_existing_yaml = single_file.exists()
        || fs::read_dir(&workflows_dir)
            .with_context(|| format!("failed to read {}", workflows_dir.display()))?
            .filter_map(|entry| entry.ok())
            .any(|entry| entry.path().extension().map(|ext| ext == "yaml" || ext == "yml").unwrap_or(false));

    if has_existing_yaml {
        return Ok(Vec::new());
    }

    let mut created = Vec::new();
    for (file_name, content) in default_workflow_template_files() {
        let path = workflows_dir.join(file_name);
        fs::write(&path, content).with_context(|| format!("failed to write {}", path.display()))?;
        created.push(path);
    }
    Ok(created)
}

pub fn title_case_phase_id(phase_id: &str) -> String {
    phase_id
        .split(['-', '_'])
        .filter(|part| !part.trim().is_empty())
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => {
                    let mut label = first.to_ascii_uppercase().to_string();
                    label.push_str(chars.as_str());
                    label
                }
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}
