#[path = "support/test_harness.rs"]
pub mod test_harness;

use anyhow::{Context, Result};
use serde_json::Value;
use std::fs;
use test_harness::CliHarness;

fn read_json(path: &std::path::Path) -> Result<Value> {
    let raw = fs::read_to_string(path).with_context(|| format!("failed to read json file {}", path.display()))?;
    serde_json::from_str(&raw).with_context(|| format!("failed to parse json at {}", path.display()))
}

#[test]
fn skill_lifecycle_install_list_update_and_lock_determinism() -> Result<()> {
    let harness = CliHarness::new()?;

    harness.run_json_ok(&["skill", "publish", "--name", "lint", "--version", "1.0.0", "--source", "zeta"])?;
    harness.run_json_ok(&["skill", "publish", "--name", "lint", "--version", "1.1.0", "--source", "zeta"])?;
    harness.run_json_ok(&["skill", "publish", "--name", "lint", "--version", "1.1.0", "--source", "alpha"])?;
    harness.run_json_ok(&["skill", "publish", "--name", "lint", "--version", "2.0.0-beta.1", "--source", "alpha"])?;

    let installed = harness.run_json_ok(&["skill", "install", "--name", "lint"])?;
    assert_eq!(
        installed.pointer("/data/installed/version").and_then(Value::as_str),
        Some("1.1.0"),
        "install should prefer stable release over prerelease"
    );
    assert_eq!(
        installed.pointer("/data/installed/source").and_then(Value::as_str),
        Some("alpha"),
        "equal semver candidates should use lexical source tie-break"
    );
    assert_eq!(
        installed.pointer("/data/lock_changed").and_then(Value::as_bool),
        Some(true),
        "first install should write lock state"
    );

    let state_dir = harness.scoped_root().join("state");
    let registry_path = state_dir.join("skills-registry.v1.json");
    let lock_path = state_dir.join("skills-lock.v1.json");
    assert!(registry_path.exists(), "install should write skills-registry.v1.json");
    assert!(lock_path.exists(), "install should write skills-lock.v1.json");

    let registry_json = read_json(&registry_path)?;
    assert!(
        registry_json.pointer("/installed").and_then(Value::as_array).is_some_and(|items| !items.is_empty()),
        "registry state should include installed entries"
    );
    let lock_json = read_json(&lock_path)?;
    assert!(
        lock_json.pointer("/entries").and_then(Value::as_array).is_some_and(|items| !items.is_empty()),
        "lock state should include entries"
    );
    assert!(lock_json.pointer("/entries/0/name").and_then(Value::as_str).is_some(), "lock entry should include name");
    assert!(
        lock_json.pointer("/entries/0/version").and_then(Value::as_str).is_some(),
        "lock entry should include version"
    );
    assert!(
        lock_json.pointer("/entries/0/source").and_then(Value::as_str).is_some(),
        "lock entry should include source"
    );
    assert!(
        lock_json.pointer("/entries/0/integrity").and_then(Value::as_str).is_some(),
        "lock entry should include integrity"
    );
    assert!(
        lock_json.pointer("/entries/0/artifact").and_then(Value::as_str).is_some(),
        "lock entry should include artifact"
    );

    let lock_before = fs::read(&lock_path).context("failed to read lock bytes before no-op")?;
    let repeated_install = harness.run_json_ok(&["skill", "install", "--name", "lint"])?;
    assert_eq!(
        repeated_install.pointer("/data/lock_changed").and_then(Value::as_bool),
        Some(false),
        "repeated install with unchanged inputs should not rewrite lock"
    );
    let lock_after_repeated_install = fs::read(&lock_path).context("failed to read lock bytes after no-op install")?;
    assert_eq!(lock_before, lock_after_repeated_install, "lockfile bytes must remain stable on no-op install");

    let listed = harness.run_json_ok(&["skill", "list"])?;
    let listed_items =
        listed.pointer("/data").and_then(Value::as_array).context("skill list should return an array payload")?;
    let lint_item = listed_items
        .iter()
        .find(|item| item.get("name").and_then(Value::as_str) == Some("lint"))
        .context("installed lint skill should be present in list output")?;
    assert_eq!(lint_item.get("version").and_then(Value::as_str), Some("1.1.0"));
    assert_eq!(lint_item.get("lock_status").and_then(Value::as_str), Some("locked"));

    let updated = harness.run_json_ok(&["skill", "update"])?;
    assert_eq!(
        updated.pointer("/data/lock_changed").and_then(Value::as_bool),
        Some(false),
        "update should not rewrite lock when resolution is unchanged"
    );
    let lock_after_update = fs::read(&lock_path).context("failed to read lock bytes after update")?;
    assert_eq!(lock_before, lock_after_update, "lockfile bytes must remain stable on no-op update");

    Ok(())
}

#[test]
fn skill_search_is_deterministic_for_identical_inputs() -> Result<()> {
    let harness = CliHarness::new()?;

    harness.run_json_ok(&["skill", "publish", "--name", "build-cache", "--version", "1.3.0", "--source", "stable"])?;
    harness.run_json_ok(&["skill", "publish", "--name", "build-cache", "--version", "1.4.0", "--source", "stable"])?;
    harness.run_json_ok(&["skill", "publish", "--name", "build-cache", "--version", "1.4.0", "--source", "alpha"])?;

    let first = harness.run_json_ok(&["skill", "search", "--query", "build"])?;
    let second = harness.run_json_ok(&["skill", "search", "--query", "build"])?;
    assert_eq!(first.pointer("/data"), second.pointer("/data"), "search output ordering should be deterministic");

    let results = first.pointer("/data").and_then(Value::as_array).context("search should return array data")?;
    assert!(results.len() >= 2, "search should return published entries for matching query");

    Ok(())
}

#[test]
fn skill_error_contract_maps_invalid_not_found_and_conflict() -> Result<()> {
    let harness = CliHarness::new()?;

    harness.run_json_ok(&["skill", "publish", "--name", "fmt", "--version", "1.0.0", "--source", "local"])?;

    let (invalid_payload, invalid_status) =
        harness.run_json_err_with_exit(&["skill", "install", "--name", "fmt", "--version", "=2.0.0"])?;
    assert_eq!(invalid_status, 2, "unsatisfied version constraint should be invalid input");
    assert_eq!(invalid_payload.pointer("/error/code").and_then(Value::as_str), Some("invalid_input"));

    let (missing_payload, missing_status) =
        harness.run_json_err_with_exit(&["skill", "install", "--name", "missing-skill"])?;
    assert_eq!(missing_status, 3, "missing skill should be not found");
    assert_eq!(missing_payload.pointer("/error/code").and_then(Value::as_str), Some("not_found"));

    let (conflict_payload, conflict_status) = harness.run_json_err_with_exit(&[
        "skill",
        "publish",
        "--name",
        "fmt",
        "--version",
        "1.0.0",
        "--source",
        "local",
    ])?;
    assert_eq!(conflict_status, 4, "duplicate publish should be conflict");
    assert_eq!(conflict_payload.pointer("/error/code").and_then(Value::as_str), Some("conflict"));

    Ok(())
}

#[test]
fn skill_update_cli_constraints_override_lock_pins() -> Result<()> {
    let harness = CliHarness::new()?;

    harness.run_json_ok(&["skill", "publish", "--name", "fmt", "--version", "1.0.0", "--source", "local"])?;
    harness.run_json_ok(&["skill", "publish", "--name", "fmt", "--version", "1.1.0", "--source", "local"])?;

    harness.run_json_ok(&["skill", "install", "--name", "fmt", "--version", "=1.0.0"])?;

    let lock_path = harness.scoped_root().join("state/skills-lock.v1.json");
    let lock_before = fs::read(&lock_path).context("failed to read lock bytes before updates")?;

    let no_override = harness.run_json_ok(&["skill", "update", "--name", "fmt"])?;
    assert_eq!(
        no_override.pointer("/data/updated/0/version").and_then(Value::as_str),
        Some("1.0.0"),
        "lock pin should keep the previously resolved version when update has no override"
    );
    assert_eq!(
        no_override.pointer("/data/lock_changed").and_then(Value::as_bool),
        Some(false),
        "no-op update should not rewrite lock state"
    );

    let overridden = harness.run_json_ok(&["skill", "update", "--name", "fmt", "--version", "=1.1.0"])?;
    assert_eq!(
        overridden.pointer("/data/updated/0/version").and_then(Value::as_str),
        Some("1.1.0"),
        "cli version override must take precedence over lock pin"
    );
    assert_eq!(
        overridden.pointer("/data/lock_changed").and_then(Value::as_bool),
        Some(true),
        "override update should rewrite lock state"
    );

    let lock_after = fs::read(&lock_path).context("failed to read lock bytes after override")?;
    assert_ne!(lock_before, lock_after, "lockfile bytes should change when resolved version changes");

    let listed = harness.run_json_ok(&["skill", "list"])?;
    let listed_items = listed.pointer("/data").and_then(Value::as_array).expect("skill list should return an array");
    let fmt_item = listed_items
        .iter()
        .find(|item| item.get("name").and_then(Value::as_str) == Some("fmt"))
        .expect("installed fmt skill should be present in list output");
    assert_eq!(
        fmt_item.get("version").and_then(Value::as_str),
        Some("1.1.0"),
        "list should reflect the updated installed version"
    );

    Ok(())
}

#[test]
fn skill_lockfile_entries_are_sorted_by_name_then_source() -> Result<()> {
    let harness = CliHarness::new()?;

    harness.run_json_ok(&["skill", "publish", "--name", "zskill", "--version", "1.0.0", "--source", "zeta"])?;
    harness.run_json_ok(&["skill", "publish", "--name", "askill", "--version", "1.0.0", "--source", "zeta"])?;
    harness.run_json_ok(&["skill", "publish", "--name", "askill", "--version", "1.0.0", "--source", "alpha"])?;

    harness.run_json_ok(&["skill", "install", "--name", "zskill", "--source", "zeta"])?;
    harness.run_json_ok(&["skill", "install", "--name", "askill", "--source", "zeta"])?;
    harness.run_json_ok(&["skill", "install", "--name", "askill", "--source", "alpha"])?;

    let lock_path = harness.scoped_root().join("state/skills-lock.v1.json");
    let lock_json = read_json(&lock_path)?;
    let entries =
        lock_json.pointer("/entries").and_then(Value::as_array).context("lockfile entries should be an array")?;

    let entry_keys: Vec<(String, String)> = entries
        .iter()
        .map(|entry| {
            let name = entry.get("name").and_then(Value::as_str).context("lock entry should include name")?;
            let source = entry.get("source").and_then(Value::as_str).context("lock entry should include source")?;
            Ok((name.to_string(), source.to_string()))
        })
        .collect::<Result<_>>()?;

    assert_eq!(
        entry_keys,
        vec![
            ("askill".to_string(), "alpha".to_string()),
            ("askill".to_string(), "zeta".to_string()),
            ("zskill".to_string(), "zeta".to_string()),
        ],
        "lock entries must have stable ordering by name then source"
    );

    Ok(())
}

#[test]
fn skill_error_contract_maps_registry_unavailable_to_exit_code_5() -> Result<()> {
    let harness = CliHarness::new()?;

    harness.run_json_ok(&["skill", "publish", "--name", "offline-skill", "--version", "1.0.0", "--source", "local"])?;

    let registry_path = harness.scoped_root().join("state/skills-registry.v1.json");
    let mut registry_json = read_json(&registry_path)?;
    let registries = registry_json
        .get_mut("registries")
        .and_then(Value::as_array_mut)
        .context("registry state should include registries array")?;
    let project_registry = registries
        .iter_mut()
        .find(|entry| entry.get("id").and_then(Value::as_str) == Some("project"))
        .context("project registry entry should exist")?;
    project_registry["available"] = Value::Bool(false);
    let rewritten =
        serde_json::to_string_pretty(&registry_json).context("failed to serialize unavailable registry fixture")?;
    fs::write(&registry_path, rewritten)
        .with_context(|| format!("failed to write unavailable registry fixture to {}", registry_path.display()))?;

    let unavailable_payload = harness.run_json_err(&["skill", "search", "--registry", "project"])?;
    assert_eq!(unavailable_payload.pointer("/error/code").and_then(Value::as_str), Some("unavailable"));

    let (unavailable_payload, unavailable_status) =
        harness.run_json_err_with_exit(&["skill", "search", "--registry", "project"])?;
    assert_eq!(unavailable_status, 5, "unavailable registry should map to unavailable exit code");
    assert_eq!(unavailable_payload.pointer("/error/code").and_then(Value::as_str), Some("unavailable"));

    Ok(())
}

#[test]
fn skill_registry_add_remove_and_list_follow_contract() -> Result<()> {
    let harness = CliHarness::new()?;

    let added =
        harness.run_json_ok(&["skill", "registry", "add", "--id", "mirror", "--url", "https://mirror.example.com"])?;
    assert_eq!(added.pointer("/data/registry/id").and_then(Value::as_str), Some("mirror"));
    assert_eq!(added.pointer("/data/registry/url").and_then(Value::as_str), Some("https://mirror.example.com"));
    assert_eq!(added.pointer("/data/registry/available").and_then(Value::as_bool), Some(true));
    assert_eq!(added.pointer("/data/registry_changed").and_then(Value::as_bool), Some(true));

    let listed = harness.run_json_ok(&["skill", "registry", "list"])?;
    let registries =
        listed.pointer("/data").and_then(Value::as_array).context("registry list should return an array payload")?;
    assert!(
        registries.iter().any(|entry| entry.get("id").and_then(Value::as_str) == Some("mirror")),
        "registry list should include the added mirror entry"
    );

    let removed = harness.run_json_ok(&["skill", "registry", "remove", "--id", "mirror"])?;
    assert_eq!(removed.pointer("/data/removed_id").and_then(Value::as_str), Some("mirror"));
    assert_eq!(removed.pointer("/data/registry_changed").and_then(Value::as_bool), Some(true));

    let (missing_url_payload, missing_url_status) =
        harness.run_json_err_with_exit(&["skill", "registry", "add", "--id", "blank-url", "--url", "   "])?;
    assert_eq!(missing_url_status, 2, "blank registry url should be invalid input");
    assert_eq!(missing_url_payload.pointer("/error/message").and_then(Value::as_str), Some("invalid url"));

    let (blank_id_payload, blank_id_status) =
        harness.run_json_err_with_exit(&["skill", "registry", "remove", "--id", "   "])?;
    assert_eq!(blank_id_status, 2, "blank registry id should be invalid input");
    assert_eq!(blank_id_payload.pointer("/error/message").and_then(Value::as_str), Some("invalid id"));

    let (missing_id_payload, missing_id_status) =
        harness.run_json_err_with_exit(&["skill", "registry", "remove", "--id", "missing"])?;
    assert_eq!(missing_id_status, 3, "unknown registry id should be not found");
    assert_eq!(
        missing_id_payload.pointer("/error/message").and_then(Value::as_str),
        Some("registry not found: missing")
    );

    Ok(())
}
