use std::fs;
use std::path::{Path, PathBuf};

const EXACT_PROHIBITED_PACKAGES: &[&str] = &[
    "tauri",
    "tauri-build",
    "wry",
    "tao",
    "gtk",
    "gtk4",
    "webkit2gtk",
    "webview2",
    "webview2-com",
];
const PROHIBITED_PREFIXES: &[&str] = &["tauri-plugin-"];
const DEPENDENCY_SECTIONS: &[&str] = &["dependencies", "dev-dependencies", "build-dependencies"];

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
struct PolicyViolation {
    manifest_path: String,
    section: String,
    dependency_key: String,
    resolved_package: String,
}

#[derive(Debug)]
struct DeclaredDependency {
    section: String,
    dependency_key: String,
    resolved_package: String,
}

#[test]
fn rust_only_dependency_policy() {
    let root = workspace_root();
    let violations = collect_workspace_policy_violations(&root)
        .expect("policy scan should succeed for the current workspace");

    assert!(
        violations.is_empty(),
        "desktop-wrapper dependency policy violated.\n\
         Prohibited packages: {}.\n\
         {}\n{}",
        prohibited_package_summary(),
        format_policy_violations(&violations),
        "Remediation: remove prohibited dependencies from workspace crate manifests."
    );
}

#[test]
fn rust_only_dependency_policy_detects_renamed_and_target_specific_dependencies() {
    let fixture_root = tempfile::tempdir().expect("tempdir should create");
    write_file(
        &fixture_root.path().join("Cargo.toml"),
        r#"
[workspace]
members = ["crates/app"]
resolver = "2"
"#,
    );
    write_file(
        &fixture_root.path().join("crates/app/Cargo.toml"),
        r#"
[package]
name = "app"
version = "0.1.0"
edition = "2021"

[dependencies]
ui-shell = { version = "2", package = "tauri" }

[target.'cfg(target_os = "linux")'.build-dependencies]
native-webkit = { version = "0.1", package = "webkit2gtk" }
"#,
    );

    let violations = collect_workspace_policy_violations(fixture_root.path())
        .expect("policy scan should succeed for fixture");

    assert_eq!(violations.len(), 2);
    assert!(violations.iter().any(|violation| {
        violation.section == "dependencies"
            && violation.dependency_key == "ui-shell"
            && violation.resolved_package == "tauri"
    }));
    assert!(violations.iter().any(|violation| {
        violation.section == "target.cfg(target_os = \"linux\").build-dependencies"
            && violation.dependency_key == "native-webkit"
            && violation.resolved_package == "webkit2gtk"
    }));
}

#[test]
fn rust_only_dependency_policy_violation_output_is_sorted_deterministically() {
    let fixture_root = tempfile::tempdir().expect("tempdir should create");
    write_file(
        &fixture_root.path().join("Cargo.toml"),
        r#"
[workspace]
members = ["crates/zeta", "crates/alpha"]
resolver = "2"
"#,
    );
    write_file(
        &fixture_root.path().join("crates/zeta/Cargo.toml"),
        r#"
[package]
name = "zeta"
version = "0.1.0"
edition = "2021"

[dependencies]
tauri = "2"
"#,
    );
    write_file(
        &fixture_root.path().join("crates/alpha/Cargo.toml"),
        r#"
[package]
name = "alpha"
version = "0.1.0"
edition = "2021"

[dev-dependencies]
tauri-dev = { version = "2", package = "tauri-build" }
"#,
    );

    let violations = collect_workspace_policy_violations(fixture_root.path())
        .expect("policy scan should succeed for fixture");
    let rendered = format_policy_violations(&violations);

    let rendered_lines = rendered.lines().collect::<Vec<_>>();
    let mut sorted_lines = rendered_lines.clone();
    sorted_lines.sort();

    assert_eq!(rendered_lines, sorted_lines);
    assert_eq!(violations.len(), 2);
}

fn workspace_root() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .ancestors()
        .nth(2)
        .expect("workspace root should exist")
        .to_path_buf()
}

fn collect_workspace_policy_violations(
    workspace_root: &Path,
) -> Result<Vec<PolicyViolation>, String> {
    let manifest_paths = workspace_member_manifest_paths(workspace_root)?;
    let mut violations = Vec::new();

    for manifest_path in manifest_paths {
        let dependencies = parse_manifest_dependencies(&manifest_path)?;
        let manifest_path_display = manifest_path
            .strip_prefix(workspace_root)
            .unwrap_or(&manifest_path)
            .display()
            .to_string();

        for dependency in dependencies {
            if is_prohibited_package(&dependency.resolved_package) {
                violations.push(PolicyViolation {
                    manifest_path: manifest_path_display.clone(),
                    section: dependency.section,
                    dependency_key: dependency.dependency_key,
                    resolved_package: dependency.resolved_package,
                });
            }
        }
    }

    violations.sort();
    Ok(violations)
}

fn workspace_member_manifest_paths(workspace_root: &Path) -> Result<Vec<PathBuf>, String> {
    let workspace_manifest_path = workspace_root.join("Cargo.toml");
    let workspace_manifest = parse_toml(&workspace_manifest_path)?;
    let workspace = workspace_manifest
        .get("workspace")
        .and_then(toml::Value::as_table)
        .ok_or_else(|| "workspace root Cargo.toml is missing [workspace] table".to_string())?;
    let members = workspace
        .get("members")
        .and_then(toml::Value::as_array)
        .ok_or_else(|| "workspace root Cargo.toml is missing workspace.members".to_string())?;

    let mut manifest_paths = Vec::new();
    for member in members {
        let member = member
            .as_str()
            .ok_or_else(|| "workspace.members entry must be a string".to_string())?;

        if let Some(prefix) = member.strip_suffix("/*") {
            let directory = workspace_root.join(prefix);
            let entries = fs::read_dir(&directory).map_err(|error| {
                format!("failed to read directory {}: {error}", directory.display())
            })?;
            for entry in entries {
                let entry =
                    entry.map_err(|error| format!("failed to read directory entry: {error}"))?;
                let manifest = entry.path().join("Cargo.toml");
                if manifest.is_file() {
                    manifest_paths.push(manifest);
                }
            }
            continue;
        }

        let manifest_path = workspace_root.join(member).join("Cargo.toml");
        if !manifest_path.is_file() {
            return Err(format!(
                "workspace member manifest does not exist: {}",
                manifest_path.display()
            ));
        }
        manifest_paths.push(manifest_path);
    }

    manifest_paths.sort();
    manifest_paths.dedup();
    Ok(manifest_paths)
}

fn parse_manifest_dependencies(manifest_path: &Path) -> Result<Vec<DeclaredDependency>, String> {
    let manifest = parse_toml(manifest_path)?;
    let mut dependencies = Vec::new();

    for section in DEPENDENCY_SECTIONS {
        collect_dependency_entries(
            manifest.get(*section),
            (*section).to_string(),
            &mut dependencies,
        );
    }

    if let Some(target_table) = manifest.get("target").and_then(toml::Value::as_table) {
        for (target_name, target_value) in target_table {
            if let Some(target_config) = target_value.as_table() {
                for section in DEPENDENCY_SECTIONS {
                    let target_section = format!("target.{target_name}.{section}");
                    collect_dependency_entries(
                        target_config.get(*section),
                        target_section,
                        &mut dependencies,
                    );
                }
            }
        }
    }

    Ok(dependencies)
}

fn collect_dependency_entries(
    table_value: Option<&toml::Value>,
    section: String,
    output: &mut Vec<DeclaredDependency>,
) {
    let Some(table) = table_value.and_then(toml::Value::as_table) else {
        return;
    };

    for (dependency_key, dependency_value) in table {
        output.push(DeclaredDependency {
            section: section.clone(),
            dependency_key: dependency_key.clone(),
            resolved_package: resolve_package_name(dependency_key, dependency_value),
        });
    }
}

fn resolve_package_name(dependency_key: &str, dependency_value: &toml::Value) -> String {
    let Some(table) = dependency_value.as_table() else {
        return dependency_key.to_string();
    };

    table
        .get("package")
        .and_then(toml::Value::as_str)
        .unwrap_or(dependency_key)
        .to_string()
}

fn is_prohibited_package(package_name: &str) -> bool {
    let package_name = package_name.to_ascii_lowercase();
    EXACT_PROHIBITED_PACKAGES.contains(&package_name.as_str())
        || PROHIBITED_PREFIXES
            .iter()
            .any(|prefix| package_name.starts_with(prefix))
}

fn prohibited_package_summary() -> String {
    let mut prohibited = EXACT_PROHIBITED_PACKAGES
        .iter()
        .map(|value| (*value).to_string())
        .collect::<Vec<_>>();
    prohibited.extend(PROHIBITED_PREFIXES.iter().map(|value| format!("{value}*")));
    prohibited.join(", ")
}

fn format_policy_violations(violations: &[PolicyViolation]) -> String {
    if violations.is_empty() {
        return "No violations found.".to_string();
    }

    violations
        .iter()
        .map(|violation| {
            format!(
                "- {} [{}] key=`{}` resolved_package=`{}`",
                violation.manifest_path,
                violation.section,
                violation.dependency_key,
                violation.resolved_package
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn parse_toml(path: &Path) -> Result<toml::Value, String> {
    let content = fs::read_to_string(path)
        .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
    toml::from_str::<toml::Value>(&content)
        .map_err(|error| format!("failed to parse {}: {error}", path.display()))
}

fn write_file(path: &Path, content: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("parent directory should be creatable");
    }
    fs::write(path, content).expect("file should be writable");
}
