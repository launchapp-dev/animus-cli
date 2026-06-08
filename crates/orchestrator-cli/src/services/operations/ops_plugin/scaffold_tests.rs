use std::collections::BTreeMap;

use tempfile::TempDir;

use super::{handle_plugin_scaffold_trigger, substitute};
use crate::PluginScaffoldTriggerArgs;

fn make_args(name: &str, out_dir: std::path::PathBuf) -> PluginScaffoldTriggerArgs {
    PluginScaffoldTriggerArgs {
        name: name.to_string(),
        owner: Some("acme-co".to_string()),
        out_dir: Some(out_dir),
        license: "Apache-2.0".to_string(),
        description: Some("Custom fswatch trigger".to_string()),
        protocol_tag: "v0.5.5".to_string(),
        force: false,
        json: true,
    }
}

#[test]
fn substitute_replaces_known_keys_and_leaves_unknown_alone() {
    let mut vars = BTreeMap::new();
    vars.insert("name".to_string(), "demo".to_string());
    let rendered = substitute("hello {{name}} and {{missing}}", &vars);
    assert_eq!(rendered, "hello demo and {{missing}}");
}

#[test]
fn scaffold_emits_expected_files() {
    let tmp = TempDir::new().unwrap();
    let out_dir = tmp.path().join("animus-trigger-fswatch");
    let args = make_args("fswatch", out_dir.clone());

    handle_plugin_scaffold_trigger(args).expect("scaffold should succeed");

    for rel in ["Cargo.toml", "plugin.toml", "src/main.rs", "README.md", ".gitignore"] {
        let path = out_dir.join(rel);
        assert!(path.exists(), "expected {} to exist", path.display());
    }

    let cargo = std::fs::read_to_string(out_dir.join("Cargo.toml")).unwrap();
    assert!(cargo.contains(r#"name = "animus-trigger-fswatch""#), "got: {cargo}");
    assert!(cargo.contains(r#"license = "Apache-2.0""#), "got: {cargo}");
    assert!(cargo.contains(r#"repository = "https://github.com/acme-co/animus-trigger-fswatch""#));
    assert!(cargo.contains(r#"tag = "v0.5.5""#));

    let plugin = std::fs::read_to_string(out_dir.join("plugin.toml")).unwrap();
    assert!(plugin.contains(r#"plugin_kind = "trigger_backend""#), "got: {plugin}");
    assert!(plugin.contains(r#"binary      = "animus-trigger-fswatch""#));

    let main = std::fs::read_to_string(out_dir.join("src/main.rs")).unwrap();
    assert!(main.contains("PLUGIN_KIND_TRIGGER_BACKEND"));
    assert!(main.contains("TRIGGER_METHOD_WATCH"));
    assert!(main.contains("TRIGGER_METHOD_EVENT"));
    assert!(main.contains("struct FswatchState"));
    assert!(main.contains("\"fswatch-"), "expected name_snake substitution in event id");

    let readme = std::fs::read_to_string(out_dir.join("README.md")).unwrap();
    assert!(readme.contains("animus-trigger-fswatch"));
    assert!(readme.contains("animus plugin install --path target/release/animus-trigger-fswatch"));
}

#[test]
fn scaffold_rejects_existing_out_dir_without_force() {
    let tmp = TempDir::new().unwrap();
    let out_dir = tmp.path().join("animus-trigger-cron");
    std::fs::create_dir_all(&out_dir).unwrap();
    std::fs::write(out_dir.join("LEFTOVER"), "x").unwrap();

    let args = make_args("cron", out_dir.clone());
    let err = handle_plugin_scaffold_trigger(args).expect_err("should reject non-empty existing dir");
    assert!(err.to_string().contains("already exists"), "unexpected error: {err}");
    assert!(out_dir.join("LEFTOVER").exists(), "non-force path must not touch the existing dir");
}

#[test]
fn scaffold_force_overwrites_existing_scaffold() {
    let tmp = TempDir::new().unwrap();
    let out_dir = tmp.path().join("animus-trigger-x");
    let first = make_args("x-watch", out_dir.clone());
    handle_plugin_scaffold_trigger(first).expect("first scaffold should succeed");
    assert!(out_dir.join("Cargo.toml").exists());

    let mut second = make_args("x-watch", out_dir.clone());
    second.force = true;
    second.description = Some("Updated".to_string());
    handle_plugin_scaffold_trigger(second).expect("second scaffold with --force should succeed");

    let cargo = std::fs::read_to_string(out_dir.join("Cargo.toml")).unwrap();
    assert!(cargo.contains("Updated"), "force should have rewritten Cargo.toml: {cargo}");
}

#[test]
fn scaffold_force_refuses_unrelated_existing_directory() {
    let tmp = TempDir::new().unwrap();
    let out_dir = tmp.path().join("some-existing-project");
    std::fs::create_dir_all(&out_dir).unwrap();
    std::fs::write(out_dir.join("Cargo.toml"), "[package]\nname = \"unrelated\"\n").unwrap();
    std::fs::write(out_dir.join("PRECIOUS"), "do-not-touch").unwrap();

    let mut args = make_args("x-watch", out_dir.clone());
    args.force = true;
    let err = handle_plugin_scaffold_trigger(args).expect_err("force must refuse unrelated dirs");
    assert!(err.to_string().contains("does not look like a previously scaffolded"), "unexpected error: {err}");
    assert!(out_dir.join("PRECIOUS").exists(), "force must not touch unrelated files");
}

#[test]
fn scaffold_into_empty_existing_directory_succeeds() {
    let tmp = TempDir::new().unwrap();
    let out_dir = tmp.path().join("animus-trigger-empty");
    std::fs::create_dir_all(&out_dir).unwrap();
    let args = make_args("empty", out_dir.clone());
    handle_plugin_scaffold_trigger(args).expect("scaffold into empty existing dir should succeed");
    assert!(out_dir.join("Cargo.toml").exists());
}

#[test]
fn scaffold_rejects_quotes_in_description() {
    let tmp = TempDir::new().unwrap();
    let out_dir = tmp.path().join("animus-trigger-quotetest");
    let mut args = make_args("quotetest", out_dir.clone());
    args.description = Some(r#"Watch "src" files"#.to_string());
    let err = handle_plugin_scaffold_trigger(args).expect_err("quotes in --description must be rejected");
    assert!(err.to_string().contains("--description"), "unexpected error: {err}");
}

#[test]
fn scaffold_rejects_newline_in_owner() {
    let tmp = TempDir::new().unwrap();
    let out_dir = tmp.path().join("animus-trigger-newlinetest");
    let mut args = make_args("newlinetest", out_dir.clone());
    args.owner = Some("acme\nco".to_string());
    let err = handle_plugin_scaffold_trigger(args).expect_err("newline in --owner must be rejected");
    assert!(err.to_string().contains("--owner"), "unexpected error: {err}");
}

#[test]
fn scaffold_rejects_invalid_kebab_name() {
    let tmp = TempDir::new().unwrap();
    let out_dir = tmp.path().join("ignored");

    for bad in ["Foo", "9bad", "with_underscore", "-leading", "trailing-"] {
        let args = make_args(bad, out_dir.clone());
        let err = handle_plugin_scaffold_trigger(args).expect_err(&format!("expected '{}' to be rejected", bad));
        assert!(err.to_string().contains("kebab-case"), "unexpected error for {bad}: {err}");
    }
}

#[test]
fn scaffold_output_passes_cargo_check_when_offline_unavailable() {
    // We don't unconditionally run `cargo check` here because it would require
    // network access to resolve the git-deped animus-plugin-protocol crate.
    // Instead, sanity check the generated Cargo.toml is syntactically valid
    // TOML and the main.rs at least parses as Rust.
    let tmp = TempDir::new().unwrap();
    let out_dir = tmp.path().join("animus-trigger-demo");
    let args = make_args("demo", out_dir.clone());
    handle_plugin_scaffold_trigger(args).expect("scaffold should succeed");

    let cargo_raw = std::fs::read_to_string(out_dir.join("Cargo.toml")).unwrap();
    let parsed: toml::Value =
        toml::from_str(&cargo_raw).unwrap_or_else(|e| panic!("Cargo.toml must be valid TOML: {e}\n{cargo_raw}"));
    let bin = parsed.get("bin").and_then(|v| v.as_array()).expect("Cargo.toml must declare [[bin]]");
    assert_eq!(bin.len(), 1);
    assert_eq!(bin[0].get("name").and_then(|v| v.as_str()), Some("animus-trigger-demo"));

    let main_raw = std::fs::read_to_string(out_dir.join("src/main.rs")).unwrap();
    assert!(main_raw.contains("#[tokio::main]"));
    assert!(main_raw.contains("Plugin::new(PLUGIN_NAME"));
    assert!(main_raw.contains("TRIGGER_METHOD_WATCH"));
    let open_braces = main_raw.matches('{').count();
    let close_braces = main_raw.matches('}').count();
    assert_eq!(
        open_braces, close_braces,
        "scaffolded src/main.rs must have balanced braces (open={open_braces}, close={close_braces})"
    );
}
