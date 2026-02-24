use super::*;
use crate::cli_types::{
    GitCommand, GitCommitArgs, GitConfirmCommand, GitPullArgs, GitPushArgs, GitRepoArgs,
    GitRepoCommand, GitWorktreeCommand,
};
use crate::print_value;
use anyhow::Result;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;
use uuid::Uuid;

mod confirm;
mod model;
mod repo;
mod store;
mod worktree;

pub(crate) async fn handle_git(command: GitCommand, project_root: &str, json: bool) -> Result<()> {
    match command {
        GitCommand::Repo { command } => repo::handle_git_repo(command, project_root, json),
        GitCommand::Branches(args) => repo::handle_git_branches(args, project_root, json),
        GitCommand::Status(args) => repo::handle_git_status(args, project_root, json),
        GitCommand::Commit(args) => repo::handle_git_commit(args, project_root, json),
        GitCommand::Push(args) => repo::handle_git_push(args, project_root, json),
        GitCommand::Pull(args) => repo::handle_git_pull(args, project_root, json),
        GitCommand::Worktree { command } => {
            worktree::handle_git_worktree(command, project_root, json)
        }
        GitCommand::Confirm { command } => confirm::handle_git_confirm(command, project_root, json),
    }
}
