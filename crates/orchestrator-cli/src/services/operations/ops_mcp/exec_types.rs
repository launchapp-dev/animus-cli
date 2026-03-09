use super::*;

#[derive(Debug, Clone, Serialize)]
pub(super) struct CliExecutionResult {
    pub(super) command: String,
    pub(super) args: Vec<String>,
    pub(super) requested_args: Vec<String>,
    pub(super) project_root: String,
    pub(super) exit_code: i32,
    pub(super) success: bool,
    pub(super) stdout: String,
    pub(super) stderr: String,
    pub(super) stdout_json: Option<Value>,
    pub(super) stderr_json: Option<Value>,
}
