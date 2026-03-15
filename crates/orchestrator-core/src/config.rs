use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RuntimeConfig {
    pub project_root: Option<String>,
    pub log_dir: Option<String>,
    pub pool_size: Option<usize>,
    pub headless: bool,
    pub runner_endpoint: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectRootSource {
    CliArg,
    GitRepoRoot,
    CurrentDir,
}

pub fn resolve_project_root(config: &RuntimeConfig) -> (String, ProjectRootSource) {
    if let Some(root) = config.project_root.as_deref().map(str::trim).filter(|root| !root.is_empty()) {
        return (normalize_project_root(root), ProjectRootSource::CliArg);
    }

    let cwd = std::env::current_dir().expect("Failed to get current directory");

    if let Some(root) = resolve_git_repo_root(&cwd) {
        return (root, ProjectRootSource::GitRepoRoot);
    }

    (cwd.to_string_lossy().to_string(), ProjectRootSource::CurrentDir)
}

fn normalize_project_root(root: &str) -> String {
    let cwd = std::env::current_dir().expect("Failed to get current directory");
    let candidate = absolutize_path(&cwd, root);

    resolve_git_repo_root(&candidate).unwrap_or_else(|| candidate.to_string_lossy().to_string())
}

fn resolve_git_repo_root(cwd: &Path) -> Option<String> {
    let output = Command::new("git").arg("-C").arg(cwd).args(["rev-parse", "--git-common-dir"]).output().ok()?;
    if !output.status.success() {
        return None;
    }

    let common_dir = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if common_dir.is_empty() {
        return None;
    }

    let common_dir_path = absolutize_path(cwd, common_dir.as_str());
    let canonical_common_dir = common_dir_path.canonicalize().unwrap_or(common_dir_path);
    if canonical_common_dir.file_name()? != ".git" {
        return None;
    }

    let repo_root = canonical_common_dir.parent()?.to_path_buf();
    Some(repo_root.canonicalize().unwrap_or(repo_root).to_string_lossy().to_string())
}

fn absolutize_path(base: &Path, path: &str) -> PathBuf {
    let candidate = PathBuf::from(path);
    if candidate.is_absolute() {
        candidate
    } else {
        base.join(candidate)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::sync::{Mutex, OnceLock};

    struct CurrentDirGuard {
        original: PathBuf,
    }

    impl CurrentDirGuard {
        fn set(cwd: &Path) -> Self {
            let original = std::env::current_dir().expect("current dir should load");
            std::env::set_current_dir(cwd).expect("test cwd should set");
            Self { original }
        }
    }

    impl Drop for CurrentDirGuard {
        fn drop(&mut self) {
            std::env::set_current_dir(&self.original).expect("cwd should restore");
        }
    }

    fn resolver_test_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    fn run_with_test_process_state<T>(cwd: &Path, _project_root: Option<&str>, test: impl FnOnce() -> T) -> T {
        let _guard = resolver_test_lock().lock().expect("project root resolver test lock should acquire");
        let _cwd_guard = CurrentDirGuard::set(cwd);
        test()
    }

    fn run_git(repo_root: &Path, args: &[&str]) -> String {
        let output =
            Command::new("git").arg("-C").arg(repo_root).args(args).output().expect("git command should start");
        assert!(
            output.status.success(),
            "git command failed: git {}\nstdout:\n{}\nstderr:\n{}",
            args.join(" "),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    #[test]
    fn cli_project_root_wins() {
        let temp = tempfile::tempdir().expect("tempdir");
        run_with_test_process_state(temp.path(), None, || {
            let config = RuntimeConfig { project_root: Some("/tmp/custom".to_string()), ..RuntimeConfig::default() };

            let (root, source) = resolve_project_root(&config);
            assert_eq!(root, "/tmp/custom");
            assert_eq!(source, ProjectRootSource::CliArg);
        });
    }

    #[test]
    fn cli_project_root_dot_in_linked_worktree_resolves_primary_repo_root() {
        let temp = tempfile::tempdir().expect("tempdir");
        let repo_root = temp.path().join("repo");
        let worktree_root = temp.path().join("repo-worktree");
        std::fs::create_dir_all(&repo_root).expect("repo root should be created");

        run_git(&repo_root, &["init"]);
        run_git(&repo_root, &["config", "user.email", "ao-tests@example.com"]);
        run_git(&repo_root, &["config", "user.name", "AO Tests"]);
        std::fs::write(repo_root.join("README.md"), "root\n").expect("seed file should write");
        run_git(&repo_root, &["add", "README.md"]);
        run_git(&repo_root, &["commit", "-m", "init"]);
        run_git(&repo_root, &["branch", "feature/cli-dot-root"]);
        run_git(&repo_root, &["worktree", "add", worktree_root.to_string_lossy().as_ref(), "feature/cli-dot-root"]);

        run_with_test_process_state(&worktree_root, None, || {
            let config = RuntimeConfig { project_root: Some(".".to_string()), ..RuntimeConfig::default() };

            let (root, source) = resolve_project_root(&config);
            assert_eq!(PathBuf::from(root), repo_root.canonicalize().expect("repo root should canonicalize"));
            assert_eq!(source, ProjectRootSource::CliArg);
        });
    }

    #[test]
    fn falls_through_to_cwd_when_cli_arg_missing() {
        let temp = tempfile::tempdir().expect("tempdir");
        run_with_test_process_state(temp.path(), None, || {
            let (_, source) = resolve_project_root(&RuntimeConfig::default());
            assert_eq!(source, ProjectRootSource::CurrentDir);
        });
    }

    #[test]
    fn resolves_repo_root_from_git_subdirectory() {
        let temp = tempfile::tempdir().expect("tempdir");
        let repo_root = temp.path().join("repo");
        let subdir = repo_root.join("nested").join("deeper");
        std::fs::create_dir_all(&subdir).expect("subdir should be created");
        run_git(&repo_root, &["init"]);

        run_with_test_process_state(&subdir, None, || {
            let (root, source) = resolve_project_root(&RuntimeConfig::default());
            assert_eq!(PathBuf::from(root), repo_root.canonicalize().expect("repo root should canonicalize"));
            assert_eq!(source, ProjectRootSource::GitRepoRoot);
        });
    }

    #[test]
    fn resolves_primary_repo_root_from_linked_worktree() {
        let temp = tempfile::tempdir().expect("tempdir");
        let repo_root = temp.path().join("repo");
        let worktree_root = temp.path().join("repo-worktree");
        std::fs::create_dir_all(&repo_root).expect("repo root should be created");

        run_git(&repo_root, &["init"]);
        run_git(&repo_root, &["config", "user.email", "ao-tests@example.com"]);
        run_git(&repo_root, &["config", "user.name", "AO Tests"]);
        std::fs::write(repo_root.join("README.md"), "root\n").expect("seed file should write");
        run_git(&repo_root, &["add", "README.md"]);
        run_git(&repo_root, &["commit", "-m", "init"]);
        run_git(&repo_root, &["branch", "feature/worktree-root"]);
        run_git(&repo_root, &["worktree", "add", worktree_root.to_string_lossy().as_ref(), "feature/worktree-root"]);

        run_with_test_process_state(&worktree_root, None, || {
            let (root, source) = resolve_project_root(&RuntimeConfig::default());
            assert_eq!(PathBuf::from(root), repo_root.canonicalize().expect("repo root should canonicalize"));
            assert_eq!(source, ProjectRootSource::GitRepoRoot);
        });
    }

    #[test]
    fn falls_back_to_current_dir_outside_git_repo() {
        let temp = tempfile::tempdir().expect("tempdir");
        let outside = temp.path().join("outside");
        std::fs::create_dir_all(&outside).expect("outside dir should be created");

        run_with_test_process_state(&outside, None, || {
            let (root, source) = resolve_project_root(&RuntimeConfig::default());
            assert_eq!(PathBuf::from(root), outside.canonicalize().expect("outside dir should canonicalize"));
            assert_eq!(source, ProjectRootSource::CurrentDir);
        });
    }
}
