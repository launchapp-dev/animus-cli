pub mod builtins;
pub mod config_source_client;
pub mod env_interp;
pub mod loading;
pub mod resolution;
pub mod types;
pub mod validation;
pub mod yaml_compiler;
pub mod yaml_diagnostic;
mod yaml_parser;
pub mod yaml_scaffold;
mod yaml_types;

#[cfg(test)]
mod tests;

pub use builtins::builtin_workflow_config;
pub use env_interp::{
    clear_workflow_secret_resolver_for_test, install_workflow_secret_resolver,
    install_workflow_secret_resolver_for_test, WorkflowSecretResolver,
};
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
    missing_project_skill_reference_warnings, missing_skill_reference_warnings_for_sources,
    missing_skill_yaml_warnings, unenforced_project_yaml_warnings, unenforced_yaml_field_warnings,
    validate_workflow_and_runtime_configs, validate_workflow_and_runtime_configs_with_project_root,
    validate_workflow_config, validate_workflow_config_with_project_root, SkillReferenceWarning,
    UnenforcedFieldWarning,
};
pub(crate) use yaml_compiler::{
    collect_project_yaml_workflow_sources, compile_yaml_sources_confined_to_pack, compile_yaml_sources_with_base,
};
pub use yaml_compiler::{
    compile_yaml_workflow_files, generated_workflow_phase_is_defined, merge_yaml_into_config,
    remove_generated_workflow_phase, upsert_generated_workflow_phase, upsert_generated_workflow_pipeline,
    validate_and_compile_yaml_workflows, write_workflow_yaml_overlay, yaml_workflows_dir, CompileYamlResult,
};
pub use yaml_diagnostic::{closest_match, edit_distance, wrap_serde_yaml_error, YamlDiagnostic, YamlExcerpt};
pub(crate) use yaml_parser::resolve_agent_system_prompt_files_confined_to_pack;
pub use yaml_parser::{
    parse_yaml_workflow_config, parse_yaml_workflow_config_with_base, parse_yaml_workflow_config_with_base_and_source,
};
pub use yaml_scaffold::{ensure_workflow_yaml_scaffold, title_case_phase_id};
pub use yaml_types::{
    DEFAULT_WORKFLOW_TEMPLATE_FILE_NAME, GENERATED_RUNTIME_OVERLAY_FILE_NAME, GENERATED_WORKFLOW_OVERLAY_FILE_NAME,
    HOTFIX_WORKFLOW_TEMPLATE_FILE_NAME, RESEARCH_WORKFLOW_TEMPLATE_FILE_NAME, STANDARD_WORKFLOW_TEMPLATE_FILE_NAME,
    YAML_WORKFLOWS_DIR,
};
