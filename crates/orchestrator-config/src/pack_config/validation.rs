use std::collections::BTreeSet;
use std::path::{Component, Path};

use anyhow::{anyhow, Result};
use semver::{Version, VersionReq};

use super::types::{
    PackDependency, PackManifest, PackMcp, PackNativeModule, PackRuntimeRequirement, PackSchedules, PackSecrets,
    PackSubjects, PACK_MANIFEST_SCHEMA_ID,
};

pub fn validate_pack_manifest(manifest: &PackManifest) -> Result<()> {
    let mut errors = Vec::new();

    if manifest.schema.trim() != PACK_MANIFEST_SCHEMA_ID {
        errors.push(format!("schema must be '{}' (got '{}')", PACK_MANIFEST_SCHEMA_ID, manifest.schema));
    }

    validate_pack_id(&manifest.id, "id", &mut errors);
    validate_semver_version(&manifest.version, "version", &mut errors);

    if manifest.title.trim().is_empty() {
        errors.push("title must not be empty".to_string());
    }

    validate_relative_path(&manifest.workflows.root, "workflows.root", &mut errors);
    if manifest.workflows.exports.is_empty() {
        errors.push("workflows.exports must include at least one workflow ref".to_string());
    } else {
        validate_exports(&manifest.id, &manifest.workflows.exports, &mut errors);
    }

    if let Some(subjects) = manifest.subjects.as_ref() {
        validate_subjects(subjects, &mut errors);
    }

    if let Some(agent_overlay) = manifest.runtime.agent_overlay.as_deref() {
        validate_relative_path(agent_overlay, "runtime.agent_overlay", &mut errors);
    }
    if let Some(workflow_overlay) = manifest.runtime.workflow_overlay.as_deref() {
        validate_relative_path(workflow_overlay, "runtime.workflow_overlay", &mut errors);
    }
    validate_runtime_requirements(&manifest.runtime.requirements, &mut errors);

    if let Some(mcp) = manifest.mcp.as_ref() {
        validate_mcp(mcp, &mut errors);
    }

    if let Some(schedules) = manifest.schedules.as_ref() {
        validate_schedules(schedules, &mut errors);
    }

    validate_dependencies(&manifest.id, &manifest.dependencies, &mut errors);
    validate_permissions(&manifest.permissions.tools, "permissions.tools", false, true, &mut errors);
    validate_permissions(&manifest.permissions.mcp_namespaces, "permissions.mcp_namespaces", true, false, &mut errors);
    validate_secrets(&manifest.secrets, &mut errors);

    if let Some(native_module) = manifest.native_module.as_ref() {
        validate_native_module(native_module, &mut errors);
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(anyhow!(errors.join("; ")))
    }
}

pub fn validate_pack_manifest_assets(pack_root: &Path, manifest: &PackManifest) -> Result<()> {
    let mut errors = Vec::new();

    validate_existing_relative_path(pack_root, &manifest.workflows.root, "workflows.root", true, &mut errors);

    if let Some(agent_overlay) = manifest.runtime.agent_overlay.as_deref() {
        validate_existing_relative_path(pack_root, agent_overlay, "runtime.agent_overlay", false, &mut errors);
    }
    if let Some(workflow_overlay) = manifest.runtime.workflow_overlay.as_deref() {
        validate_existing_relative_path(pack_root, workflow_overlay, "runtime.workflow_overlay", false, &mut errors);
    }

    if let Some(mcp) = manifest.mcp.as_ref() {
        if let Some(servers) = mcp.servers.as_deref() {
            validate_existing_relative_path(pack_root, servers, "mcp.servers", false, &mut errors);
        }
        if let Some(tools) = mcp.tools.as_deref() {
            validate_existing_relative_path(pack_root, tools, "mcp.tools", false, &mut errors);
        }
    }

    if let Some(schedules) = manifest.schedules.as_ref() {
        validate_existing_relative_path(pack_root, &schedules.file, "schedules.file", false, &mut errors);
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(anyhow!(errors.join("; ")))
    }
}

fn validate_subjects(subjects: &PackSubjects, errors: &mut Vec<String>) {
    if subjects.kinds.is_empty() {
        errors.push("subjects.kinds must include at least one subject kind".to_string());
        return;
    }

    let mut seen = BTreeSet::new();
    for kind in &subjects.kinds {
        validate_subject_kind(kind, "subjects.kinds", errors);
        let normalized = kind.trim().to_ascii_lowercase();
        if !normalized.is_empty() && !seen.insert(normalized) {
            errors.push(format!("subjects.kinds contains duplicate subject kind '{}'", kind));
        }
    }

    if let Some(default_kind) = subjects.default_kind.as_deref() {
        validate_subject_kind(default_kind, "subjects.default_kind", errors);
        if subjects.kinds.iter().all(|kind| !kind.eq_ignore_ascii_case(default_kind)) {
            errors.push(format!("subjects.default_kind '{}' must be listed in subjects.kinds", default_kind));
        }
    }
}

fn validate_exports(pack_id: &str, exports: &[String], errors: &mut Vec<String>) {
    let mut seen = BTreeSet::new();
    let expected_prefix = format!("{pack_id}/");
    for export in exports {
        let trimmed = export.trim();
        if trimmed.is_empty() {
            errors.push("workflows.exports must not contain empty workflow refs".to_string());
            continue;
        }
        if !trimmed.starts_with(&expected_prefix) {
            errors.push(format!("workflow export '{}' must be prefixed with '{}/'", trimmed, pack_id));
        }
        let normalized = trimmed.to_ascii_lowercase();
        if !seen.insert(normalized) {
            errors.push(format!("workflows.exports contains duplicate workflow ref '{}'", trimmed));
        }
    }
}

fn validate_runtime_requirements(requirements: &[PackRuntimeRequirement], errors: &mut Vec<String>) {
    let mut seen = BTreeSet::new();

    for requirement in requirements {
        let key = format!(
            "{}:{}",
            requirement.runtime.binary_name(),
            requirement.binary.as_deref().unwrap_or_default().trim().to_ascii_lowercase()
        );
        if !seen.insert(key) {
            errors.push(format!(
                "runtime.requirements contains a duplicate '{}' runtime declaration",
                requirement.runtime.binary_name()
            ));
        }

        if let Some(binary) = requirement.binary.as_deref() {
            let trimmed = binary.trim();
            if trimmed.is_empty() {
                errors.push(format!(
                    "runtime.requirements['{}'].binary must not be empty",
                    requirement.runtime.binary_name()
                ));
            } else if !is_simple_binary_name(trimmed) {
                errors.push(format!(
                    "runtime.requirements['{}'].binary '{}' must be a simple executable name, not a path",
                    requirement.runtime.binary_name(),
                    trimmed
                ));
            }
        }

        if let Some(version) = requirement.version.as_deref() {
            if version.trim().is_empty() {
                errors.push(format!(
                    "runtime.requirements['{}'].version must not be empty",
                    requirement.runtime.binary_name()
                ));
            } else if let Err(error) = VersionReq::parse(version.trim()) {
                errors.push(format!(
                    "runtime.requirements['{}'].version '{}' is not a valid semver requirement: {}",
                    requirement.runtime.binary_name(),
                    version.trim(),
                    error
                ));
            }
        }

        if let Some(reason) = requirement.reason.as_deref() {
            if reason.trim().is_empty() {
                errors.push(format!(
                    "runtime.requirements['{}'].reason must not be empty when provided",
                    requirement.runtime.binary_name()
                ));
            }
        }
    }
}

fn is_simple_binary_name(value: &str) -> bool {
    let mut components = Path::new(value).components();
    matches!(components.next(), Some(Component::Normal(_))) && components.next().is_none()
}

fn validate_mcp(mcp: &PackMcp, errors: &mut Vec<String>) {
    if mcp.servers.is_none() && mcp.tools.is_none() {
        errors.push("mcp section must declare at least one of mcp.servers or mcp.tools".to_string());
    }

    if let Some(servers) = mcp.servers.as_deref() {
        validate_relative_path(servers, "mcp.servers", errors);
    }

    if let Some(tools) = mcp.tools.as_deref() {
        validate_relative_path(tools, "mcp.tools", errors);
    }
}

fn validate_schedules(schedules: &PackSchedules, errors: &mut Vec<String>) {
    validate_relative_path(&schedules.file, "schedules.file", errors);
}

fn validate_dependencies(pack_id: &str, dependencies: &[PackDependency], errors: &mut Vec<String>) {
    let mut seen = BTreeSet::new();

    for dependency in dependencies {
        validate_pack_id(&dependency.id, "dependencies.id", errors);

        let normalized = dependency.id.trim().to_ascii_lowercase();
        if !normalized.is_empty() && !seen.insert(normalized.clone()) {
            errors.push(format!("dependencies contains duplicate pack id '{}'", dependency.id));
        }

        if normalized == pack_id.trim().to_ascii_lowercase() {
            errors.push(format!("dependencies must not include self-reference '{}'", dependency.id));
        }

        if let Some(version) = dependency.version.as_deref() {
            validate_version_req(version, &format!("dependencies['{}'].version", dependency.id), errors);
        }

        if let Some(reason) = dependency.reason.as_deref() {
            if reason.trim().is_empty() {
                errors.push(format!("dependencies['{}'].reason must not be empty when provided", dependency.id));
            }
        }
    }
}

fn validate_permissions(
    values: &[String],
    field: &str,
    allow_dot: bool,
    allow_uppercase: bool,
    errors: &mut Vec<String>,
) {
    let mut seen = BTreeSet::new();
    for value in values {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            errors.push(format!("{field} must not contain empty values"));
            continue;
        }
        if !is_identifier(trimmed, allow_dot, allow_uppercase) {
            errors.push(format!("{field} contains invalid identifier '{}'", trimmed));
        }
        let normalized = trimmed.to_ascii_lowercase();
        if !seen.insert(normalized) {
            errors.push(format!("{field} contains duplicate value '{}'", trimmed));
        }
    }
}

fn validate_secrets(secrets: &PackSecrets, errors: &mut Vec<String>) {
    let mut seen = BTreeSet::new();
    for (field, values) in [("secrets.required", &secrets.required), ("secrets.optional", &secrets.optional)] {
        for value in values {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                errors.push(format!("{field} must not contain empty values"));
                continue;
            }
            if !is_secret_name(trimmed) {
                errors.push(format!("{field} contains invalid secret name '{}'", trimmed));
            }
            let normalized = trimmed.to_ascii_uppercase();
            if !seen.insert(normalized) {
                errors.push(format!("secret '{}' must not appear in multiple secret lists", trimmed));
            }
        }
    }
}

fn validate_native_module(native_module: &PackNativeModule, errors: &mut Vec<String>) {
    if native_module.feature.trim().is_empty() {
        errors.push("native_module.feature must not be empty".to_string());
    } else if !is_feature_name(native_module.feature.trim()) {
        errors.push(format!(
            "native_module.feature '{}' must use lowercase letters, numbers, '-' or '_'",
            native_module.feature
        ));
    }

    validate_pack_id(&native_module.module_id, "native_module.module_id", errors);
}

fn validate_existing_relative_path(
    pack_root: &Path,
    raw_path: &str,
    field: &str,
    expect_directory: bool,
    errors: &mut Vec<String>,
) {
    if !validate_relative_path(raw_path, field, errors) {
        return;
    }

    let target = pack_root.join(raw_path.trim());
    if !target.exists() {
        errors.push(format!("{field} points to missing path '{}'", target.display()));
        return;
    }

    if expect_directory && !target.is_dir() {
        errors.push(format!("{field} must point to a directory (got '{}')", target.display()));
    }

    if !expect_directory && !target.is_file() {
        errors.push(format!("{field} must point to a file (got '{}')", target.display()));
    }
}

fn validate_relative_path(raw_path: &str, field: &str, errors: &mut Vec<String>) -> bool {
    let trimmed = raw_path.trim();
    if trimmed.is_empty() {
        errors.push(format!("{field} must not be empty"));
        return false;
    }

    let path = Path::new(trimmed);
    if path.is_absolute() {
        errors.push(format!("{field} must be relative to the pack root (got '{}')", trimmed));
        return false;
    }

    if path.components().next().is_none() {
        errors.push(format!("{field} must not be empty"));
        return false;
    }

    for component in path.components() {
        match component {
            Component::Normal(_) => {}
            Component::CurDir | Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                errors.push(format!(
                    "{field} must stay within the pack root without '.' or '..' segments (got '{}')",
                    trimmed
                ));
                return false;
            }
        }
    }

    true
}

fn validate_pack_id(raw: &str, field: &str, errors: &mut Vec<String>) {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        errors.push(format!("{field} must not be empty"));
        return;
    }
    if !is_identifier(trimmed, true, false) {
        errors.push(format!("{field} '{}' must use lowercase letters, numbers, '.', '-' or '_'", trimmed));
    }
}

fn validate_subject_kind(raw: &str, field: &str, errors: &mut Vec<String>) {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        errors.push(format!("{field} must not contain empty subject kinds"));
        return;
    }
    if !is_identifier(trimmed, true, false) {
        errors.push(format!(
            "{field} contains invalid subject kind '{}'; use lowercase letters, numbers, '.', '-' or '_'",
            trimmed
        ));
    }
}

fn validate_semver_version(raw: &str, field: &str, errors: &mut Vec<String>) {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        errors.push(format!("{field} must not be empty"));
        return;
    }

    if let Err(error) = Version::parse(trimmed) {
        errors.push(format!("{field} '{}' is not valid semver: {}", trimmed, error));
    }
}

fn validate_version_req(raw: &str, field: &str, errors: &mut Vec<String>) {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        errors.push(format!("{field} must not be empty"));
        return;
    }

    if let Err(error) = VersionReq::parse(trimmed) {
        errors.push(format!("{field} '{}' is not a valid semver requirement: {}", trimmed, error));
    }
}

fn is_identifier(value: &str, allow_dot: bool, allow_uppercase: bool) -> bool {
    if value.chars().next().is_none_or(|ch| !ch.is_ascii_alphanumeric()) {
        return false;
    }

    let mut previous_separator = false;
    for ch in value.chars() {
        if !is_identifier_char(ch, allow_dot, allow_uppercase) {
            return false;
        }

        let is_separator = ch == '-' || ch == '_' || (allow_dot && ch == '.');
        if is_separator && previous_separator {
            return false;
        }
        previous_separator = is_separator;
    }

    !previous_separator
}

fn is_identifier_char(ch: char, allow_dot: bool, allow_uppercase: bool) -> bool {
    ch.is_ascii_lowercase()
        || ch.is_ascii_digit()
        || (allow_uppercase && ch.is_ascii_uppercase())
        || ch == '-'
        || ch == '_'
        || (allow_dot && ch == '.')
}

fn is_secret_name(value: &str) -> bool {
    value.chars().all(|ch| ch.is_ascii_uppercase() || ch.is_ascii_digit() || ch == '_')
        && value.chars().next().is_some_and(|ch| ch.is_ascii_uppercase())
}

fn is_feature_name(value: &str) -> bool {
    value.chars().all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-' || ch == '_')
        && value.chars().next().is_some_and(|ch| ch.is_ascii_lowercase())
}
