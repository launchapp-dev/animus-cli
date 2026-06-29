use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use animus_config_protocol::parse::yaml_workflows_dir;
use animus_config_protocol::yaml_types::{
    DEFAULT_WORKFLOW_TEMPLATE_FILE_NAME, HOTFIX_WORKFLOW_TEMPLATE_FILE_NAME, RESEARCH_WORKFLOW_TEMPLATE_FILE_NAME,
    STANDARD_WORKFLOW_TEMPLATE_FILE_NAME,
};

// Default workflow templates — TEST FIXTURES ONLY. The kernel no longer
// scaffolds these into real projects (see `ensure_workflow_yaml_scaffold`);
// they survive solely so the `test-utils` config_source seam can give tests a
// standard workflow to load.
#[cfg(any(test, feature = "test-utils"))]
pub fn default_workflow_template_files() -> [(&'static str, &'static str); 4] {
    [
        (
            DEFAULT_WORKFLOW_TEMPLATE_FILE_NAME,
            r#"default_workflow_ref: standard-workflow

# Project-local workflow extensions and overrides.
tools_allowlist:
  - cargo

# v0.6 kernel-purification: the kernel ships ZERO baked agents/phases. This
# scaffold defines a minimal, self-contained starter set so a fresh project is
# valid and runnable; installed packs and your own edits extend or override it.
agents:
  default:
    description: Default workflow phase agent profile
    system_prompt: >-
      You are the workflow phase execution agent. Produce deterministic,
      repository-safe outputs and keep changes scoped to the active phase.

phases:
  requirements:
    mode: agent
    agent_id: default
    directive: Clarify implementation scope, constraints, and acceptance criteria.
  research:
    mode: agent
    agent_id: default
    directive: Gather codebase and external evidence to de-risk the next step.
  implementation:
    mode: agent
    agent_id: default
    directive: Implement production-quality code for this task. Keep changes focused and executable.
  code-review:
    mode: agent
    agent_id: default
    directive: Review quality, risks, and maintainability before completion.
  testing:
    mode: agent
    agent_id: default
    directive: Run and update test coverage for the delivered changes.

# UI/metadata registry mirrored from `phases:` so `animus workflow phases list`
# can display the scaffolded phases. Packs and edits can extend this.
phase_catalog:
  requirements:
    label: Requirements
    description: Clarify scope, constraints, and acceptance criteria.
    category: planning
  research:
    label: Research
    description: Gather implementation evidence and references.
    category: planning
  implementation:
    label: Implementation
    description: Deliver production-quality implementation changes.
    category: build
  code-review:
    label: Code Review
    description: Review quality, risks, and maintainability before completion.
    category: review
  testing:
    label: Testing
    description: Run and update test coverage for the delivered changes.
    category: qa
"#,
        ),
        (
            STANDARD_WORKFLOW_TEMPLATE_FILE_NAME,
            r#"workflows:
  - id: standard-workflow
    name: Standard Workflow
    description: Default task delivery workflow for this repository.
    phases:
      - requirements
      - implementation
      - code-review
      - testing
"#,
        ),
        (
            HOTFIX_WORKFLOW_TEMPLATE_FILE_NAME,
            r#"workflows:
  - id: hotfix-workflow
    name: Hotfix Workflow
    description: Fast-track workflow for urgent fixes.
    phases:
      - implementation
      - code-review
      - testing
"#,
        ),
        (
            RESEARCH_WORKFLOW_TEMPLATE_FILE_NAME,
            r#"workflows:
  - id: research-workflow
    name: Research Workflow
    description: Validate scope and produce findings without landing implementation changes.
    phases:
      - requirements
      - research
"#,
        ),
    ]
}

/// v0.6: the kernel ships ZERO baked workflow content. Workflows come from the
/// active flavor / `config_source` plugin (e.g. config-postgres team_*, or
/// config-yaml reading author-provided `.animus/workflows/*.yaml`), never from
/// kernel-scaffolded defaults. This ensures the workflows directory exists but
/// writes no template files — so deleting workflow YAML does NOT get silently
/// repopulated with bundled defaults, and an empty project stays empty until the
/// flavor/config_source provides workflows.
pub fn ensure_workflow_yaml_scaffold(project_root: &Path) -> Result<Vec<PathBuf>> {
    let workflows_dir = yaml_workflows_dir(project_root);
    fs::create_dir_all(&workflows_dir).with_context(|| format!("failed to create {}", workflows_dir.display()))?;
    Ok(Vec::new())
}

/// TEST ONLY: write the default workflow templates into a project (the historical
/// `ensure_workflow_yaml_scaffold` behavior). Production no longer scaffolds
/// baked defaults; the `test-utils` config_source seam uses this so tests that
/// rely on the standard workflow keep working. Only writes when no workflow YAML
/// already exists, so tests that author their own YAML are not clobbered.
#[cfg(any(test, feature = "test-utils"))]
pub fn scaffold_default_workflows_for_tests(project_root: &Path) -> Result<Vec<PathBuf>> {
    let workflows_dir = yaml_workflows_dir(project_root);
    fs::create_dir_all(&workflows_dir).with_context(|| format!("failed to create {}", workflows_dir.display()))?;

    let single_file = project_root.join(".animus").join("workflows.yaml");
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
