#[path = "support/test_harness.rs"]
pub mod test_harness;

use anyhow::Result;
use serde_json::Value;
use std::fs;
use std::path::Path;
use std::process::Command;
use test_harness::CliHarness;

const TEMPLATE_REGISTRY_URL_ENV: &str = "ANIMUS_TEMPLATE_REGISTRY_URL";

#[test]
fn init_non_interactive_requires_template_or_path() -> Result<()> {
    let harness = CliHarness::new()?;

    let (payload, status) = harness.run_json_err_with_exit(&["init", "--non-interactive", "--plan"])?;
    assert_eq!(status, 2);
    assert_eq!(payload.pointer("/error/code").and_then(Value::as_str), Some("invalid_input"));
    assert!(payload
        .pointer("/error/message")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .contains("non-interactive init requires --template or --path"));

    Ok(())
}

#[test]
fn init_plan_reports_selected_template_and_required_changes() -> Result<()> {
    let harness = CliHarness::new()?;
    let registry = create_template_registry_repo()?;
    let registry_url = registry.path().to_string_lossy().into_owned();

    let payload = harness.run_json_ok_with_env(
        &["init", "--template", "task-queue", "--non-interactive", "--plan"],
        &[(TEMPLATE_REGISTRY_URL_ENV, registry_url.as_str())],
    )?;
    assert_eq!(payload.pointer("/data/stage").and_then(Value::as_str), Some("plan"));
    assert_eq!(payload.pointer("/data/mode").and_then(Value::as_str), Some("non_interactive"));
    assert_eq!(payload.pointer("/data/template/id").and_then(Value::as_str), Some("task-queue"));
    assert_eq!(payload.pointer("/data/template/source_kind").and_then(Value::as_str), Some("registry"));
    assert_eq!(payload.pointer("/data/apply/applied").and_then(Value::as_bool), Some(false));
    assert!(payload.pointer("/data/required_changes/template_files").and_then(Value::as_array).is_some_and(|files| {
        files.iter().any(|file| {
            matches!(
                (file.get("path").and_then(Value::as_str), file.get("action").and_then(Value::as_str)),
                (Some(".animus/workflows/standard-workflow.yaml"), Some("create"))
            )
        })
    }));
    // The daemon git/merge policy knobs were removed in v0.5.x; templates
    // that still declare them load fine, but init no longer plans daemon
    // field changes.
    assert_eq!(
        payload.pointer("/data/required_changes/daemon_config").and_then(Value::as_array).map(Vec::len),
        Some(0)
    );

    Ok(())
}

#[test]
fn init_apply_writes_template_files_and_daemon_defaults() -> Result<()> {
    let harness = CliHarness::new()?;
    let registry = create_template_registry_repo()?;
    let registry_url = registry.path().to_string_lossy().into_owned();

    let payload = harness.run_json_ok_with_env(
        &["init", "--template", "conductor", "--non-interactive"],
        &[(TEMPLATE_REGISTRY_URL_ENV, registry_url.as_str())],
    )?;
    assert_eq!(payload.pointer("/data/stage").and_then(Value::as_str), Some("apply"));
    assert_eq!(payload.pointer("/data/template/id").and_then(Value::as_str), Some("conductor"));
    assert_eq!(payload.pointer("/data/apply/applied").and_then(Value::as_bool), Some(true));
    assert!(payload
        .pointer("/data/apply/changed_domains")
        .and_then(Value::as_array)
        .is_some_and(|domains| domains.iter().any(|value| value.as_str() == Some("template_files"))));
    assert!(payload.pointer("/data/apply/written_files").and_then(Value::as_array).is_some_and(|files| files
        .iter()
        .any(|value| value.as_str() == Some(".animus/workflows/conductor-workflow.yaml"))));

    let conductor_path = harness.project_root().join(".animus/workflows/conductor-workflow.yaml");
    assert!(conductor_path.exists(), "conductor template should write its workflow wrapper");
    let conductor_contents = fs::read_to_string(&conductor_path)?;
    assert!(conductor_contents.contains("conductor-workflow"));

    let pm_config_path = harness.scoped_root().join("daemon").join("pm-config.json");
    let pm_config: Value = serde_json::from_str(&fs::read_to_string(pm_config_path)?)?;
    // Removed v0.5.x daemon git/merge policy keys are no longer written.
    assert!(pm_config.get("auto_merge_enabled").is_none());
    assert!(pm_config.get("auto_pr_enabled").is_none());
    assert!(pm_config.get("auto_commit_before_merge").is_none());

    let compile = harness.run_json_ok_with_env(
        &["workflow", "config", "compile"],
        &[(TEMPLATE_REGISTRY_URL_ENV, registry_url.as_str())],
    )?;
    assert!(compile.get("ok").and_then(Value::as_bool) == Some(true));

    Ok(())
}

#[test]
fn init_rejects_conflicting_project_files_without_force() -> Result<()> {
    let harness = CliHarness::new()?;
    let registry = create_template_registry_repo()?;
    let registry_url = registry.path().to_string_lossy().into_owned();
    let custom_workflow_path = harness.project_root().join(".animus/workflows/custom.yaml");
    fs::create_dir_all(custom_workflow_path.parent().expect("workflow path should have a parent"))?;
    fs::write(&custom_workflow_path, "user-owned workflow")?;

    let (payload, status) = harness.run_json_err_with_exit_and_env(
        &["init", "--template", "task-queue", "--non-interactive"],
        &[(TEMPLATE_REGISTRY_URL_ENV, registry_url.as_str())],
    )?;
    assert_eq!(status, 4);
    assert_eq!(payload.pointer("/error/code").and_then(Value::as_str), Some("conflict"));
    assert!(payload
        .pointer("/error/message")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .contains(".animus/workflows/custom.yaml"));
    assert_eq!(fs::read_to_string(custom_workflow_path)?, "user-owned workflow");

    Ok(())
}

fn create_template_registry_repo() -> Result<tempfile::TempDir> {
    let registry = tempfile::tempdir()?;
    write_registry_template(
        registry.path(),
        "task-queue",
        "Task Queue Pattern",
        "task-queue",
        (true, true, true),
        &[],
        &[
            (
                ".animus/workflows/custom.yaml",
                "default_workflow_ref: standard-workflow\n\ntools_allowlist:\n  - cargo\n  - animus\nagents:\n  default:\n    description: Default\n    system_prompt: Default agent\nphases:\n  implementation:\n    mode: agent\n    agent_id: default\n",
            ),
            (
                ".animus/workflows/standard-workflow.yaml",
                "workflows:\n  - id: standard-workflow\n    name: Task Queue Delivery Workflow\n    phases:\n      - implementation\n",
            ),
            (".animus/workflows/hotfix-workflow.yaml", "default_workflow_ref: standard-workflow\n"),
            (".animus/workflows/research-workflow.yaml", "default_workflow_ref: standard-workflow\n"),
        ],
    )?;
    write_registry_template(
        registry.path(),
        "conductor",
        "Conductor Pattern",
        "conductor",
        (false, true, false),
        &[],
        &[
            (
                ".animus/workflows/custom.yaml",
                "default_workflow_ref: conductor-workflow\n\ntools_allowlist:\n  - cargo\n  - animus\nagents:\n  default:\n    description: Default\n    system_prompt: Default agent\nphases:\n  implementation:\n    mode: agent\n    agent_id: default\n",
            ),
            (
                ".animus/workflows/conductor-workflow.yaml",
                "workflows:\n  - id: conductor-workflow\n    name: Conductor Planning Workflow\n    phases:\n      - implementation\n",
            ),
            (
                ".animus/workflows/standard-workflow.yaml",
                "workflows:\n  - id: standard-workflow\n    name: Task Queue Delivery Workflow\n    phases:\n      - implementation\n",
            ),
            (".animus/workflows/hotfix-workflow.yaml", "default_workflow_ref: standard-workflow\n"),
            (".animus/workflows/research-workflow.yaml", "default_workflow_ref: standard-workflow\n"),
        ],
    )?;
    write_registry_template(
        registry.path(),
        "direct-workflow",
        "Direct Workflow Pattern",
        "direct-workflow",
        (false, false, false),
        &[],
        &[
            (
                ".animus/workflows/custom.yaml",
                "default_workflow_ref: standard-workflow\n\ntools_allowlist:\n  - cargo\n  - animus\nagents:\n  default:\n    description: Default\n    system_prompt: Default agent\nphases:\n  implementation:\n    mode: agent\n    agent_id: default\n",
            ),
            (
                ".animus/workflows/standard-workflow.yaml",
                "workflows:\n  - id: standard-workflow\n    name: Direct Workflow Delivery\n    phases:\n      - implementation\n",
            ),
            (".animus/workflows/hotfix-workflow.yaml", "default_workflow_ref: standard-workflow\n"),
            (".animus/workflows/research-workflow.yaml", "default_workflow_ref: standard-workflow\n"),
        ],
    )?;
    git(["init", "-b", "main"], registry.path())?;
    git(["config", "user.name", "Animus Tests"], registry.path())?;
    git(["config", "user.email", "animus-tests@example.com"], registry.path())?;
    git(["add", "."], registry.path())?;
    git(["commit", "-m", "fixtures"], registry.path())?;
    Ok(registry)
}

fn write_registry_template(
    registry_root: &Path,
    id: &str,
    title: &str,
    pattern: &str,
    daemon: (bool, bool, bool),
    packs: &[&str],
    files: &[(&str, &str)],
) -> Result<()> {
    let template_root = registry_root.join("templates").join(id);
    let skeleton_root = template_root.join("skeleton");
    fs::create_dir_all(&skeleton_root)?;
    let packs_toml = packs
        .iter()
        .map(|pack_id| format!("[[packs]]\nid = \"{pack_id}\"\nactivate = true\n"))
        .collect::<Vec<_>>()
        .join("\n");
    fs::write(
        template_root.join("template.toml"),
        format!(
            r#"schema = "animus.template.v1"
id = "{id}"
version = "0.1.0"
title = "{title}"
description = "{title}"
pattern = "{pattern}"
next_steps = ["animus workflow list"]

[source]
mode = "copy"
root = "skeleton"

[daemon]
auto_merge = {}
auto_pr = {}
auto_commit_before_merge = {}

{}
"#,
            daemon.0, daemon.1, daemon.2, packs_toml
        ),
    )?;
    for (relative_path, contents) in files {
        let path = skeleton_root.join(relative_path);
        fs::create_dir_all(path.parent().expect("template file should have a parent"))?;
        fs::write(path, contents)?;
    }
    Ok(())
}

fn git<const N: usize>(args: [&str; N], cwd: &Path) -> Result<()> {
    let status = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()?;
    anyhow::ensure!(status.success(), "git command failed in {}", cwd.display());
    Ok(())
}

#[test]
fn init_first_clone_writes_pinned_commit_file() -> Result<()> {
    let harness = CliHarness::new()?;
    let registry = create_template_registry_repo()?;
    let registry_url = registry.path().to_string_lossy().into_owned();

    harness.run_json_ok_with_env(
        &["init", "--template", "task-queue", "--non-interactive", "--plan"],
        &[(TEMPLATE_REGISTRY_URL_ENV, registry_url.as_str())],
    )?;

    let commit_path = registry_cache_commit_path(&harness);
    assert!(commit_path.exists(), "expected pinned commit file at {}", commit_path.display());
    let pinned = fs::read_to_string(&commit_path)?.trim().to_string();
    let upstream_head = git_rev_parse_head(registry.path())?;
    assert_eq!(pinned, upstream_head, "pinned commit should match upstream HEAD after first clone");
    assert_eq!(pinned.len(), 40, "expected a 40-char sha but got {pinned:?}");

    Ok(())
}

#[test]
fn init_subsequent_call_uses_pinned_commit_not_latest_upstream() -> Result<()> {
    let harness = CliHarness::new()?;
    let registry = create_template_registry_repo()?;
    let registry_url = registry.path().to_string_lossy().into_owned();

    let first = harness.run_json_ok_with_env(
        &["init", "--template", "conductor", "--non-interactive"],
        &[(TEMPLATE_REGISTRY_URL_ENV, registry_url.as_str())],
    )?;
    assert_eq!(first.pointer("/data/template/id").and_then(Value::as_str), Some("conductor"));
    let conductor_path = harness.project_root().join(".animus/workflows/conductor-workflow.yaml");
    let conductor_contents_before = fs::read_to_string(&conductor_path)?;
    assert!(conductor_contents_before.contains("Conductor Planning Workflow"));

    bump_upstream_template(
        registry.path(),
        "conductor",
        ".animus/workflows/conductor-workflow.yaml",
        "workflows:\n  - id: conductor-workflow\n    name: Tampered Conductor Workflow\n    phases:\n      - workflow_ref: animus.requirement/plan\n",
    )?;

    let harness2 = CliHarness::with_existing_home(&harness)?;
    let payload = harness2.run_json_ok_with_env(
        &["init", "--template", "conductor", "--non-interactive"],
        &[(TEMPLATE_REGISTRY_URL_ENV, registry_url.as_str())],
    )?;
    assert_eq!(payload.pointer("/data/template/id").and_then(Value::as_str), Some("conductor"));

    let conductor_after =
        fs::read_to_string(harness2.project_root().join(".animus/workflows/conductor-workflow.yaml"))?;
    assert!(
        conductor_after.contains("Conductor Planning Workflow"),
        "pinned registry should still produce original template content, got: {conductor_after}"
    );
    assert!(!conductor_after.contains("Tampered"), "tampered upstream content must not leak through pinned cache");

    let pinned_sha = fs::read_to_string(registry_cache_commit_path(&harness))?.trim().to_string();
    let upstream_head = git_rev_parse_head(registry.path())?;
    assert_ne!(pinned_sha, upstream_head, "upstream HEAD should have advanced past the pinned commit");

    Ok(())
}

#[test]
fn init_with_update_registry_fetches_latest_and_repins() -> Result<()> {
    let harness = CliHarness::new()?;
    let registry = create_template_registry_repo()?;
    let registry_url = registry.path().to_string_lossy().into_owned();

    harness.run_json_ok_with_env(
        &["init", "--template", "conductor", "--non-interactive"],
        &[(TEMPLATE_REGISTRY_URL_ENV, registry_url.as_str())],
    )?;
    let pinned_before = fs::read_to_string(registry_cache_commit_path(&harness))?.trim().to_string();

    bump_upstream_template(
        registry.path(),
        "conductor",
        ".animus/workflows/conductor-workflow.yaml",
        "workflows:\n  - id: conductor-workflow\n    name: Updated Conductor Workflow\n    phases:\n      - workflow_ref: animus.requirement/plan\n",
    )?;
    let upstream_head_after_bump = git_rev_parse_head(registry.path())?;
    assert_ne!(upstream_head_after_bump, pinned_before, "upstream HEAD should differ after bump");

    let harness2 = CliHarness::with_existing_home(&harness)?;
    let payload = harness2.run_json_ok_with_env(
        &["init", "--template", "conductor", "--non-interactive", "--update-registry"],
        &[(TEMPLATE_REGISTRY_URL_ENV, registry_url.as_str())],
    )?;
    assert_eq!(payload.pointer("/data/template/id").and_then(Value::as_str), Some("conductor"));

    let conductor_after =
        fs::read_to_string(harness2.project_root().join(".animus/workflows/conductor-workflow.yaml"))?;
    assert!(
        conductor_after.contains("Updated Conductor Workflow"),
        "after --update-registry the new upstream content should be applied, got: {conductor_after}"
    );

    let pinned_after = fs::read_to_string(registry_cache_commit_path(&harness))?.trim().to_string();
    assert_eq!(pinned_after, upstream_head_after_bump, "--update-registry must re-pin to the new upstream HEAD");
    assert_ne!(pinned_after, pinned_before, "pinned sha must change after --update-registry");

    Ok(())
}

#[test]
fn init_reports_error_when_pinned_commit_diverges() -> Result<()> {
    let harness = CliHarness::new()?;
    let registry = create_template_registry_repo()?;
    let registry_url = registry.path().to_string_lossy().into_owned();

    harness.run_json_ok_with_env(
        &["init", "--template", "conductor", "--non-interactive"],
        &[(TEMPLATE_REGISTRY_URL_ENV, registry_url.as_str())],
    )?;

    let commit_path = registry_cache_commit_path(&harness);
    fs::write(&commit_path, "0000000000000000000000000000000000000000\n")?;

    let harness2 = CliHarness::with_existing_home(&harness)?;
    let (payload, _status) = harness2.run_json_err_with_exit_and_env(
        &["init", "--template", "conductor", "--non-interactive", "--plan"],
        &[(TEMPLATE_REGISTRY_URL_ENV, registry_url.as_str())],
    )?;
    let message = payload.pointer("/error/message").and_then(Value::as_str).unwrap_or_default().to_string();
    assert!(
        message.contains("diverged from the pinned commit"),
        "divergence error message should call out the pinned commit mismatch, got: {message}"
    );
    assert!(
        message.contains("0000000000000000000000000000000000000000"),
        "divergence error should include the bogus pinned sha, got: {message}"
    );
    assert!(message.contains("--update-registry"), "divergence error should advise --update-registry, got: {message}");

    Ok(())
}

fn registry_cache_commit_path(harness: &CliHarness) -> std::path::PathBuf {
    harness.config_root().join(".animus").join("template-registries").join("launchapp").join(".commit")
}

fn git_rev_parse_head(repo: &Path) -> Result<String> {
    let output = Command::new("git").args(["rev-parse", "HEAD"]).current_dir(repo).output()?;
    anyhow::ensure!(output.status.success(), "git rev-parse HEAD failed in {}", repo.display());
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn bump_upstream_template(registry_root: &Path, template_id: &str, relative_path: &str, contents: &str) -> Result<()> {
    let target = registry_root.join("templates").join(template_id).join("skeleton").join(relative_path);
    fs::create_dir_all(target.parent().expect("template path should have a parent"))?;
    fs::write(&target, contents)?;
    git(["add", "."], registry_root)?;
    git(["commit", "-m", "bump"], registry_root)?;
    Ok(())
}

#[test]
fn init_supports_local_template_directories() -> Result<()> {
    let harness = CliHarness::new()?;
    let template_root = tempfile::tempdir()?;
    let source_root = template_root.path().join("skeleton/.animus/workflows");
    fs::create_dir_all(&source_root)?;
    fs::write(
        template_root.path().join("template.toml"),
        r#"schema = "animus.template.v1"
id = "local-copy"
version = "0.1.0"
title = "Local Copy Template"
description = "Local template fixture for init e2e coverage."
pattern = "local-copy"
next_steps = ["animus workflow list"]

[source]
mode = "copy"
root = "skeleton"

[daemon]
auto_merge = true
auto_pr = false
auto_commit_before_merge = true
"#,
    )?;
    fs::write(
        source_root.join("local-template.yaml"),
        "workflows:\n  - id: local-template\n    name: Local Template\n    phases:\n      - workflow_ref: animus.task/standard\n",
    )?;

    let template_path = template_root.path().to_string_lossy().into_owned();
    let payload = harness.run_json_ok(&["init", "--path", &template_path, "--non-interactive"])?;
    assert_eq!(payload.pointer("/data/template/id").and_then(Value::as_str), Some("local-copy"));
    assert_eq!(payload.pointer("/data/template/source_kind").and_then(Value::as_str), Some("local"));

    let local_workflow_path = harness.project_root().join(".animus/workflows/local-template.yaml");
    assert!(local_workflow_path.exists(), "local template file should be copied into the project");
    assert!(fs::read_to_string(&local_workflow_path)?.contains("local-template"));

    let pm_config_path = harness.scoped_root().join("daemon").join("pm-config.json");
    let pm_config: Value = serde_json::from_str(&fs::read_to_string(pm_config_path)?)?;
    // The template's legacy [daemon] auto_* keys are tolerated but ignored.
    assert!(pm_config.get("auto_merge_enabled").is_none());
    assert!(pm_config.get("auto_pr_enabled").is_none());
    assert!(pm_config.get("auto_commit_before_merge").is_none());

    Ok(())
}

// ---------------------------------------------------------------------------
// Recommended pack install (--install-packs) + secrets migration surfacing
// ---------------------------------------------------------------------------

const PACK_SOURCE_DIR_ENV: &str = "ANIMUS_INIT_PACK_SOURCE_DIR";
const DEFAULT_INSTALL_MANIFEST_JSON: &str = include_str!("../config/default-install.json");
const RECOMMENDED_PACK_IDS: &[&str] = &["animus.core-skills", "animus.task", "animus.requirement", "animus.review"];

/// `(id, pinned version)` pairs read from the bundled `default-install.json`
/// so the fixture versions stay in lockstep with the real pins.
fn recommended_pack_pins() -> Result<Vec<(String, String)>> {
    let manifest: Value = serde_json::from_str(DEFAULT_INSTALL_MANIFEST_JSON)?;
    let packs = manifest
        .get("packs")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("default-install.json should declare packs"))?;
    packs
        .iter()
        .map(|pack| {
            let id = pack.get("id").and_then(Value::as_str).map(str::to_string);
            let version = pack.get("tag").and_then(Value::as_str).map(|tag| tag.trim_start_matches('v').to_string());
            match (id, version) {
                (Some(id), Some(version)) => Ok((id, version)),
                other => Err(anyhow::anyhow!("pack entry missing id or tag: {other:?}")),
            }
        })
        .collect()
}

/// Build a local source directory that mirrors the recommended pack layout so
/// `--install-packs` can run offline (no git clone, no network).
fn create_recommended_pack_fixture_dir() -> Result<tempfile::TempDir> {
    let source = tempfile::tempdir()?;
    for (pack_id, version) in recommended_pack_pins()? {
        let pack_root = source.path().join(&pack_id);
        fs::create_dir_all(pack_root.join("workflows"))?;
        fs::write(
            pack_root.join("workflows").join("standard.yaml"),
            "workflows:\n  - id: standard\n    name: Fixture Workflow\n    phases:\n      - implementation\n",
        )?;
        fs::write(
            pack_root.join("pack.toml"),
            format!(
                r#"schema = "animus.pack.v1"
id = "{pack_id}"
version = "{version}"
kind = "domain-pack"
title = "{pack_id} fixture"
description = "init e2e fixture pack"

[ownership]
mode = "bundled"

[workflows]
root = "workflows"
exports = ["{pack_id}/standard"]
"#
            ),
        )?;
    }
    Ok(source)
}

#[test]
fn init_walkthrough_install_packs_installs_recommended_packs() -> Result<()> {
    let harness = CliHarness::new()?;
    let source = create_recommended_pack_fixture_dir()?;
    let source_dir = source.path().to_string_lossy().into_owned();

    let payload = harness.run_json_ok_with_env(
        &["init", "--walkthrough", "--non-interactive", "--no-install", "--no-template", "--install-packs"],
        &[(PACK_SOURCE_DIR_ENV, source_dir.as_str())],
    )?;

    assert_eq!(payload.pointer("/data/apply/packs/skipped").and_then(Value::as_bool), Some(false));
    let results = payload
        .pointer("/data/apply/packs/results")
        .and_then(Value::as_array)
        .expect("walkthrough envelope should include apply.packs.results");
    assert_eq!(results.len(), RECOMMENDED_PACK_IDS.len());
    for result in results {
        assert_eq!(
            result.get("status").and_then(Value::as_str),
            Some("installed"),
            "every recommended pack should install from the fixture source, got: {result}"
        );
    }

    for (pack_id, version) in recommended_pack_pins()? {
        let installed_root = harness.config_root().join(".animus").join("packs").join(&pack_id).join(&version);
        assert!(installed_root.is_dir(), "pack {pack_id} should be installed at {}", installed_root.display());
    }

    let next_steps = payload.pointer("/data/next_steps").and_then(Value::as_array).expect("next_steps");
    assert!(
        next_steps
            .iter()
            .filter_map(Value::as_str)
            .any(|step| step.contains("animus workflow run animus.task/standard")),
        "next steps should point at the first runnable workflow, got: {next_steps:?}"
    );

    // The packs must also be activated for this project.
    let pack_list = harness.run_json_ok(&["pack", "list"])?;
    let rows = pack_list.pointer("/data").and_then(Value::as_array).expect("pack list rows");
    for pack_id in RECOMMENDED_PACK_IDS {
        assert!(
            rows.iter().any(|row| row.get("pack_id").and_then(Value::as_str) == Some(*pack_id)
                && row.get("active").and_then(Value::as_bool) == Some(true)),
            "pack {pack_id} should be listed active after init, got: {rows:?}"
        );
    }

    // Disable one pack to verify the re-run re-activates packs that are
    // already installed but turned off for the project.
    harness.run_json_ok(&["pack", "pin", "--pack-id", "animus.task", "--disable"])?;

    // Re-running init with the same pins must be idempotent: every pack is
    // reported as already_installed and nothing is re-cloned.
    let second = harness.run_json_ok_with_env(
        &["init", "--walkthrough", "--non-interactive", "--no-install", "--no-template", "--install-packs"],
        &[(PACK_SOURCE_DIR_ENV, source_dir.as_str())],
    )?;
    let second_results = second
        .pointer("/data/apply/packs/results")
        .and_then(Value::as_array)
        .expect("second walkthrough run should include apply.packs.results");
    for result in second_results {
        assert_eq!(
            result.get("status").and_then(Value::as_str),
            Some("already_installed"),
            "pinned packs already present should be skipped, got: {result}"
        );
    }

    // The disabled pack must be active again after the re-run.
    let pack_list_after = harness.run_json_ok(&["pack", "list"])?;
    let rows_after = pack_list_after.pointer("/data").and_then(Value::as_array).expect("pack list rows");
    assert!(
        rows_after.iter().any(|row| row.get("pack_id").and_then(Value::as_str) == Some("animus.task")
            && row.get("active").and_then(Value::as_bool) == Some(true)),
        "already-installed pack should be re-activated by init, got: {rows_after:?}"
    );

    Ok(())
}

#[test]
fn init_walkthrough_install_packs_failure_keeps_init_successful() -> Result<()> {
    let harness = CliHarness::new()?;
    let empty_source = tempfile::tempdir()?;
    let source_dir = empty_source.path().to_string_lossy().into_owned();

    let payload = harness.run_json_ok_with_env(
        &["init", "--walkthrough", "--non-interactive", "--no-install", "--no-template", "--install-packs"],
        &[(PACK_SOURCE_DIR_ENV, source_dir.as_str())],
    )?;

    let results = payload
        .pointer("/data/apply/packs/results")
        .and_then(Value::as_array)
        .expect("walkthrough envelope should include apply.packs.results");
    assert!(!results.is_empty());
    for result in results {
        assert_eq!(result.get("status").and_then(Value::as_str), Some("failed"));
        let detail = result.get("detail").and_then(Value::as_str).unwrap_or_default();
        assert!(
            detail.contains("animus pack install --path"),
            "failed result should carry the manual install command, got: {detail}"
        );
    }

    let next_steps = payload.pointer("/data/next_steps").and_then(Value::as_array).expect("next_steps");
    assert!(
        next_steps.iter().filter_map(Value::as_str).any(|step| step.contains("Pack install failed for")),
        "next steps should surface the failed installs, got: {next_steps:?}"
    );

    Ok(())
}

#[test]
fn init_walkthrough_without_install_packs_flag_keeps_previous_behavior() -> Result<()> {
    let harness = CliHarness::new()?;

    let payload =
        harness.run_json_ok(&["init", "--walkthrough", "--non-interactive", "--no-install", "--no-template"])?;

    assert_eq!(payload.pointer("/data/apply/packs/skipped").and_then(Value::as_bool), Some(true));
    let packs_root = harness.config_root().join(".animus").join("packs");
    assert!(
        !packs_root.join("animus.task").exists(),
        "non-interactive init without --install-packs must not install packs"
    );

    let next_steps = payload.pointer("/data/next_steps").and_then(Value::as_array).expect("next_steps");
    assert!(
        next_steps.iter().filter_map(Value::as_str).any(|step| step.contains("--install-packs")),
        "next steps should mention the --install-packs opt-in, got: {next_steps:?}"
    );

    Ok(())
}

#[test]
fn init_walkthrough_non_interactive_keeps_default_flavor() -> Result<()> {
    // Non-interactive walkthrough must never prompt for a flavor and must
    // record `default` in both the plan and the apply envelope.
    let harness = CliHarness::new()?;

    let plan = harness.run_json_ok(&["init", "--walkthrough", "--non-interactive", "--plan", "--no-template"])?;
    assert_eq!(
        plan.pointer("/data/planned_actions/flavor").and_then(Value::as_str),
        Some("default"),
        "non-interactive walkthrough plan must report the default flavor, got: {plan:?}"
    );

    let apply =
        harness.run_json_ok(&["init", "--walkthrough", "--non-interactive", "--no-install", "--no-template"])?;
    assert_eq!(
        apply.pointer("/data/apply/flavor").and_then(Value::as_str),
        Some("default"),
        "non-interactive walkthrough apply must report the default flavor, got: {apply:?}"
    );

    Ok(())
}

#[test]
fn init_walkthrough_surfaces_secrets_migration_for_env_keys() -> Result<()> {
    let harness = CliHarness::new()?;

    let payload = harness.run_json_ok_with_env(
        &["init", "--walkthrough", "--non-interactive", "--no-install", "--no-template"],
        &[("ANTHROPIC_API_KEY", "test-key-do-not-store")],
    )?;

    let detected = payload
        .pointer("/data/secrets/detected_env_keys")
        .and_then(Value::as_array)
        .expect("walkthrough envelope should include secrets.detected_env_keys");
    assert!(
        detected.iter().filter_map(Value::as_str).any(|key| key == "ANTHROPIC_API_KEY"),
        "ANTHROPIC_API_KEY should be detected, got: {detected:?}"
    );
    let commands = payload
        .pointer("/data/secrets/suggested_commands")
        .and_then(Value::as_array)
        .expect("secrets.suggested_commands");
    assert!(commands.iter().filter_map(Value::as_str).any(|c| c == "animus secret set ANTHROPIC_API_KEY"));
    assert_eq!(payload.pointer("/data/secrets/docs").and_then(Value::as_str), Some("docs/reference/secrets.md"));

    // Non-interactive runs must never import into the OS keychain.
    assert_eq!(payload.pointer("/data/apply/secrets/accepted").and_then(Value::as_bool), Some(false));
    assert_eq!(payload.pointer("/data/apply/secrets/stored").and_then(Value::as_array).map(Vec::len), Some(0));

    let next_steps = payload.pointer("/data/next_steps").and_then(Value::as_array).expect("next_steps");
    assert!(
        next_steps.iter().filter_map(Value::as_str).any(|step| step.contains("animus secret set")),
        "next steps should include the keychain migration hint, got: {next_steps:?}"
    );

    Ok(())
}

#[test]
fn init_template_flow_supports_install_packs_flag() -> Result<()> {
    let harness = CliHarness::new()?;
    let registry = create_template_registry_repo()?;
    let registry_url = registry.path().to_string_lossy().into_owned();
    let source = create_recommended_pack_fixture_dir()?;
    let source_dir = source.path().to_string_lossy().into_owned();

    let payload = harness.run_json_ok_with_env(
        &["init", "--template", "task-queue", "--non-interactive", "--install-packs"],
        &[(TEMPLATE_REGISTRY_URL_ENV, registry_url.as_str()), (PACK_SOURCE_DIR_ENV, source_dir.as_str())],
    )?;

    let results = payload
        .pointer("/data/apply/recommended_packs/results")
        .and_then(Value::as_array)
        .expect("apply.recommended_packs.results");
    assert_eq!(results.len(), RECOMMENDED_PACK_IDS.len());
    for result in results {
        assert_eq!(result.get("status").and_then(Value::as_str), Some("installed"));
    }
    assert!(payload
        .pointer("/data/apply/changed_domains")
        .and_then(Value::as_array)
        .is_some_and(|domains| domains.iter().any(|value| value.as_str() == Some("pack_installation"))));

    Ok(())
}
