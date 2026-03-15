use std::process::Command;

fn bin() -> &'static std::path::Path {
    assert_cmd::cargo::cargo_bin!("ao-oai-runner")
}

#[test]
fn version_flag_exits_successfully() {
    let status = Command::new(bin()).arg("--version").status().unwrap();
    assert!(status.success(), "ao-oai-runner --version should exit 0");
}

#[test]
fn version_output_contains_package_version() {
    let output = Command::new(bin()).arg("--version").output().unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();

    assert!(stdout.contains("ao-oai-runner"), "version output should contain binary name, got: {stdout}");
    assert!(
        stdout.contains(env!("CARGO_PKG_VERSION")),
        "version output should contain package version {}, got: {stdout}",
        env!("CARGO_PKG_VERSION")
    );
}

#[test]
fn version_output_contains_git_hash_in_parens() {
    let output = Command::new(bin()).arg("--version").output().unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    let trimmed = stdout.trim();

    assert!(
        trimmed.contains('(') && trimmed.contains(')'),
        "version output should contain git hash in parentheses, got: {trimmed}"
    );

    let hash = trimmed.split('(').nth(1).and_then(|s| s.split(')').next()).unwrap_or("");

    assert!(!hash.is_empty(), "git hash between parentheses should not be empty, got: {trimmed}");

    let valid = hash == "unknown" || hash.chars().all(|c| c.is_ascii_hexdigit());
    assert!(valid, "git hash should be hex digits or 'unknown', got: {hash}");
}

#[test]
fn version_output_format_matches_expected_pattern() {
    let output = Command::new(bin()).arg("--version").output().unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    let trimmed = stdout.trim();

    let pkg_version = env!("CARGO_PKG_VERSION");
    let expected_prefix = format!("ao-oai-runner {pkg_version} (");
    assert!(
        trimmed.starts_with(&expected_prefix),
        "version output should match pattern 'ao-oai-runner X.Y.Z (HASH)', got: {trimmed}"
    );
    assert!(trimmed.ends_with(')'), "version output should end with ')', got: {trimmed}");
}
