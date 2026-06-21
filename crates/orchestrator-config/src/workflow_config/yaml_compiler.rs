use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Result;

use super::types::WorkflowConfig;
use animus_config_protocol::parse::yaml_workflows_dir;

pub struct CompileYamlResult {
    pub config: WorkflowConfig,
    pub source_files: Vec<PathBuf>,
    pub output_path: PathBuf,
}

pub fn validate_and_compile_yaml_workflows(project_root: &Path) -> Result<Option<CompileYamlResult>> {
    let workflows_dir = yaml_workflows_dir(project_root);
    let single_file = project_root.join(".animus").join("workflows.yaml");

    let mut source_files: Vec<PathBuf> = Vec::new();
    if single_file.exists() {
        source_files.push(single_file.clone());
    }
    if workflows_dir.is_dir() {
        if let Ok(entries) = fs::read_dir(&workflows_dir) {
            for entry in entries.filter_map(|e| e.ok()) {
                let path = entry.path();
                if path.extension().map(|ext| ext == "yaml" || ext == "yml").unwrap_or(false) {
                    source_files.push(path);
                }
            }
        }
    }
    source_files.sort();

    if source_files.is_empty() {
        return Ok(None);
    }

    // v0.6 bootstrap/dev compile path: source the base from the in-tree YAML
    // library parser (no `config_source` plugin required), then run the kernel
    // compiler (pack-overlay merge + validate). The daemon runtime load path
    // still requires a plugin; this dev/bootstrap surface does not.
    let final_config = super::loading::compile_workflow_config_from_library(project_root)?
        .map(|loaded| loaded.config)
        .unwrap_or_else(super::builtin_workflow_config);
    let output_path = if single_file.exists() { single_file } else { workflows_dir };
    Ok(Some(CompileYamlResult { config: final_config, source_files, output_path }))
}
