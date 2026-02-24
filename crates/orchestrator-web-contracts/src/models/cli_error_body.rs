use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct CliErrorBody {
    pub code: String,
    pub message: String,
    pub exit_code: i32,
}
