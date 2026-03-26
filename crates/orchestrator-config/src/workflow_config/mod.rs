pub mod builtins;
pub mod loading;
pub mod resolution;
pub mod types;
pub mod validation;
pub mod yaml_compiler;
mod yaml_parser;
pub mod yaml_scaffold;
mod yaml_types;

#[cfg(test)]
mod tests;

pub use builtins::builtin_workflow_config;
pub use loading::{
    ensure_workflow_config_compiled, ensure_workflow_config_file, legacy_workflow_config_paths, load_workflow_config,
    load_workflow_config_or_default, load_workflow_config_with_metadata, workflow_config_hash, workflow_config_path,
    write_workflow_config,
};
pub use resolution::{
    resolve_workflow_phase_plan, resolve_workflow_rework_attempts, resolve_workflow_skip_guards,
    resolve_workflow_verdict_routing,
};
pub use types::*;
pub use validation::{
    validate_workflow_and_runtime_configs, validate_workflow_and_runtime_configs_with_project_root,
    validate_workflow_config, validate_workflow_config_with_project_root,
};
pub(crate) use yaml_compiler::{collect_project_yaml_workflow_sources, compile_yaml_sources_with_base};
pub use yaml_compiler::{
    compile_yaml_workflow_files, merge_yaml_into_config, validate_and_compile_yaml_workflows,
    write_workflow_yaml_overlay, yaml_workflows_dir, CompileYamlResult,
};
pub use yaml_parser::{parse_yaml_workflow_config, parse_yaml_workflow_config_with_base};
pub use yaml_scaffold::{ensure_workflow_yaml_scaffold, title_case_phase_id};
pub use yaml_types::{
    DEFAULT_WORKFLOW_TEMPLATE_FILE_NAME, GENERATED_RUNTIME_OVERLAY_FILE_NAME, GENERATED_WORKFLOW_OVERLAY_FILE_NAME,
    HOTFIX_WORKFLOW_TEMPLATE_FILE_NAME, RESEARCH_WORKFLOW_TEMPLATE_FILE_NAME, STANDARD_WORKFLOW_TEMPLATE_FILE_NAME,
    YAML_WORKFLOWS_DIR,
};
