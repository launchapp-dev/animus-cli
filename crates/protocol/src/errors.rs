use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, Serialize, Deserialize, Error)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ProtocolError {
    #[error("Protocol version mismatch: expected {expected}, got {got}")]
    VersionMismatch { expected: String, got: String },
    #[error("Invalid message format: {0}")]
    InvalidMessage(String),
    #[error("Serialization error: {0}")]
    Serialization(String),
}
