use super::*;

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, Default)]
pub(super) struct ProjectRootInput {
    #[serde(default)]
    pub(super) project_root: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub(super) struct RunIdInput {
    pub(super) run_id: String,
    #[serde(default)]
    pub(super) project_root: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub(super) struct ExecutionIdInput {
    pub(super) execution_id: String,
    #[serde(default)]
    pub(super) project_root: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub(super) struct RunnerOrphansCleanupInput {
    pub(super) run_id: Vec<String>,
    #[serde(default)]
    pub(super) project_root: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(super) enum OnError {
    #[default]
    Stop,
    Continue,
}

impl OnError {
    pub(super) fn as_str(&self) -> &'static str {
        match self {
            Self::Stop => "stop",
            Self::Continue => "continue",
        }
    }
}

pub(super) struct BatchItemExec {
    pub(super) target_id: String,
    pub(super) command: String,
    pub(super) args: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub(super) struct IdInput {
    pub(super) id: String,
    #[serde(default)]
    pub(super) project_root: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub(super) struct IdListInput {
    pub(super) id: String,
    #[serde(default)]
    pub(super) project_root: Option<String>,
    #[serde(default)]
    pub(super) limit: Option<usize>,
    #[serde(default)]
    pub(super) offset: Option<usize>,
    #[serde(default)]
    pub(super) max_tokens: Option<usize>,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct ListGuardInput {
    pub(super) limit: Option<usize>,
    pub(super) offset: Option<usize>,
    pub(super) max_tokens: Option<usize>,
}
