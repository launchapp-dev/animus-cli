use super::*;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct GitRepoRefCli {
    pub(super) name: String,
    pub(super) path: String,
    #[serde(default)]
    pub(super) url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub(super) struct GitRepoRegistry {
    #[serde(default)]
    pub(super) repos: Vec<GitRepoRefCli>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct GitWorktreeInfoCli {
    pub(super) worktree_name: String,
    pub(super) path: String,
    #[serde(default)]
    pub(super) head: Option<String>,
    #[serde(default)]
    pub(super) branch: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct GitSyncStatusCli {
    pub(super) worktree_name: String,
    pub(super) clean: bool,
    #[serde(default)]
    pub(super) branch: Option<String>,
    #[serde(default)]
    pub(super) ahead_behind: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct GitConfirmationOutcomeCli {
    pub(super) success: bool,
    pub(super) message: String,
    #[serde(default)]
    pub(super) metadata: Option<Value>,
    pub(super) recorded_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct GitConfirmationRecordCli {
    pub(super) id: String,
    pub(super) operation_type: String,
    pub(super) repo_name: String,
    pub(super) context: Value,
    pub(super) required: bool,
    pub(super) blocked: bool,
    pub(super) reason: String,
    pub(super) created_at: String,
    #[serde(default)]
    pub(super) approved: Option<bool>,
    #[serde(default)]
    pub(super) comment: Option<String>,
    #[serde(default)]
    pub(super) user_id: Option<String>,
    #[serde(default)]
    pub(super) responded_at: Option<String>,
    #[serde(default)]
    pub(super) outcome: Option<GitConfirmationOutcomeCli>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub(super) struct GitConfirmationStoreCli {
    #[serde(default)]
    pub(super) requests: Vec<GitConfirmationRecordCli>,
}
