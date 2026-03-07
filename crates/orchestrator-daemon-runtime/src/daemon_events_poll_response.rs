use protocol::DaemonEventRecord;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonEventsPollResponse {
    pub schema: String,
    pub events_path: String,
    pub count: usize,
    pub events: Vec<DaemonEventRecord>,
}
