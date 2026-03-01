use std::process::Command;

fn main() {
    // Get the git short hash
    let git_hash = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .and_then(|output| {
            if output.status.success() {
                String::from_utf8(output.stdout).ok()
            } else {
                None
            }
        })
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "unknown".to_string());

    // Set the GIT_HASH environment variable for use in the main crate
    println!("cargo:rustc-env=GIT_HASH={}", git_hash);

    // Re-run build.rs if git HEAD changes
    println!("cargo:rerun-if-changed=.git/HEAD");
}
