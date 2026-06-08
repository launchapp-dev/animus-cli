use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{OnceLock, RwLock};

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
        // Key relative `--project-root` arguments by (arg, cwd) so a
        // long-lived process that changes cwd or resolves multiple repos
        // doesn't reuse the first repo's resolution for the second. (codex
        // round-1 P2.)
        let arg_path = PathBuf::from(root);
        let key = if arg_path.is_absolute() {
            CacheKey::CliArg { arg: root.to_string(), base: None }
        } else {
            let cwd = std::env::current_dir().expect("Failed to get current directory");
            CacheKey::CliArg { arg: root.to_string(), base: Some(cwd) }
        };
        if let Some(cached) = project_root_cache_get(&key) {
            return (cached.value, ProjectRootSource::CliArg);
        }
        let resolved = normalize_project_root(root);
        project_root_cache_put(key, CachedRoot { value: resolved.clone(), source: ProjectRootSource::CliArg });
        return (resolved, ProjectRootSource::CliArg);
    }

    let cwd = std::env::current_dir().expect("Failed to get current directory");
    let key = CacheKey::Cwd(cwd.clone());

    if let Some(cached) = project_root_cache_get(&key) {
        return (cached.value, cached.source);
    }

    if let Some(root) = resolve_git_repo_root(&cwd) {
        project_root_cache_put(key, CachedRoot { value: root.clone(), source: ProjectRootSource::GitRepoRoot });
        return (root, ProjectRootSource::GitRepoRoot);
    }

    let fallback = cwd.to_string_lossy().to_string();
    project_root_cache_put(key, CachedRoot { value: fallback.clone(), source: ProjectRootSource::CurrentDir });
    (fallback, ProjectRootSource::CurrentDir)
}

#[derive(Hash, Eq, PartialEq, Clone)]
enum CacheKey {
    /// `CliArg { arg, base }`. `base` is `Some(cwd)` for relative `--project-root`
    /// arguments so the same relative string resolved from different cwds is
    /// cached independently. Absolute args use `None`.
    CliArg {
        arg: String,
        base: Option<PathBuf>,
    },
    Cwd(PathBuf),
}

#[derive(Clone)]
struct CachedRoot {
    value: String,
    source: ProjectRootSource,
}

fn cache_disabled() -> bool {
    std::env::var_os("ANIMUS_DISABLE_PROJECT_ROOT_CACHE").is_some()
}

fn project_root_cache() -> &'static RwLock<HashMap<CacheKey, CachedRoot>> {
    static CACHE: OnceLock<RwLock<HashMap<CacheKey, CachedRoot>>> = OnceLock::new();
    CACHE.get_or_init(|| RwLock::new(HashMap::new()))
}

fn project_root_cache_get(key: &CacheKey) -> Option<CachedRoot> {
    if cache_disabled() {
        return None;
    }
    project_root_cache().read().ok()?.get(key).cloned()
}

fn project_root_cache_put(key: CacheKey, value: CachedRoot) {
    if cache_disabled() {
        return;
    }
    if let Ok(mut guard) = project_root_cache().write() {
        guard.insert(key, value);
    }
}

#[doc(hidden)]
pub fn clear_project_root_cache_for_test() {
    if let Ok(mut guard) = project_root_cache().write() {
        guard.clear();
    }
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
        clear_project_root_cache_for_test();
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
        run_git(&repo_root, &["config", "user.name", "Animus Tests"]);
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
        run_git(&repo_root, &["config", "user.name", "Animus Tests"]);
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

    #[test]
    fn repeat_calls_hit_in_process_cache() {
        let temp = tempfile::tempdir().expect("tempdir");
        let repo_root = temp.path().join("repo");
        let subdir = repo_root.join("nested");
        std::fs::create_dir_all(&subdir).expect("subdir should be created");
        run_git(&repo_root, &["init"]);

        run_with_test_process_state(&subdir, None, || {
            let (first_root, first_source) = resolve_project_root(&RuntimeConfig::default());
            let (second_root, second_source) = resolve_project_root(&RuntimeConfig::default());
            assert_eq!(first_root, second_root);
            assert_eq!(first_source, ProjectRootSource::GitRepoRoot);
            assert_eq!(second_source, ProjectRootSource::GitRepoRoot);
        });
    }

    #[test]
    fn cached_cwd_lookup_preserves_git_repo_source() {
        // When cwd == repo root, the cached string equals cwd; the cache
        // must still report `GitRepoRoot` on the warm read, not flip to
        // `CurrentDir` based on string equality. (codex round-1 P3.)
        let temp = tempfile::tempdir().expect("tempdir");
        let repo_root = temp.path().join("repo");
        std::fs::create_dir_all(&repo_root).expect("repo root should be created");
        run_git(&repo_root, &["init"]);

        run_with_test_process_state(&repo_root, None, || {
            let (_, source_first) = resolve_project_root(&RuntimeConfig::default());
            let (_, source_second) = resolve_project_root(&RuntimeConfig::default());
            assert_eq!(source_first, ProjectRootSource::GitRepoRoot);
            assert_eq!(source_second, ProjectRootSource::GitRepoRoot);
        });
    }

    #[test]
    fn relative_cli_arg_keys_include_cwd() {
        // `--project-root .` resolved from two different cwds must not
        // collide in the cache. (codex round-1 P2.)
        let temp = tempfile::tempdir().expect("tempdir");
        let repo_a = temp.path().join("repo_a");
        let repo_b = temp.path().join("repo_b");
        std::fs::create_dir_all(&repo_a).expect("repo_a should be created");
        std::fs::create_dir_all(&repo_b).expect("repo_b should be created");
        run_git(&repo_a, &["init"]);
        run_git(&repo_b, &["init"]);

        let _guard = resolver_test_lock().lock().expect("project root resolver test lock should acquire");
        clear_project_root_cache_for_test();

        let original = std::env::current_dir().expect("cwd");

        std::env::set_current_dir(&repo_a).expect("cwd repo_a");
        let config = RuntimeConfig { project_root: Some(".".to_string()), ..RuntimeConfig::default() };
        let (root_a, _) = resolve_project_root(&config);

        std::env::set_current_dir(&repo_b).expect("cwd repo_b");
        let (root_b, _) = resolve_project_root(&config);

        std::env::set_current_dir(&original).expect("restore cwd");

        assert_eq!(
            PathBuf::from(&root_a),
            repo_a.canonicalize().expect("repo_a canonicalize"),
            "repo_a resolution must reflect repo_a cwd"
        );
        assert_eq!(
            PathBuf::from(&root_b),
            repo_b.canonicalize().expect("repo_b canonicalize"),
            "repo_b resolution must reflect repo_b cwd — cache must not collide on the same `.` argument"
        );
        assert_ne!(root_a, root_b);
    }

    #[test]
    fn cache_kill_switch_disables_caching() {
        let temp = tempfile::tempdir().expect("tempdir");
        let outside = temp.path().join("outside");
        std::fs::create_dir_all(&outside).expect("outside dir should be created");

        // Hold the resolver lock so the env mutation below is serialized
        // against other resolver tests that also touch process state.
        let _guard = resolver_test_lock().lock().expect("project root resolver test lock should acquire");
        let _cwd_guard = CurrentDirGuard::set(&outside);
        clear_project_root_cache_for_test();

        std::env::set_var("ANIMUS_DISABLE_PROJECT_ROOT_CACHE", "1");
        let (_, source_first) = resolve_project_root(&RuntimeConfig::default());
        let (_, source_second) = resolve_project_root(&RuntimeConfig::default());
        std::env::remove_var("ANIMUS_DISABLE_PROJECT_ROOT_CACHE");

        // Both calls still produce the same correct source; we just don't
        // populate the cache, so this is observable only through performance.
        assert_eq!(source_first, ProjectRootSource::CurrentDir);
        assert_eq!(source_second, ProjectRootSource::CurrentDir);
        // Cache must remain empty after kill-switch use.
        assert!(project_root_cache().read().expect("cache lock").is_empty());
    }
}
