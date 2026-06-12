use super::*;
use crate::cli_types::{ApprovalCommand, GitCommand, GitRepoCommand, GitWorktreeCommand};
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

pub(crate) use confirm::handle_approval;

pub(crate) async fn handle_git(command: GitCommand, project_root: &str, json: bool) -> Result<()> {
    match command {
        GitCommand::Repo { command } => repo::handle_git_repo(command, project_root, json),
        GitCommand::Worktree { command } => worktree::handle_git_worktree(command, project_root, json).await,
    }
}
