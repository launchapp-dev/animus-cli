use std::error::Error;
use std::fmt::{Display, Formatter};

use protocol::classify_anyhow_error_kind;
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct WebApiError {
    pub code: String,
    pub message: String,
    pub exit_code: i32,
}

impl WebApiError {
    pub fn new(code: impl Into<String>, message: impl Into<String>, exit_code: i32) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            exit_code,
        }
    }
}

impl Display for WebApiError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl Error for WebApiError {}

impl From<anyhow::Error> for WebApiError {
    fn from(value: anyhow::Error) -> Self {
        let kind = classify_anyhow_error_kind(&value);
        Self::new(kind.code(), value.to_string(), kind.exit_code())
    }
}
