use super::*;
use anyhow::{Context, Result};

use super::model::GitRepoRefCli;
use super::store::{
    ensure_force_confirmation, load_git_repo_registry, repos_root, resolve_repo_path, run_git,
    save_git_repo_registry,
};

pub(super) fn handle_git_repo(
    command: GitRepoCommand,
    project_root: &str,
    json: bool,
) -> Result<()> {
    match command {
        GitRepoCommand::List => {
            let mut registry = load_git_repo_registry(project_root)?;
            if run_git(
                Path::new(project_root),
                &["rev-parse", "--is-inside-work-tree"],
            )
            .is_ok()
                && !registry.repos.iter().any(|repo| repo.name == "current")
            {
                registry.repos.insert(
                    0,
                    GitRepoRefCli {
                        name: "current".to_string(),
                        path: project_root.to_string(),
                        url: None,
                    },
                );
            }
            print_value(registry.repos, json)
        }
        GitRepoCommand::Get(args) => {
            let path = resolve_repo_path(project_root, &args.repo)?;
            let branch = run_git(&path, &["rev-parse", "--abbrev-ref", "HEAD"]).ok();
            print_value(
                serde_json::json!({
                    "name": args.repo,
                    "path": path,
                    "branch": branch,
                }),
                json,
            )
        }
        GitRepoCommand::Init(args) => {
            let repo_path = args
                .path
                .map(PathBuf::from)
                .unwrap_or_else(|| repos_root(project_root).join(&args.name));
            if let Some(parent) = repo_path.parent() {
                fs::create_dir_all(parent)?;
            }
            ProcessCommand::new("git")
                .arg("init")
                .arg(&repo_path)
                .output()
                .with_context(|| format!("failed to initialize repo at {}", repo_path.display()))?;
            let mut registry = load_git_repo_registry(project_root)?;
            registry.repos.retain(|repo| repo.name != args.name);
            registry.repos.push(GitRepoRefCli {
                name: args.name,
                path: repo_path.display().to_string(),
                url: None,
            });
            save_git_repo_registry(project_root, &registry)?;
            print_value(registry.repos, json)
        }
        GitRepoCommand::Clone(args) => {
            let repo_path = args
                .path
                .map(PathBuf::from)
                .unwrap_or_else(|| repos_root(project_root).join(&args.name));
            if let Some(parent) = repo_path.parent() {
                fs::create_dir_all(parent)?;
            }
            let output = ProcessCommand::new("git")
                .arg("clone")
                .arg(&args.url)
                .arg(&repo_path)
                .output()
                .with_context(|| {
                    format!("failed to clone {} into {}", args.url, repo_path.display())
                })?;
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
                anyhow::bail!("git clone failed: {stderr}");
            }
            let mut registry = load_git_repo_registry(project_root)?;
            registry.repos.retain(|repo| repo.name != args.name);
            registry.repos.push(GitRepoRefCli {
                name: args.name,
                path: repo_path.display().to_string(),
                url: Some(args.url),
            });
            save_git_repo_registry(project_root, &registry)?;
            print_value(registry.repos, json)
        }
    }
}

pub(super) fn handle_git_branches(args: GitRepoArgs, project_root: &str, json: bool) -> Result<()> {
    let repo_path = resolve_repo_path(project_root, &args.repo)?;
    let output = run_git(&repo_path, &["branch", "--format", "%(refname:short)"])?;
    let branches: Vec<String> = output
        .lines()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.to_string())
        .collect();
    print_value(branches, json)
}

pub(super) fn handle_git_status(args: GitRepoArgs, project_root: &str, json: bool) -> Result<()> {
    let repo_path = resolve_repo_path(project_root, &args.repo)?;
    let output = run_git(&repo_path, &["status", "--porcelain", "-b"])?;
    let mut lines = output.lines();
    let branch = lines.next().unwrap_or_default().trim().to_string();
    let changes: Vec<String> = lines
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.to_string())
        .collect();
    print_value(
        serde_json::json!({
            "repo": args.repo,
            "branch": branch,
            "changes": changes,
            "clean": changes.is_empty(),
        }),
        json,
    )
}

pub(super) fn handle_git_commit(args: GitCommitArgs, project_root: &str, json: bool) -> Result<()> {
    let repo_path = resolve_repo_path(project_root, &args.repo)?;
    let _ = run_git(&repo_path, &["add", "-A"])?;
    let output = run_git(&repo_path, &["commit", "-m", &args.message])?;
    print_value(
        serde_json::json!({
            "repo": args.repo,
            "message": args.message,
            "output": output,
        }),
        json,
    )
}

pub(super) fn handle_git_push(args: GitPushArgs, project_root: &str, json: bool) -> Result<()> {
    let repo_path = resolve_repo_path(project_root, &args.repo)?;
    if args.force {
        ensure_force_confirmation(project_root, args.confirmation_id.as_deref())?;
    }
    let mut cmd = vec!["push", args.remote.as_str(), args.branch.as_str()];
    if args.force {
        cmd.push("--force");
    }
    let output = run_git(&repo_path, &cmd)?;
    print_value(
        serde_json::json!({
            "repo": args.repo,
            "remote": args.remote,
            "branch": args.branch,
            "force": args.force,
            "output": output,
        }),
        json,
    )
}

pub(super) fn handle_git_pull(args: GitPullArgs, project_root: &str, json: bool) -> Result<()> {
    let repo_path = resolve_repo_path(project_root, &args.repo)?;
    let output = run_git(
        &repo_path,
        &["pull", args.remote.as_str(), args.branch.as_str()],
    )?;
    print_value(
        serde_json::json!({
            "repo": args.repo,
            "remote": args.remote,
            "branch": args.branch,
            "output": output,
        }),
        json,
    )
}
