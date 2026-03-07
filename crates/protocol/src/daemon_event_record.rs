use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonEventRecord {
    pub schema: String,
    pub id: String,
    #[serde(default)]
    pub seq: u64,
    pub timestamp: String,
    pub event_type: String,
    #[serde(default)]
    pub project_root: Option<String>,
    pub data: Value,
}
