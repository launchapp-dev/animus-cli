use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MergeConflictContext {
    pub source_branch: String,
    pub target_branch: String,
    pub merge_worktree_path: String,
    pub conflicted_files: Vec<String>,
    pub merge_queue_branch: String,
    pub push_remote: String,
}
