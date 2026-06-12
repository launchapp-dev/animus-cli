use super::*;
use anyhow::Result;

use super::model::GitRepoRefCli;
use super::store::{load_git_repo_registry, run_git};

pub(super) fn handle_git_repo(command: GitRepoCommand, project_root: &str, json: bool) -> Result<()> {
    match command {
        GitRepoCommand::List => {
            let mut registry = load_git_repo_registry(project_root)?;
            if run_git(Path::new(project_root), &["rev-parse", "--is-inside-work-tree"]).is_ok()
                && !registry.repos.iter().any(|repo| repo.name == "current")
            {
                registry.repos.insert(
                    0,
                    GitRepoRefCli { name: "current".to_string(), path: project_root.to_string(), url: None },
                );
            }
            print_value(registry.repos, json)
        }
    }
}
