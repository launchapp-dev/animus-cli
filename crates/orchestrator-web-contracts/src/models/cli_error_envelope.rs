use serde::Serialize;

use crate::models::CliErrorBody;

#[derive(Debug, Clone, Serialize)]
pub struct CliErrorEnvelope {
    pub schema: &'static str,
    pub ok: bool,
    pub error: CliErrorBody,
}
