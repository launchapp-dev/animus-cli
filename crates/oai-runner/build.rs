use std::process::Command;

fn main() {
    let git_hash = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .and_then(|output| if output.status.success() { String::from_utf8(output.stdout).ok() } else { None })
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "unknown".to_string());

    println!("cargo:rustc-env=GIT_HASH={}", git_hash);

    let git_dir = Command::new("git")
        .args(["rev-parse", "--git-dir"])
        .output()
        .ok()
        .and_then(|output| if output.status.success() { String::from_utf8(output.stdout).ok() } else { None })
        .map(|s| s.trim().to_string());

    if let Some(git_dir) = git_dir {
        let git_dir = std::path::Path::new(&git_dir);
        println!("cargo:rerun-if-changed={}", git_dir.join("HEAD").display());
        println!("cargo:rerun-if-changed={}", git_dir.join("packed-refs").display());
        if let Ok(head) = std::fs::read_to_string(git_dir.join("HEAD")) {
            if let Some(branch_ref) = head.strip_prefix("ref: ") {
                println!("cargo:rerun-if-changed={}", git_dir.join(branch_ref.trim()).display());
            }
        }
    }
}
