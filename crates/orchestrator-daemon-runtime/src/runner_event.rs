#[derive(Debug, Clone, serde::Deserialize)]
pub struct RunnerEvent {
    pub event: String,
    #[serde(default)]
    pub task_id: String,
    #[serde(default, alias = "pipeline")]
    pub workflow_ref: Option<String>,
    #[serde(default)]
    pub exit_code: Option<i32>,
}
