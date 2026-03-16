use std::fs;

use crate::test_support::{env_lock, EnvVarGuard};
use crate::workflow_config::builtin_workflow_config;

use super::{
    activate_pack_mcp_overlay, apply_pack_mcp_overlay, check_pack_runtime_requirements,
    ensure_pack_runtime_requirements, load_pack_manifest, load_pack_mcp_overlay, parse_pack_manifest,
    validate_pack_manifest, validate_pack_manifest_assets, ExternalRuntimeKind, PackManifest, PackRuntimeCheckStatus,
    PackRuntimeRequirement, PACK_MANIFEST_FILE_NAME,
};

fn valid_manifest_toml() -> &'static str {
    r#"
schema = "ao.pack.v1"
id = "ao.requirements"
version = "0.1.0"
kind = "domain-pack"
title = "AO Requirements"
description = "Planning and materialization workflows for requirements."

[ownership]
mode = "bundled"

[compatibility]
ao_core = ">=0.1.0"
workflow_schema = "v2"
subject_schema = "v2"

[subjects]
kinds = ["ao.requirement"]
default_kind = "ao.requirement"

[workflows]
root = "workflows"
exports = [
  "ao.requirements/draft",
  "ao.requirements/refine",
  "ao.requirements/execute",
]

[runtime]
agent_overlay = "runtime/agent-runtime.overlay.yaml"
workflow_overlay = "runtime/workflow-runtime.overlay.yaml"

[[runtime.requirements]]
runtime = "python"
version = ">=3.11.0"
optional = false
reason = "Execute requirement-pack helpers."

[[runtime.requirements]]
runtime = "uv"
optional = true

[mcp]
servers = "mcp/servers.toml"
tools = "mcp/tools.toml"

[schedules]
file = "schedules/schedules.yaml"

[[dependencies]]
id = "ao.task"
version = ">=0.1.0"
reason = "Materialize tasks from approved requirements."

[permissions]
tools = ["git", "cargo"]
mcp_namespaces = ["ao", "github"]

[secrets]
required = ["OPENAI_API_KEY"]
optional = ["GITHUB_TOKEN"]

[native_module]
feature = "plugin-ao-requirements"
module_id = "ao.requirements"
optional = true
"#
}

fn write_valid_pack_fixture(root: &std::path::Path) {
    fs::create_dir_all(root.join("workflows")).expect("create workflows");
    fs::create_dir_all(root.join("runtime")).expect("create runtime");
    fs::create_dir_all(root.join("mcp")).expect("create mcp");
    fs::create_dir_all(root.join("schedules")).expect("create schedules");

    fs::write(root.join(PACK_MANIFEST_FILE_NAME), valid_manifest_toml()).expect("write manifest");
    fs::write(root.join("runtime/agent-runtime.overlay.yaml"), "agents: {}\n").expect("write agent overlay");
    fs::write(root.join("runtime/workflow-runtime.overlay.yaml"), "workflows: []\n").expect("write workflow overlay");
    fs::write(root.join("mcp/servers.toml"), "[[server]]\nid = 'ao'\ncommand = 'node'\n").expect("write servers");
    fs::write(root.join("mcp/tools.toml"), "[phase.research]\nservers = ['ao']\n").expect("write tools");
    fs::write(root.join("schedules/schedules.yaml"), "schedules: []\n").expect("write schedules");
}

#[test]
fn parse_pack_manifest_accepts_valid_manifest() {
    let manifest = parse_pack_manifest(valid_manifest_toml()).expect("valid manifest should parse");
    assert_eq!(manifest.id, "ao.requirements");
    assert_eq!(manifest.version, "0.1.0");
    assert_eq!(manifest.workflows.exports.len(), 3);
    assert_eq!(manifest.runtime.requirements.len(), 2);
}

#[test]
fn validate_pack_manifest_rejects_invalid_semver_and_export_prefix() {
    let mut manifest: PackManifest = toml::from_str(valid_manifest_toml()).expect("deserialize manifest");
    manifest.id = "-bad-pack".to_string();
    manifest.version = "not-semver".to_string();
    manifest.workflows.exports[0] = "builtin/requirements-execute".to_string();

    let error = validate_pack_manifest(&manifest).expect_err("invalid manifest should fail");
    let message = error.to_string();
    assert!(message.contains("id '-bad-pack' must use lowercase letters, numbers, '.', '-' or '_'"));
    assert!(message.contains("version 'not-semver' is not valid semver"));
    assert!(message.contains("must be prefixed with '-bad-pack/'"));
}

#[test]
fn validate_pack_manifest_rejects_bad_subject_kind_and_duplicate_runtime() {
    let mut manifest: PackManifest = toml::from_str(valid_manifest_toml()).expect("deserialize manifest");
    manifest.subjects.as_mut().expect("subjects").kinds.push("Bad Subject".to_string());
    manifest.runtime.requirements.push(manifest.runtime.requirements[0].clone());

    let error = validate_pack_manifest(&manifest).expect_err("invalid manifest should fail");
    let message = error.to_string();
    assert!(message.contains("contains invalid subject kind 'Bad Subject'"));
    assert!(message.contains("duplicate 'python' runtime declaration"));
}

#[test]
fn validate_pack_manifest_rejects_runtime_binary_paths() {
    let mut manifest: PackManifest = toml::from_str(valid_manifest_toml()).expect("deserialize manifest");
    manifest.runtime.requirements[0].binary = Some("./bin/python3".to_string());

    let error = validate_pack_manifest(&manifest).expect_err("path-like runtime binary should fail");
    assert!(error.to_string().contains("must be a simple executable name, not a path"));
}

#[test]
fn validate_pack_manifest_assets_rejects_traversal_and_missing_files() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_valid_pack_fixture(temp.path());

    let mut manifest = parse_pack_manifest(valid_manifest_toml()).expect("parse manifest");
    manifest.runtime.workflow_overlay = Some("../escape.yaml".to_string());
    let error = validate_pack_manifest_assets(temp.path(), &manifest).expect_err("traversal should fail");
    assert!(error.to_string().contains("must stay within the pack root"));

    let mut manifest = parse_pack_manifest(valid_manifest_toml()).expect("parse manifest");
    manifest.mcp.as_mut().expect("mcp").tools = Some("mcp/missing-tools.toml".to_string());
    let error = validate_pack_manifest_assets(temp.path(), &manifest).expect_err("missing asset should fail");
    assert!(error.to_string().contains("missing path"));
}

#[test]
fn load_pack_manifest_reads_and_validates_fixture() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_valid_pack_fixture(temp.path());

    let loaded = load_pack_manifest(temp.path()).expect("pack manifest should load");
    assert_eq!(loaded.manifest.id, "ao.requirements");
    assert_eq!(loaded.pack_root, temp.path());
    assert_eq!(loaded.manifest_path, temp.path().join(PACK_MANIFEST_FILE_NAME));
}

#[test]
fn load_pack_mcp_overlay_namespaces_servers_and_phase_bindings() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_valid_pack_fixture(temp.path());

    let loaded = load_pack_manifest(temp.path()).expect("load pack");
    let overlay = load_pack_mcp_overlay(&loaded).expect("load MCP overlay");
    assert!(overlay.servers.contains_key("ao.requirements/ao"));
    assert_eq!(
        overlay.phase_mcp_bindings.get("research").expect("research binding").servers,
        vec!["ao.requirements/ao".to_string()]
    );
}

#[test]
fn apply_pack_mcp_overlay_merges_namespaced_phase_servers() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_valid_pack_fixture(temp.path());

    let loaded = load_pack_manifest(temp.path()).expect("load pack");
    let mut workflow = builtin_workflow_config();
    apply_pack_mcp_overlay(&mut workflow, &loaded).expect("apply overlay");

    assert!(workflow.mcp_servers.contains_key("ao.requirements/ao"));
    assert_eq!(
        workflow.phase_mcp_bindings.get("research").expect("research binding should be present").servers,
        vec!["ao.requirements/ao".to_string()]
    );
}

#[test]
fn load_pack_manifest_rejects_missing_workflows_root() {
    let temp = tempfile::tempdir().expect("tempdir");
    fs::create_dir_all(temp.path().join("runtime")).expect("create runtime");
    fs::create_dir_all(temp.path().join("mcp")).expect("create mcp");
    fs::create_dir_all(temp.path().join("schedules")).expect("create schedules");
    fs::write(temp.path().join(PACK_MANIFEST_FILE_NAME), valid_manifest_toml()).expect("write manifest");
    fs::write(temp.path().join("runtime/agent-runtime.overlay.yaml"), "agents: {}\n").expect("write agent overlay");
    fs::write(temp.path().join("runtime/workflow-runtime.overlay.yaml"), "workflows: []\n")
        .expect("write workflow overlay");
    fs::write(temp.path().join("mcp/servers.toml"), "[[server]]\nid = 'ao'\ncommand = 'node'\n")
        .expect("write servers");
    fs::write(temp.path().join("mcp/tools.toml"), "[phase.research]\nservers = ['ao']\n").expect("write tools");
    fs::write(temp.path().join("schedules/schedules.yaml"), "schedules: []\n").expect("write schedules");

    let error = load_pack_manifest(temp.path()).expect_err("missing workflows root should fail");
    assert!(error.to_string().contains("workflows.root points to missing path"));
}

#[cfg(unix)]
fn write_probe_script(path: &std::path::Path, output: &str) {
    use std::os::unix::fs::PermissionsExt;

    fs::write(path, format!("#!/bin/sh\necho '{}'\n", output)).expect("write probe script");
    let mut perms = fs::metadata(path).expect("metadata").permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms).expect("set permissions");
}

#[cfg(unix)]
#[test]
fn check_pack_runtime_requirements_accepts_matching_binary_override() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_valid_pack_fixture(temp.path());

    let probe = temp.path().join("python-probe.sh");
    write_probe_script(&probe, "Python 3.11.8");

    let mut loaded = load_pack_manifest(temp.path()).expect("load pack");
    loaded.manifest.runtime.requirements = vec![PackRuntimeRequirement {
        runtime: ExternalRuntimeKind::Python,
        binary: Some(probe.to_string_lossy().to_string()),
        version: Some(">=3.11.0".to_string()),
        optional: false,
        reason: None,
    }];

    let report = check_pack_runtime_requirements(&loaded).expect("runtime check");
    assert_eq!(report.checks.len(), 1);
    assert_eq!(report.checks[0].status, PackRuntimeCheckStatus::Satisfied);
    assert_eq!(report.checks[0].detected_version.as_deref(), Some("3.11.8"));
}

#[cfg(unix)]
#[test]
fn ensure_pack_runtime_requirements_rejects_missing_required_runtime() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_valid_pack_fixture(temp.path());

    let mut loaded = load_pack_manifest(temp.path()).expect("load pack");
    loaded.manifest.runtime.requirements = vec![PackRuntimeRequirement {
        runtime: ExternalRuntimeKind::Node,
        binary: Some(temp.path().join("missing-node").to_string_lossy().to_string()),
        version: Some(">=20.0.0".to_string()),
        optional: false,
        reason: None,
    }];

    let error = ensure_pack_runtime_requirements(&loaded).expect_err("missing runtime should fail");
    assert!(error.to_string().contains("requires runtime 'node'"));
}

#[cfg(unix)]
#[test]
fn activate_pack_mcp_overlay_validates_runtimes_before_merging() {
    let _lock = env_lock().lock().expect("env lock should not be poisoned");
    let temp = tempfile::tempdir().expect("tempdir");
    write_valid_pack_fixture(temp.path());
    let _secret_guard = EnvVarGuard::set("OPENAI_API_KEY", "fixture-secret");

    let probe = temp.path().join("python-probe.sh");
    write_probe_script(&probe, "Python 3.11.8");

    let mut loaded = load_pack_manifest(temp.path()).expect("load pack");
    loaded.manifest.runtime.requirements = vec![PackRuntimeRequirement {
        runtime: ExternalRuntimeKind::Python,
        binary: Some(probe.to_string_lossy().to_string()),
        version: Some(">=3.11.0".to_string()),
        optional: false,
        reason: None,
    }];

    let mut workflow = builtin_workflow_config();
    let report = activate_pack_mcp_overlay(&mut workflow, &loaded).expect("activate overlay");
    assert_eq!(report.checks[0].status, PackRuntimeCheckStatus::Satisfied);
    assert!(workflow.mcp_servers.contains_key("ao.requirements/ao"));
}

#[cfg(unix)]
#[test]
fn activate_pack_mcp_overlay_requires_declared_secrets_at_activation_time() {
    let _lock = env_lock().lock().expect("env lock should not be poisoned");
    let temp = tempfile::tempdir().expect("tempdir");
    write_valid_pack_fixture(temp.path());
    let _secret_guard = EnvVarGuard::unset("OPENAI_API_KEY");

    let loaded = load_pack_manifest(temp.path()).expect("load pack");
    let mut workflow = builtin_workflow_config();
    let error = activate_pack_mcp_overlay(&mut workflow, &loaded).expect_err("missing required secret should fail");
    assert!(error.to_string().contains("requires secret 'OPENAI_API_KEY'"));
}
