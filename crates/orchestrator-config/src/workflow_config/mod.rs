//! Workflow config: TYPES + standardized YAML PARSER now live in the canonical
//! `animus-config-protocol` crate. This kernel module is the COMPILER
//! (pack-overlay merge, validation, state-machine derivation, disk cache,
//! loading orchestration, the config_source plugin client) plus thin re-exports
//! of the moved types/parser so the kernel's ~hundreds of
//! `crate::workflow_config::*` / `crate::workflow_config::types::*` reference
//! sites keep compiling unchanged.

pub mod config_source_client;
pub mod config_write;
pub mod environment_routing;
pub mod loading;
pub mod resolution;
pub mod validation;
pub mod yaml_compiler;
pub mod yaml_scaffold;

#[cfg(test)]
mod tests;

// ---------------------------------------------------------------------------
// Re-exports of the moved TYPES + PARSER (canonical home:
// `animus-config-protocol`).
// ---------------------------------------------------------------------------

/// The `WorkflowConfig` types, re-exported so internal paths like
/// `crate::workflow_config::types::Foo` keep resolving.
pub mod types {
    pub use animus_config_protocol::workflow_types::*;
}

pub use animus_config_protocol::builtins::{builtin_workflow_config, builtin_workflow_config_base};
pub use animus_config_protocol::env_interp::{interpolate_env, interpolate_env_with, lint_sensitive_interpolations};
pub use animus_config_protocol::parse::{
    collect_project_yaml_workflow_sources, compile_yaml_sources_confined_to_pack, compile_yaml_sources_with_base,
    compile_yaml_workflow_files, merge_yaml_into_config, yaml_workflows_dir, YAML_WORKFLOWS_DIR,
};
pub use animus_config_protocol::workflow_types::*;
pub use animus_config_protocol::yaml_diagnostic::{
    closest_match, edit_distance, wrap_serde_yaml_error, YamlDiagnostic, YamlExcerpt,
};
pub use animus_config_protocol::yaml_parser::{
    parse_yaml_workflow_config, parse_yaml_workflow_config_with_base, parse_yaml_workflow_config_with_base_and_source,
    resolve_agent_system_prompt_files_confined_to_pack,
};
pub use animus_config_protocol::yaml_types::{
    title_case_phase_id, DEFAULT_WORKFLOW_TEMPLATE_FILE_NAME, GENERATED_RUNTIME_OVERLAY_FILE_NAME,
    GENERATED_WORKFLOW_OVERLAY_FILE_NAME, HOTFIX_WORKFLOW_TEMPLATE_FILE_NAME, RESEARCH_WORKFLOW_TEMPLATE_FILE_NAME,
    STANDARD_WORKFLOW_TEMPLATE_FILE_NAME,
};

// Generated-overlay readers/writers (live with the YAML types in the protocol
// crate); re-exported so the kernel + CLI keep their authoring surface.
pub use animus_config_protocol::overlay::{
    generated_workflow_phase_is_defined, remove_generated_workflow_phase, upsert_generated_workflow_phase,
    upsert_generated_workflow_pipeline, write_workflow_yaml_overlay,
};

// ---------------------------------------------------------------------------
// Kernel-owned compiler / loading / validation surface.
// ---------------------------------------------------------------------------

pub use config_write::{
    remove_agent_profile, remove_workflow_definition, upsert_agent_profile, upsert_workflow_definition,
    write_full_workflow_config,
};
pub use environment_routing::resolve_environment;
pub use loading::{
    ensure_workflow_config_compiled, ensure_workflow_config_file, legacy_workflow_config_paths, load_workflow_config,
    load_workflow_config_or_default, load_workflow_config_or_default_for_actor, load_workflow_config_with_metadata,
    try_load_workflow_config, workflow_config_hash, workflow_config_path, write_workflow_config,
    WorkflowConfigAvailability,
};
pub use resolution::{
    resolve_workflow_phase_plan, resolve_workflow_rework_attempts, resolve_workflow_skip_guards,
    resolve_workflow_verdict_routing,
};
pub use validation::{
    missing_project_skill_reference_warnings, missing_skill_reference_warnings_for_sources,
    missing_skill_yaml_warnings, unenforced_project_yaml_warnings, unenforced_yaml_field_warnings,
    validate_workflow_and_runtime_configs, validate_workflow_and_runtime_configs_with_project_root,
    validate_workflow_config, validate_workflow_config_with_project_root, SkillReferenceWarning,
    UnenforcedFieldWarning,
};
pub use yaml_compiler::{validate_and_compile_yaml_workflows, CompileYamlResult};
pub use yaml_scaffold::ensure_workflow_yaml_scaffold;
